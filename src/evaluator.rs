use std::sync::Arc;

use crate::ast::{Arg, Expr, ForBlock, IfBlock, IncludeDirective, Node};
use crate::error::MdsError;
use crate::scope::{FunctionDef, Scope};
use crate::value::Value;

/// Maximum call depth to prevent stack overflow from deeply nested calls.
const MAX_CALL_DEPTH: usize = 128;

/// Maximum number of iterations allowed in a single @for loop.
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Maximum total iterations across all loops in a single compilation.
/// This prevents nested loops from multiplying iterations into the billions.
const MAX_TOTAL_ITERATIONS: usize = 1_000_000;

/// Maximum size of the output string in bytes (50 MB).
const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum number of accumulated warnings before further warnings are silently dropped.
const MAX_WARNINGS: usize = 1_000;

/// Transient state threaded through the evaluator for a single compilation.
///
/// Bundles the three mutable parameters that were previously threaded individually
/// through every function signature, reducing arity and making call sites clearer.
pub(crate) struct EvalContext<'a> {
    /// LIFO stack of active function call keys, used to detect direct recursion.
    /// Vec is used for O(n) contains at MAX_CALL_DEPTH=128 — acceptable.
    call_stack: Vec<String>,
    /// Cumulative loop iterations across all @for loops in one compilation.
    total_iterations: usize,
    /// Accumulated warnings (e.g. empty @include).
    warnings: &'a mut Vec<String>,
}

/// Evaluate a module body into a final rendered string.
///
/// Warnings (e.g. empty `@include`) are appended to `warnings`.
pub fn evaluate(
    nodes: &[Node],
    scope: &mut Scope,
    warnings: &mut Vec<String>,
) -> Result<String, MdsError> {
    let mut ctx = EvalContext {
        call_stack: Vec::new(),
        total_iterations: 0,
        warnings,
    };
    evaluate_nodes(nodes, scope, &mut ctx)
}

fn evaluate_nodes(
    nodes: &[Node],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let mut output = String::new();

    for node in nodes {
        match node {
            Node::Text(t) => output.push_str(&t.text),
            Node::EscapedBrace => output.push('{'),
            Node::Interpolation(interp) => {
                output.push_str(&evaluate_expr(&interp.expr, scope, ctx)?);
            }
            Node::If(block) => {
                output.push_str(&evaluate_if(block, scope, ctx)?);
            }
            Node::For(block) => {
                output.push_str(&evaluate_for(block, scope, ctx)?);
            }
            Node::Define(_) => {
                // Handled by resolver with full lexical capture
            }
            Node::Import(_) | Node::Export(_) => {
                // Handled by resolver, skip during evaluation
            }
            Node::Include(inc) => {
                output.push_str(&evaluate_include(inc, scope, ctx)?);
            }
        }
        if output.len() > MAX_OUTPUT_SIZE {
            return Err(MdsError::resource_limit(format!(
                "output exceeds maximum size of {} bytes",
                MAX_OUTPUT_SIZE
            )));
        }
    }

    Ok(output)
}

fn evaluate_expr(
    expr: &Expr,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    match expr {
        Expr::Var(name) => {
            let value = scope
                .get_var(name)
                .ok_or_else(|| MdsError::undefined_var(name))?;
            Ok(value.to_string())
        }
        Expr::Call { name, args } => {
            let resolved_args = resolve_args(args, scope, ctx, 0)?;
            call_function(name, &resolved_args, scope, ctx)
        }
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => {
            let resolved_args = resolve_args(args, scope, ctx, 0)?;
            call_qualified_function(namespace, name, &resolved_args, scope, ctx)
        }
    }
}

fn resolve_args(
    args: &[Arg],
    scope: &mut Scope,
    ctx: &mut EvalContext,
    depth: usize,
) -> Result<Vec<Value>, MdsError> {
    if depth > MAX_CALL_DEPTH {
        return Err(MdsError::recursion(format!(
            "argument expression depth exceeds {MAX_CALL_DEPTH}"
        )));
    }
    args.iter()
        .map(|arg| match arg {
            Arg::StringLiteral(s) => Ok(Value::String(s.clone())),
            Arg::Var(name) => scope
                .get_var(name)
                .cloned()
                .ok_or_else(|| MdsError::undefined_var(name)),
            Arg::Call {
                name,
                args: inner_args,
            } => {
                let resolved = resolve_args(inner_args, scope, ctx, depth + 1)?;
                let result = call_function(name, &resolved, scope, ctx)?;
                Ok(Value::String(result))
            }
        })
        .collect()
}

/// On multi-fault, prefer the first error over the second.
///
/// Used for double-fault scenarios (render error AND an internal pop/LIFO error):
/// the render error carries the actionable source-span diagnostic for the user,
/// while pop/LIFO failures are compiler bugs that surface as secondary errors.
fn prefer_first_error<T>(first: Result<T, MdsError>, second: Result<(), MdsError>) -> Result<T, MdsError> {
    match (first, second) {
        (Err(first_err), _) => Err(first_err),
        (Ok(_), Err(second_err)) => Err(second_err),
        (Ok(val), Ok(())) => Ok(val),
    }
}

fn invoke_function(
    func: &FunctionDef,
    call_key: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    if ctx.call_stack.iter().any(|s| s == call_key) {
        return Err(MdsError::recursion(call_key));
    }
    if ctx.call_stack.len() >= MAX_CALL_DEPTH {
        return Err(MdsError::recursion(format!(
            "{call_key} (call depth exceeds {MAX_CALL_DEPTH})"
        )));
    }
    if args.len() != func.params.len() {
        return Err(MdsError::arity(call_key, func.params.len(), args.len()));
    }
    scope.push();
    // Restore captured lexical scope from definition site so the function body
    // can resolve alias imports, sibling functions, and frontmatter variables
    // from its defining module.
    for (alias, ns) in &func.captured.namespaces {
        scope.set_namespace(alias, ns.clone());
    }
    // captured.functions are owned FunctionDef (not Arc) — wrap in Arc for scope insertion.
    for (name, f) in &func.captured.functions {
        scope.set_function(name, Arc::new(f.clone()));
    }
    // Captured vars are restored before param binding so that params shadow
    // captured vars correctly (params take precedence over closure variables).
    for (name, val) in &func.captured.vars {
        scope.set_var(name, val.clone());
    }
    for (param, value) in func.params.iter().zip(args.iter()) {
        scope.set_var(param, value.clone());
    }
    ctx.call_stack.push(call_key.to_string());
    let result = evaluate_nodes(&func.body, scope, ctx);
    // Safety-critical LIFO invariant: call_stack tracks recursion detection.
    // A mismatched pop would silently corrupt recursion state and allow
    // stack overflows. Return a structured error rather than panicking so
    // callers get a proper diagnostic instead of an opaque abort.
    let popped = ctx.call_stack.pop();
    let lifo_result = if popped.as_deref() == Some(call_key) {
        Ok(())
    } else {
        Err(MdsError::syntax(format!(
            "internal error: call_stack LIFO violated: expected '{call_key}', got {popped:?}"
        )))
    };
    let pop_result = scope.pop();
    // On multi-fault, preserve the render error — it carries the actionable
    // source-span diagnostic for the user. LIFO/pop failures are compiler
    // bugs and surface as secondary errors.
    prefer_first_error(result, lifo_result.and(pop_result))
}

fn call_function(
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let func = scope
        .get_function(name)
        .ok_or_else(|| MdsError::undefined_fn(name))?
        .clone();
    invoke_function(&func, name, args, scope, ctx)
}

fn call_qualified_function(
    namespace: &str,
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let qualified_name = format!("{namespace}.{name}");

    let ns = scope
        .get_namespace(namespace)
        .ok_or_else(|| MdsError::undefined_var(namespace))?;

    let func = ns
        .functions
        .get(name)
        .ok_or_else(|| MdsError::undefined_fn(&qualified_name))?
        .clone();

    invoke_function(&func, &qualified_name, args, scope, ctx)
}

fn evaluate_if(
    block: &IfBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let value = scope
        .get_var(&block.condition)
        .ok_or_else(|| MdsError::undefined_var(&block.condition))?;

    if value.is_truthy() {
        evaluate_nodes(&block.then_body, scope, ctx)
    } else if let Some(else_body) = &block.else_body {
        evaluate_nodes(else_body, scope, ctx)
    } else {
        Ok(String::new())
    }
}

fn evaluate_for(
    block: &ForBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let iterable = scope
        .get_var(&block.iterable)
        .ok_or_else(|| MdsError::undefined_var(&block.iterable))?;

    let items = iterable
        .as_array()
        .ok_or_else(|| MdsError::type_error(iterable.type_name()))?
        .to_vec();

    if items.len() > MAX_LOOP_ITERATIONS {
        return Err(MdsError::resource_limit(format!(
            "array has {} elements, exceeding maximum loop iteration limit of {}",
            items.len(),
            MAX_LOOP_ITERATIONS
        )));
    }

    let mut output = String::new();

    for item in items {
        ctx.total_iterations += 1;
        if ctx.total_iterations > MAX_TOTAL_ITERATIONS {
            return Err(MdsError::resource_limit(format!(
                "total loop iterations exceeded maximum of {} across all loops in this compilation",
                MAX_TOTAL_ITERATIONS
            )));
        }
        scope.push();
        scope.set_var(&block.var, item);
        let rendered = evaluate_nodes(&block.body, scope, ctx);
        let pop_result = scope.pop();
        output.push_str(&prefer_first_error(rendered, pop_result)?);
    }

    Ok(output)
}

fn evaluate_include(
    inc: &IncludeDirective,
    scope: &Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let ns = scope
        .get_namespace(&inc.alias)
        .ok_or_else(|| MdsError::undefined_var(&inc.alias))?;

    if let Some(body) = &ns.prompt_body {
        return Ok(body.clone());
    }
    if ctx.warnings.len() < MAX_WARNINGS {
        ctx.warnings.push(format!(
            "warning: @include of '{}' produced empty output — module has no body text",
            inc.alias
        ));
    }
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::ast::{DefineBlock, Interpolation, TextNode};

    fn text(s: &str) -> Node {
        Node::Text(TextNode {
            text: s.to_string(),
        })
    }

    #[test]
    fn evaluate_text() {
        let nodes = vec![text("Hello world!")];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings).unwrap(),
            "Hello world!"
        );
    }

    #[test]
    fn evaluate_variable_interpolation() {
        let nodes = vec![
            text("Hello "),
            Node::Interpolation(Interpolation {
                expr: Expr::Var("name".to_string()),
                offset: 6,
                len: 4,
            }),
            text("!"),
        ];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var("name", Value::String("Alice".to_string()));
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings).unwrap(),
            "Hello Alice!"
        );
    }

    #[test]
    fn evaluate_undefined_var_error() {
        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Var("unknown".to_string()),
            offset: 0,
            len: 7,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        let err = evaluate(&nodes, &mut scope, &mut warnings).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown"),
            "error message should reference the undefined variable name; got: {msg}"
        );
    }

    #[test]
    fn evaluate_if_truthy() {
        let nodes = vec![Node::If(IfBlock {
            condition: "flag".to_string(),
            then_body: vec![text("yes")],
            else_body: Some(vec![text("no")]),
            offset: 0,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var("flag", Value::Boolean(true));
        assert_eq!(evaluate(&nodes, &mut scope, &mut warnings).unwrap(), "yes");
    }

    #[test]
    fn evaluate_if_falsy() {
        let nodes = vec![Node::If(IfBlock {
            condition: "flag".to_string(),
            then_body: vec![text("yes")],
            else_body: Some(vec![text("no")]),
            offset: 0,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var("flag", Value::Boolean(false));
        assert_eq!(evaluate(&nodes, &mut scope, &mut warnings).unwrap(), "no");
    }

    #[test]
    fn evaluate_for_loop() {
        let nodes = vec![Node::For(ForBlock {
            var: "item".to_string(),
            iterable: "items".to_string(),
            body: vec![
                text("- "),
                Node::Interpolation(Interpolation {
                    expr: Expr::Var("item".to_string()),
                    offset: 2,
                    len: 4,
                }),
                text("\n"),
            ],
            offset: 0,
        })];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        scope.set_var(
            "items",
            Value::Array(vec![
                Value::String("apple".into()),
                Value::String("banana".into()),
            ]),
        );
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings).unwrap(),
            "- apple\n- banana\n"
        );
    }

    #[test]
    fn evaluate_function_call() {
        // The resolver handles @define and populates scope before evaluate() is called.
        // Simulate that here by pre-registering the function in scope.
        let define = DefineBlock {
            name: "greet".to_string(),
            params: vec!["name".to_string()],
            body: vec![
                text("Hello "),
                Node::Interpolation(Interpolation {
                    expr: Expr::Var("name".to_string()),
                    offset: 6,
                    len: 4,
                }),
                text("!"),
            ],
            offset: 0,
        };
        let mut scope = Scope::new();
        scope.set_function("greet", Arc::new(FunctionDef::from(&define)));

        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Call {
                name: "greet".to_string(),
                args: vec![Arg::StringLiteral("Bob".to_string())],
            },
            offset: 20,
            len: 12,
        })];
        let mut warnings = vec![];
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings).unwrap(),
            "Hello Bob!"
        );
    }

    #[test]
    fn evaluate_escaped_brace() {
        let nodes = vec![
            text("Use "),
            Node::EscapedBrace,
            text("name} for interpolation"),
        ];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        assert_eq!(
            evaluate(&nodes, &mut scope, &mut warnings).unwrap(),
            "Use {name} for interpolation"
        );
    }

    // ── prefer_first_error: double-fault error-preservation behaviour ─────────

    fn make_err(msg: &str) -> MdsError {
        MdsError::syntax(msg.to_string())
    }

    #[test]
    fn prefer_first_error_both_ok_returns_value() {
        let result: Result<&str, MdsError> = prefer_first_error(Ok("hello"), Ok(()));
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn prefer_first_error_first_err_wins_over_ok_second() {
        // When the render (first) fails and secondary is Ok, the render error surfaces.
        let result: Result<&str, MdsError> =
            prefer_first_error(Err(make_err("render error")), Ok(()));
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("render error"),
            "first error should be returned; got: {msg}"
        );
    }

    #[test]
    fn prefer_first_error_first_err_wins_over_second_err() {
        // Double-fault: both render and secondary fail. Render error takes precedence
        // because it carries the actionable source-span diagnostic for the user.
        let result: Result<&str, MdsError> = prefer_first_error(
            Err(make_err("render error")),
            Err(make_err("lifo error")),
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("render error"),
            "render (first) error should win over lifo error in double-fault; got: {msg}"
        );
    }

    #[test]
    fn prefer_first_error_ok_first_surfaces_second_err() {
        // When render succeeds but the secondary (LIFO/pop) fails, the secondary error surfaces.
        let result: Result<&str, MdsError> =
            prefer_first_error(Ok("value"), Err(make_err("lifo error")));
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("lifo error"),
            "secondary error should be returned when first is Ok; got: {msg}"
        );
    }
}
