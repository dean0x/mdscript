use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{
    required_param_count, Arg, CondValue, Condition, Expr, ForBlock, IfBlock, IncludeDirective,
    Node,
};
use crate::error::MdsError;
use crate::limits::{MAX_DOT_SEGMENTS, MAX_ELSEIF_BRANCHES, MAX_OUTPUT_SIZE};
use crate::scope::{FunctionDef, Scope};
use crate::value::Value;

/// Maximum call depth to prevent stack overflow from deeply nested calls.
const MAX_CALL_DEPTH: usize = 128;

/// Maximum number of iterations allowed in a single @for loop.
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Maximum total iterations across all loops in a single compilation.
/// This prevents nested loops from multiplying iterations into the billions.
const MAX_TOTAL_ITERATIONS: usize = 1_000_000;

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

/// Resolve a dot-separated path against the current scope.
///
/// `root` is the root variable name; `fields` are the field names to traverse into the
/// resolved root value. Passing an empty `fields` slice returns the root variable itself.
/// Returns `Ok(Value)` with the resolved value, or an error if the path is invalid.
fn resolve_dot_path(root: &str, fields: &[String], scope: &Scope) -> Result<Value, MdsError> {
    if fields.len() > MAX_DOT_SEGMENTS {
        return Err(MdsError::syntax(format!(
            "dot path exceeds maximum segment count of {MAX_DOT_SEGMENTS}"
        )));
    }
    let mut current = scope
        .get_var(root)
        .cloned()
        .ok_or_else(|| MdsError::undefined_var(root))?;
    for (i, field) in fields.iter().enumerate() {
        // Build the traversed path for error messages (e.g. "a.b" when failing at "c").
        let traversed_path = || {
            std::iter::once(root)
                .chain(fields[..i].iter().map(|s| s.as_str()))
                .collect::<Vec<_>>()
                .join(".")
        };
        match current {
            Value::Object(ref map) => {
                current = map.get(field).cloned().ok_or_else(|| {
                    MdsError::syntax(format!(
                        "field '{field}' not found on '{}'",
                        traversed_path()
                    ))
                })?;
            }
            _ => {
                return Err(MdsError::syntax(format!(
                    "cannot access field '{field}' on {} '{}'",
                    current.type_name(),
                    traversed_path()
                )));
            }
        }
    }
    Ok(current)
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
            // Objects cannot be directly interpolated — user must access a specific field
            if matches!(value, Value::Object(_)) {
                return Err(MdsError::syntax(format!(
                    "cannot interpolate object '{name}' directly, access a specific field with dot notation (e.g. {{{name}.field}})"
                )));
            }
            Ok(value.to_string())
        }
        Expr::Call { name, args } => {
            let resolved_args = resolve_args(args, scope, ctx, 0)?;
            Ok(call_function(name, &resolved_args, scope, ctx)?.to_string())
        }
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => {
            let resolved_args = resolve_args(args, scope, ctx, 0)?;
            Ok(call_qualified_function(namespace, name, &resolved_args, scope, ctx)?.to_string())
        }
        Expr::MemberAccess { object, fields } => {
            // Give a targeted error when the name refers to an imported namespace rather
            // than a variable — before delegating to resolve_dot_path which only looks up vars.
            if scope.get_var(object).is_none() && scope.get_namespace(object).is_some() {
                return Err(MdsError::syntax(format!(
                    "'{object}' is an imported module, not a variable — to call a function use {{{object}.func()}}"
                )));
            }
            let value = resolve_dot_path(object, fields, scope)?;
            // Objects cannot be directly interpolated — user must access a specific field
            match value {
                Value::Object(_) => Err(MdsError::syntax(format!(
                    "cannot interpolate object directly, access a specific field with dot notation (e.g. {{{object}.{field}}})",
                    field = fields.last().map_or("field", |f| f.as_str())
                ))),
                _ => Ok(value.to_string()),
            }
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
            Arg::NumberLiteral(n) => Ok(Value::Number(*n)),
            Arg::BooleanLiteral(b) => Ok(Value::Boolean(*b)),
            Arg::NullLiteral => Ok(Value::Null),
            Arg::Var(name) => scope
                .get_var(name)
                .cloned()
                .ok_or_else(|| MdsError::undefined_var(name)),
            Arg::Call {
                name,
                args: inner_args,
            } => {
                let resolved = resolve_args(inner_args, scope, ctx, depth + 1)?;
                call_function(name, &resolved, scope, ctx)
            }
            Arg::MemberAccess { object, fields } => resolve_dot_path(object, fields, scope),
        })
        .collect()
}

/// On double-fault, prefer the first error over the second.
///
/// Used when a render error AND an internal pop/LIFO error occur simultaneously:
/// the render error carries the actionable source-span diagnostic for the user,
/// while pop/LIFO failures are compiler bugs that surface as secondary errors.
fn prefer_first_error<T>(
    first: Result<T, MdsError>,
    second: Result<(), MdsError>,
) -> Result<T, MdsError> {
    match (first, second) {
        (Err(first_err), _) => Err(first_err),
        (Ok(_), Err(second_err)) => Err(second_err),
        (Ok(val), Ok(())) => Ok(val),
    }
}

/// Convert a `CondValue` (compile-time literal) to a runtime `Value`.
pub(crate) fn condvalue_to_value(cv: &CondValue) -> Value {
    match cv {
        CondValue::String(s) => Value::String(s.clone()),
        CondValue::Number(n) => Value::Number(*n),
        CondValue::Boolean(b) => Value::Boolean(*b),
        CondValue::Null => Value::Null,
    }
}

/// Restore the closure captures recorded at function-definition time into the
/// current scope frame.
///
/// Namespaces, sibling functions, and variables captured from the definition
/// site are written into `scope` before parameter binding so that params shadow
/// captured vars correctly (params take precedence over closure variables).
fn restore_captured_scope(func: &FunctionDef, scope: &mut Scope) {
    for (alias, ns) in &func.captured.namespaces {
        scope.set_namespace(alias, ns.clone());
    }
    // captured.functions are owned FunctionDef (not Arc) — wrap in Arc for scope insertion.
    for (name, f) in &func.captured.functions {
        scope.set_function(name, Arc::new(f.clone()));
    }
    for (name, val) in &func.captured.vars {
        scope.set_var(name, val.clone());
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
    let required = required_param_count(&func.params);
    let total = func.params.len();
    if args.len() < required || args.len() > total {
        return Err(MdsError::arity(call_key, required, total, args.len()));
    }
    scope.push();
    // Restore captured lexical scope from definition site so the function body
    // can resolve alias imports, sibling functions, and frontmatter variables
    // from its defining module.
    restore_captured_scope(func, scope);
    // Bind params index-by-index; fill missing optional params with their defaults.
    for (i, param) in func.params.iter().enumerate() {
        let value = if i < args.len() {
            args[i].clone()
        } else {
            // Required params were already checked above; this arm only fires for
            // optional params not supplied by the caller.
            let default = param.default.as_ref().ok_or_else(|| {
                MdsError::syntax(format!(
                    "internal error: non-optional param '{}' missing but arity check passed",
                    param.name
                ))
            })?;
            condvalue_to_value(default)
        };
        scope.set_var(&param.name, value);
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
    // On double-fault, preserve the render error — it carries the actionable
    // source-span diagnostic for the user. LIFO/pop failures are compiler
    // bugs and surface as secondary errors.
    prefer_first_error(result, lifo_result.and(pop_result))
}

fn call_function(
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Value, MdsError> {
    // User-defined functions take priority (shadowing built-ins).
    if let Some(func) = scope.get_function(name).cloned() {
        return invoke_function(&func, name, args, scope, ctx).map(Value::String);
    }
    // Fall back to built-ins.
    if let Some(meta) = crate::builtins::get_builtin(name) {
        // Defense-in-depth arity check: the validator enforces this at compile
        // time, but we guard here too so the evaluator is safe when called
        // without prior validation (e.g. in unit tests or future API consumers).
        if args.len() < meta.min_args || args.len() > meta.max_args {
            return Err(MdsError::arity(
                name,
                meta.min_args,
                meta.max_args,
                args.len(),
            ));
        }
        // Call the handler directly using the meta reference we already hold,
        // avoiding a second linear scan through BUILTINS that call_builtin
        // would perform internally.
        return (meta.handler)(args);
    }
    Err(MdsError::undefined_fn(name))
}

fn call_qualified_function(
    namespace: &str,
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<Value, MdsError> {
    let qualified_name = format!("{namespace}.{name}");

    let ns = scope
        .get_namespace(namespace)
        .ok_or_else(|| MdsError::undefined_var(namespace))?;

    let func = ns
        .functions
        .get(name)
        .ok_or_else(|| MdsError::undefined_fn(&qualified_name))?
        .clone();

    invoke_function(&func, &qualified_name, args, scope, ctx).map(Value::String)
}

/// Compare a runtime `Value` against a literal `CondValue` using strict equality.
///
/// Type matching is strict — `Number(3) != String("3")`. Different types always
/// return `false`. `NaN == NaN` is `false` (IEEE 754 via Rust's `f64 ==`).
fn values_equal(value: &Value, expected: &CondValue) -> bool {
    match (value, expected) {
        (Value::String(s), CondValue::String(e)) => s == e,
        (Value::Number(n), CondValue::Number(e)) => n == e,
        (Value::Boolean(b), CondValue::Boolean(e)) => b == e,
        (Value::Null, CondValue::Null) => true,
        _ => false,
    }
}

/// Resolve the dot-path for a condition, returning the runtime `Value`.
fn resolve_condition_value(condition: &Condition, scope: &Scope) -> Result<Value, MdsError> {
    let path = condition.path();
    resolve_dot_path(condition.root()?, &path[1..], scope)
}

/// Evaluate a condition to a boolean, resolving the dot-path variable from scope.
fn evaluate_condition(condition: &Condition, scope: &Scope) -> Result<bool, MdsError> {
    match condition {
        Condition::Truthy(_) => Ok(resolve_condition_value(condition, scope)?.is_truthy()),
        Condition::Not(_) => Ok(!resolve_condition_value(condition, scope)?.is_truthy()),
        Condition::Eq(_, expected) => Ok(values_equal(
            &resolve_condition_value(condition, scope)?,
            expected,
        )),
        Condition::NotEq(_, expected) => Ok(!values_equal(
            &resolve_condition_value(condition, scope)?,
            expected,
        )),
        // Short-circuit And: return false on first false operand.
        // Parser invariant: And operands are always leaf conditions (parse_and_level calls
        // parse_simple_condition for each part). The total operand count is bounded at
        // parse time by MAX_LOGICAL_OPERANDS (limits.rs), preventing adversarial trees.
        // The debug_assert below is a dev-only canary (elided in release builds): it fires
        // in tests if a future grammar change introduces And-in-And nesting without updating
        // this evaluator, surfacing the breakage before it reaches production.
        Condition::And(operands) => {
            for operand in operands {
                debug_assert!(
                    !matches!(operand, Condition::And(_) | Condition::Or(_)),
                    "And operand should be a leaf condition, not And/Or"
                );
                if !evaluate_condition(operand, scope)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        // Short-circuit Or: return true on first true operand.
        // Parser invariant: Or operands are always And-level results (And or leaf), never Or.
        // The total operand count is bounded at parse time by MAX_LOGICAL_OPERANDS (limits.rs).
        // The debug_assert is a dev-only canary that fires in tests if a future grammar
        // change introduces Or-in-Or nesting without updating this evaluator.
        Condition::Or(operands) => {
            for operand in operands {
                debug_assert!(
                    !matches!(operand, Condition::Or(_)),
                    "Or operand should not be Or (parser flattens same-level operators)"
                );
                if evaluate_condition(operand, scope)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn evaluate_if(
    block: &IfBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    // Evaluate the primary condition
    if evaluate_condition(&block.condition, scope)? {
        return evaluate_nodes(&block.then_body, scope, ctx);
    }

    // Evaluate @elseif branches in order (short-circuit: first true branch wins)
    // Parser enforces MAX_ELSEIF_BRANCHES at construction time; assert the invariant
    // holds so evaluator correctness cannot silently depend on the parser limit alone.
    debug_assert!(
        block.elseif_branches.len() <= MAX_ELSEIF_BRANCHES,
        "elseif_branches length {} exceeds MAX_ELSEIF_BRANCHES {}",
        block.elseif_branches.len(),
        MAX_ELSEIF_BRANCHES,
    );
    for (cond, body) in &block.elseif_branches {
        if evaluate_condition(cond, scope)? {
            return evaluate_nodes(body, scope, ctx);
        }
    }

    // Fall through to @else body if present
    if let Some(else_body) = &block.else_body {
        evaluate_nodes(else_body, scope, ctx)
    } else {
        Ok(String::new())
    }
}

/// Execute one loop body iteration: push a scope frame, bind variables, render, pop.
///
/// `bindings` is a `Vec` of `(name, value)` pairs to set in the pushed scope frame.
/// Values are moved into scope directly, avoiding a clone per iteration.
/// On double-fault (render error + pop error), the render error is preferred.
fn run_loop_body(
    scope: &mut Scope,
    ctx: &mut EvalContext,
    body: &[Node],
    bindings: Vec<(&str, Value)>,
) -> Result<String, MdsError> {
    scope.push();
    for (name, val) in bindings {
        scope.set_var(name, val);
    }
    let rendered = evaluate_nodes(body, scope, ctx);
    let pop_result = scope.pop();
    prefer_first_error(rendered, pop_result)
}

/// Key-value iteration path for `@for key, value in obj:`.
fn evaluate_for_key_value(
    key_var: &str,
    val_var: &str,
    map: HashMap<String, Value>,
    body: &[Node],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    if map.len() > MAX_LOOP_ITERATIONS {
        return Err(MdsError::resource_limit(format!(
            "object has {} entries, exceeding maximum loop iteration limit of {}",
            map.len(),
            MAX_LOOP_ITERATIONS
        )));
    }

    // Sort keys alphabetically for deterministic output
    let mut entries: Vec<(String, Value)> = map.into_iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut output = String::new();
    for (key, val) in entries {
        ctx.total_iterations += 1;
        if ctx.total_iterations > MAX_TOTAL_ITERATIONS {
            return Err(MdsError::resource_limit(format!(
                "total loop iterations exceeded maximum of {} across all loops in this compilation",
                MAX_TOTAL_ITERATIONS
            )));
        }
        let rendered = run_loop_body(
            scope,
            ctx,
            body,
            vec![(key_var, Value::String(key)), (val_var, val)],
        )?;
        output.push_str(&rendered);
    }
    Ok(output)
}

/// Array iteration path for `@for item in array:`.
fn evaluate_for_array(
    loop_var: &str,
    iterable: Value,
    body: &[Node],
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    if let Value::Object(_) = &iterable {
        return Err(MdsError::syntax(
            "to iterate over an object's entries, use `@for key, value in obj:` syntax",
        ));
    }

    let array = iterable
        .as_array()
        .ok_or_else(|| MdsError::type_error(iterable.type_name()))?;

    // Check length before cloning to avoid allocating the full Vec only to
    // immediately return a resource-limit error for oversized arrays.
    if array.len() > MAX_LOOP_ITERATIONS {
        return Err(MdsError::resource_limit(format!(
            "array has {} elements, exceeding maximum loop iteration limit of {}",
            array.len(),
            MAX_LOOP_ITERATIONS
        )));
    }

    // Clone the array items to release the shared borrow on `scope` so the
    // loop body can call `scope.set_var` without a borrow conflict.
    let items = array.to_vec();

    let mut output = String::new();
    for item in items {
        ctx.total_iterations += 1;
        if ctx.total_iterations > MAX_TOTAL_ITERATIONS {
            return Err(MdsError::resource_limit(format!(
                "total loop iterations exceeded maximum of {} across all loops in this compilation",
                MAX_TOTAL_ITERATIONS
            )));
        }
        output.push_str(&run_loop_body(scope, ctx, body, vec![(loop_var, item)])?);
    }

    Ok(output)
}

fn evaluate_for(
    block: &ForBlock,
    scope: &mut Scope,
    ctx: &mut EvalContext,
) -> Result<String, MdsError> {
    let root = block
        .iterable
        .first()
        .ok_or_else(|| MdsError::syntax("internal error: @for block has empty iterable path"))?;
    let iterable = resolve_dot_path(root, &block.iterable[1..], scope)?;

    if let Some(ref key_var) = block.key_var {
        // Key-value iteration: @for key, value in obj:
        let map = match iterable {
            Value::Object(m) => m,
            _ => {
                return Err(MdsError::syntax(format!(
                    "key-value iteration requires an object, but got {}",
                    iterable.type_name()
                )));
            }
        };
        return evaluate_for_key_value(key_var, &block.var, map, &block.body, scope, ctx);
    }

    // Standard array iteration: @for item in iterable:
    evaluate_for_array(&block.var, iterable, &block.body, scope, ctx)
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
    use crate::ast::{Condition, DefineBlock, Interpolation, Param, TextNode};
    use std::sync::Arc;

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
            condition: Condition::Truthy(vec!["flag".to_string()]),
            elseif_branches: vec![],
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
            condition: Condition::Truthy(vec!["flag".to_string()]),
            elseif_branches: vec![],
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
            key_var: None,
            iterable: vec!["items".to_string()],
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
            params: vec![Param::required("name")],
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
        MdsError::syntax(msg)
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
        let result: Result<&str, MdsError> =
            prefer_first_error(Err(make_err("render error")), Err(make_err("lifo error")));
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

    // ── Resource limit: MAX_CALL_DEPTH ───────────────────────────────────────

    #[test]
    fn call_depth_limit_rejects_deep_call_stack() {
        // Build a call chain of MAX_CALL_DEPTH + 2 functions (f0 calls f1, f1 calls f2, ...).
        // Uses direct AST construction to avoid the resolver's O(n^3) closure-capture overhead.
        //
        // Each FunctionDef has a simple body: [Interpolation(Call { "f{i+1}", [] })].
        // The last function in the chain calls f0 to close the cycle (preventing
        // the direct-recursion short-circuit which only catches same-key re-entry).
        let n = MAX_CALL_DEPTH + 2;
        let mut scope = Scope::new();

        // Register n functions: fi calls f(i+1 % n)
        for i in 0..n {
            let next = (i + 1) % n;
            let next_name = format!("f{next}");
            let body = vec![Node::Interpolation(Interpolation {
                expr: Expr::Call {
                    name: next_name,
                    args: vec![],
                },
                offset: 0,
                len: 3,
            })];
            let func = FunctionDef {
                params: vec![],
                body,
                captured: crate::scope::CapturedScope::default(),
            };
            scope.set_function(&format!("f{i}"), Arc::new(func));
        }

        // Invoke f0 — should fail with a call depth error
        let call_node = vec![Node::Interpolation(Interpolation {
            expr: Expr::Call {
                name: "f0".to_string(),
                args: vec![],
            },
            offset: 0,
            len: 4,
        })];
        let mut warnings = vec![];
        let result = evaluate(&call_node, &mut scope, &mut warnings);
        assert!(result.is_err(), "call chain of {n} must be rejected");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("call depth"),
            "error should mention call depth (got: {err})"
        );
    }

    // ── values_equal: NaN semantics ──────────────────────────────────────────

    #[test]
    fn values_equal_nan_is_not_equal_to_itself() {
        // IEEE 754 defines NaN != NaN. values_equal must follow this — even though
        // the parser rejects NaN literals in condition values, the runtime Value type
        // holds f64 and could theoretically carry a NaN produced by arithmetic.
        // Verify that values_equal returns false for NaN == NaN.
        let nan_value = Value::Number(f64::NAN);
        let nan_cond = CondValue::Number(f64::NAN);
        assert!(
            !values_equal(&nan_value, &nan_cond),
            "NaN must not equal NaN (IEEE 754)"
        );
    }

    // ── Resource limit: MAX_OUTPUT_SIZE ──────────────────────────────────────

    #[test]
    fn output_size_limit_rejects_oversized_output() {
        // Build a node list that accumulates past MAX_OUTPUT_SIZE (50 MB) across two
        // nodes, rather than allocating a single 50 MB+ string in one shot.
        // Each node is ~26 MB; after the second node the accumulated output exceeds
        // the limit and evaluate_nodes returns a ResourceLimit error.
        //
        // Using two nodes of ~26 MB each keeps peak allocation at ~52 MB total
        // (26 MB node + 26 MB node + accumulating output buffer), which is safer
        // for CI environments than a single 50 MB+ pre-allocated string.
        let half = MAX_OUTPUT_SIZE / 2 + 1;
        let chunk = "x".repeat(half);
        let nodes = vec![text(&chunk), text(&chunk)];
        let mut scope = Scope::new();
        let mut warnings = vec![];
        let result = evaluate(&nodes, &mut scope, &mut warnings);
        assert!(
            result.is_err(),
            "output exceeding MAX_OUTPUT_SIZE must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("output") || err.contains("maximum size") || err.contains("50"),
            "error should mention output size limit, got: {err}"
        );
    }

    // ── Default parameter filling ─────────────────────────────────────────────

    #[test]
    fn evaluate_default_param_used_when_not_provided() {
        let result = crate::compile_str(
            "@define greet(name = \"World\"):\nHello {name}!\n@end\n{greet()}\n",
        )
        .unwrap();
        assert_eq!(result, "Hello World!\n");
    }

    #[test]
    fn evaluate_default_param_overridden_when_provided() {
        let result = crate::compile_str(
            "@define greet(name = \"World\"):\nHello {name}!\n@end\n{greet(\"Alice\")}\n",
        )
        .unwrap();
        assert_eq!(result, "Hello Alice!\n");
    }

    #[test]
    fn evaluate_all_defaults_provided() {
        let result =
            crate::compile_str("@define add(a = 1, b = 2):\n{a} {b}\n@end\n{add(10, 20)}\n")
                .unwrap();
        assert_eq!(result, "10 20\n");
    }

    #[test]
    fn evaluate_mixed_required_and_default() {
        let result = crate::compile_str(
            "@define greet(name, greeting = \"Hello\"):\n{greeting} {name}!\n@end\n{greet(\"Bob\")}\n",
        )
        .unwrap();
        assert_eq!(result, "Hello Bob!\n");
    }

    #[test]
    fn evaluate_default_param_number() {
        // condvalue_to_value: CondValue::Number → Value::Number
        let result = crate::compile_str("@define show(x = 42):\n{x}\n@end\n{show()}\n").unwrap();
        assert!(
            result.contains("42"),
            "Number default should produce its numeric value, got: {result}"
        );
    }

    #[test]
    fn evaluate_default_param_boolean_true() {
        // condvalue_to_value: CondValue::Boolean(true) → Value::Boolean(true)
        let result = crate::compile_str(
            "@define show(flag = true):\n@if flag:\nyes\n@else:\nno\n@end\n@end\n{show()}\n",
        )
        .unwrap();
        assert!(
            result.contains("yes"),
            "Boolean true default should be truthy, got: {result}"
        );
    }

    #[test]
    fn evaluate_default_param_boolean_false() {
        // condvalue_to_value: CondValue::Boolean(false) → Value::Boolean(false)
        let result = crate::compile_str(
            "@define show(flag = false):\n@if flag:\nyes\n@else:\nno\n@end\n@end\n{show()}\n",
        )
        .unwrap();
        assert!(
            result.contains("no"),
            "Boolean false default should be falsy, got: {result}"
        );
    }

    #[test]
    fn evaluate_default_param_null() {
        // condvalue_to_value: CondValue::Null → Value::Null (falsy)
        let result = crate::compile_str(
            "@define show(x = null):\n@if x:\nset\n@else:\nnull_branch\n@end\n@end\n{show()}\n",
        )
        .unwrap();
        assert!(
            result.contains("null_branch"),
            "Null default should be falsy, got: {result}"
        );
    }

    #[test]
    fn evaluate_arity_error_on_too_few_required_args() {
        let result = crate::compile_str("@define greet(name):\n{name}\n@end\n{greet()}\n");
        assert!(result.is_err(), "too few required args should fail");
    }

    // ── Built-in functions integration ────────────────────────────────────────

    #[test]
    fn builtin_upper_in_interpolation() {
        let result = crate::compile_str("---\nword: hello\n---\n{upper(word)}\n").unwrap();
        assert!(
            result.contains("HELLO"),
            "upper() should uppercase, got: {result}"
        );
    }

    #[test]
    fn builtin_lower_in_interpolation() {
        let result = crate::compile_str("---\nword: HELLO\n---\n{lower(word)}\n").unwrap();
        assert!(
            result.contains("hello"),
            "lower() should lowercase, got: {result}"
        );
    }

    #[test]
    fn builtin_length_string_in_interpolation() {
        let result = crate::compile_str("---\nword: hello\n---\n{length(word)}\n").unwrap();
        assert!(
            result.contains('5'),
            "length of 'hello' should be 5, got: {result}"
        );
    }

    #[test]
    fn builtin_shadowed_by_user_function() {
        // A user-defined function named 'upper' should shadow the built-in.
        let src = "@define upper(x):\ncustom\n@end\n{upper(\"anything\")}\n";
        let result = crate::compile_str(src).unwrap();
        assert_eq!(
            result.trim(),
            "custom",
            "user-defined upper() should shadow built-in"
        );
    }

    #[test]
    fn builtin_compose_join_split() {
        let result =
            crate::compile_str("---\ncsv: a,b,c\n---\n{join(split(csv, \",\"), \" | \")}\n")
                .unwrap();
        assert!(
            result.contains("a | b | c"),
            "join(split()) should work, got: {result}"
        );
    }

    #[test]
    fn builtin_string_with_number_literal_arg() {
        let result = crate::compile_str("{string(42)}\n").unwrap();
        assert_eq!(result.trim(), "42");
    }
}
