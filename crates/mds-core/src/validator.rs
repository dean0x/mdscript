use crate::ast::{
    required_param_count, Arg, BlockNode, Condition, Expr, ForBlock, IfBlock, MessageBlock, Node,
};
use crate::error::MdsError;
use crate::scope::Scope;
use crate::value::Value;

/// Validate semantic correctness of a module AST.
/// Checks variable references, function arity, and type constraints
/// before evaluation. Block-scoped variables (e.g., @for loop vars)
/// are verified at evaluation time.
pub fn validate(
    nodes: &[Node],
    scope: &mut Scope,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    for node in nodes {
        validate_node(node, scope, file, source)?;
    }
    Ok(())
}

fn validate_node(node: &Node, scope: &mut Scope, file: &str, source: &str) -> Result<(), MdsError> {
    match node {
        Node::Text(_) | Node::EscapedBrace => Ok(()),
        Node::Interpolation(interp) => {
            validate_expr(&interp.expr, scope, file, source, interp.offset, interp.len)
        }
        Node::If(block) => validate_if_node(block, scope, file, source),
        Node::For(block) => validate_for_node(block, scope, file, source),
        Node::Define(def) => {
            scope.push();
            for param in &def.params {
                // Use an empty array as the placeholder for each parameter so
                // that `@for item in param:` inside the body passes the type
                // check. The actual type is enforced at call time by the evaluator.
                scope.set_var(&param.name, Value::Array(vec![]));
            }
            let result = validate(&def.body, scope, file, source);
            let _ = scope.pop(); // Cannot fail — we just pushed
            result
        }
        Node::Import(_) | Node::Export(_) => {
            // Handled by resolver
            Ok(())
        }
        Node::Include(inc) => scope
            .get_namespace(&inc.alias)
            .ok_or_else(|| {
                MdsError::undefined_var_at(&inc.alias, file, source, inc.offset, inc.alias.len())
            })
            .map(|_| ()),
        Node::Message(block) => validate_message_node(block, scope, file, source),
        Node::Block(block) => validate_block_node(block, scope, file, source),
    }
}

fn validate_message_node(
    block: &MessageBlock,
    scope: &mut Scope,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    // Validate the role expression if dynamic.
    // Bare-word roles are StringLiterals and need no scope validation.
    match &block.role {
        Expr::StringLiteral(_) => {}
        role_expr => {
            validate_expr(role_expr, scope, file, source, block.offset, 0)?;
        }
    }
    validate(&block.body, scope, file, source)
}

fn validate_block_node(
    block: &BlockNode,
    scope: &mut Scope,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    validate(&block.body, scope, file, source)
}

/// Validate an `@if` block: conditions and all branch bodies.
///
/// INVARIANT: @if does not push a scope frame. then_body and else_body are
/// validated against the same &mut Scope. This is safe because no directive
/// that modifies scope (e.g. @define, @for) is valid at block level inside
/// an @if — those are caught by the parser. If future directives that inject
/// scope bindings are added at this level, each branch must get its own
/// push()/pop() frame to prevent bindings from leaking across branches.
fn validate_if_node(
    block: &IfBlock,
    scope: &mut Scope,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    // Validate that the root variable of each condition is defined in scope.
    validate_condition(&block.condition, scope, file, source, block.offset)?;
    validate(&block.then_body, scope, file, source)?;
    // Validate all @elseif branches
    for (elseif_cond, elseif_body) in &block.elseif_branches {
        validate_condition(elseif_cond, scope, file, source, block.offset)?;
        validate(elseif_body, scope, file, source)?;
    }
    if let Some(else_body) = &block.else_body {
        validate(else_body, scope, file, source)?;
    }
    Ok(())
}

/// Validate a `@for` block: iterable expression check and body with loop variables in scope.
///
/// Static type checks are performed only when the iterable is a simple `Expr::Var`
/// because other expression types (Call, QualifiedCall, MemberAccess) have return
/// types that cannot be determined statically. Any type mismatch on those expressions
/// surfaces at evaluation time.
fn validate_for_node(
    block: &ForBlock,
    scope: &mut Scope,
    file: &str,
    source: &str,
) -> Result<(), MdsError> {
    // Validate the iterable expression.
    match &block.iterable {
        Expr::Var(root) => {
            let iterable_val = scope.get_var(root).ok_or_else(|| {
                MdsError::undefined_var_at(root, file, source, block.offset, root.len())
            })?;
            // Only perform static type checks for simple identifier iterables when
            // there is no key_var (single-var iteration should be an array or object).
            //
            // ACCEPTED LIMITATION: same as before — for object fields, dot-paths, and
            // expression iterables, type mismatches surface at evaluation time.
            if block.key_var.is_none() && !matches!(iterable_val, Value::Array(_)) {
                if matches!(iterable_val, Value::Object(_)) {
                    return Err(MdsError::syntax_at(
                        format!(
                            "cannot iterate over object '{root}' with a single variable — \
                             use @for key, value in {root}: to iterate over an object's entries"
                        ),
                        file,
                        source,
                        block.offset,
                        root.len(),
                    ));
                }
                return Err(MdsError::type_error_at(
                    iterable_val.type_name(),
                    file,
                    source,
                    block.offset,
                    root.len(),
                ));
            }
        }
        Expr::MemberAccess { object, .. } => {
            // Validate that the root object exists; field type is checked at runtime.
            scope.get_var(object).ok_or_else(|| {
                MdsError::undefined_var_at(object, file, source, block.offset, object.len())
            })?;
        }
        Expr::Call { name, .. } | Expr::QualifiedCall { name, .. } => {
            validate_expr(
                &block.iterable,
                scope,
                file,
                source,
                block.offset,
                name.len(),
            )?;
        }
        // Literal variants should have been rejected at parse time; guard defensively.
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NullLiteral => {
            return Err(MdsError::syntax(
                "internal error: literal value used as @for iterable — should have been rejected at parse time",
            ));
        }
    }

    scope.push();
    if let Some(ref key_var) = block.key_var {
        scope.set_var(key_var, Value::Null);
    }
    scope.set_var(&block.var, Value::Null);
    let result = validate(&block.body, scope, file, source);
    let _ = scope.pop(); // Cannot fail — we just pushed
    result
}

/// Validate condition expressions: check that variables and functions are defined in scope.
///
/// For compound conditions (`And`/`Or`), recursively validates all operands
/// (conservative — validates all branches, not just the short-circuit path).
/// Literal Expr variants (StringLiteral, NumberLiteral, etc.) need no validation.
fn validate_condition(
    condition: &Condition,
    scope: &Scope,
    file: &str,
    source: &str,
    offset: usize,
) -> Result<(), MdsError> {
    match condition {
        Condition::And(operands) | Condition::Or(operands) => {
            for operand in operands {
                validate_condition(operand, scope, file, source, offset)?;
            }
            Ok(())
        }
        Condition::Truthy(expr) | Condition::Not(expr) => {
            validate_condition_expr(expr, scope, file, source, offset)
        }
        Condition::Eq(lhs, rhs) | Condition::NotEq(lhs, rhs) => {
            validate_condition_expr(lhs, scope, file, source, offset)?;
            validate_condition_expr(rhs, scope, file, source, offset)
        }
    }
}

/// Validate a single expression used inside a condition.
///
/// Literal variants (StringLiteral, NumberLiteral, BooleanLiteral, NullLiteral)
/// are always valid and need no scope lookup. All other variants delegate to
/// `validate_expr` which performs the full expression validation.
fn validate_condition_expr(
    expr: &Expr,
    scope: &Scope,
    file: &str,
    source: &str,
    offset: usize,
) -> Result<(), MdsError> {
    match expr {
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NullLiteral => Ok(()),
        _ => {
            // Use the expression's own name length as the span length for errors
            let len = match expr {
                Expr::Var(name) => name.len(),
                Expr::Call { name, .. } => name.len(),
                Expr::QualifiedCall { name, .. } => name.len(),
                Expr::MemberAccess { object, .. } => object.len(),
                _ => unreachable!("literal variants handled above"),
            };
            validate_expr(expr, scope, file, source, offset, len)
        }
    }
}

/// Resolve a call by name and validate arity.
///
/// Returns `Ok(())` if the call is valid, or an error if the function is
/// unknown or the arity is out of range.
///
/// `len` is the span length used for error diagnostics — callers pass the
/// full call-site span for top-level calls and `name.len()` for nested calls
/// inside argument lists.
fn validate_call_arity(
    name: &str,
    arg_count: usize,
    scope: &Scope,
    file: &str,
    source: &str,
    offset: usize,
    len: usize,
) -> Result<(), MdsError> {
    if let Some(func) = scope.get_function(name) {
        let required = required_param_count(&func.params);
        let total = func.params.len();
        if arg_count < required || arg_count > total {
            return Err(MdsError::arity_at(
                name, required, total, arg_count, file, source, offset, len,
            ));
        }
        Ok(())
    } else if let Some(meta) = crate::builtins::get_builtin(name) {
        if arg_count < meta.min_args || arg_count > meta.max_args {
            return Err(MdsError::arity_at(
                name,
                meta.min_args,
                meta.max_args,
                arg_count,
                file,
                source,
                offset,
                len,
            ));
        }
        Ok(())
    } else {
        Err(MdsError::undefined_fn_at(name, file, source, offset, len))
    }
}

fn validate_expr(
    expr: &Expr,
    scope: &Scope,
    file: &str,
    source: &str,
    offset: usize,
    len: usize,
) -> Result<(), MdsError> {
    match expr {
        Expr::Var(name) => scope
            .get_var(name)
            .ok_or_else(|| MdsError::undefined_var_at(name, file, source, offset, name.len()))
            .map(|_| ()),
        Expr::MemberAccess { object, .. } => {
            // Validate that the root object is defined in scope.
            // Field existence is checked at runtime since objects may vary.
            scope
                .get_var(object)
                .ok_or_else(|| {
                    MdsError::undefined_var_at(object, file, source, offset, object.len())
                })
                .map(|_| ())
        }
        Expr::Call { name, args } => {
            validate_call_arity(name, args.len(), scope, file, source, offset, len)?;
            validate_var_args(args, scope, file, source, offset, 0)
        }
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => {
            let ns = scope
                .get_namespace(namespace)
                .ok_or_else(|| MdsError::undefined_var_at(namespace, file, source, offset, len))?;
            let qualified = format!("{namespace}.{name}");
            let func = ns
                .functions
                .get(name)
                .ok_or_else(|| MdsError::undefined_fn_at(&qualified, file, source, offset, len))?;
            let required = required_param_count(&func.params);
            let total = func.params.len();
            if args.len() < required || args.len() > total {
                return Err(MdsError::arity_at(
                    &qualified,
                    required,
                    total,
                    args.len(),
                    file,
                    source,
                    offset,
                    len,
                ));
            }
            validate_var_args(args, scope, file, source, offset, 0)
        }
        // Literal variants are always valid — no scope lookup required.
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NullLiteral => Ok(()),
    }
}

/// Check that all arguments are valid: variable refs exist, nested calls are well-formed.
fn validate_var_args(
    args: &[Arg],
    scope: &Scope,
    file: &str,
    source: &str,
    offset: usize,
    depth: usize,
) -> Result<(), MdsError> {
    if depth > crate::limits::MAX_NESTING_DEPTH {
        return Err(MdsError::syntax(
            "nested argument validation depth exceeded",
        ));
    }
    for arg in args {
        match arg {
            Arg::StringLiteral(_)
            | Arg::NumberLiteral(_)
            | Arg::BooleanLiteral(_)
            | Arg::NullLiteral => {}
            Arg::Var(var_name) => {
                scope.get_var(var_name).ok_or_else(|| {
                    MdsError::undefined_var_at(var_name, file, source, offset, var_name.len())
                })?;
            }
            Arg::MemberAccess { object, .. } => {
                // Validate that the root object is defined in scope.
                // Field existence is checked at runtime.
                scope.get_var(object).ok_or_else(|| {
                    MdsError::undefined_var_at(object, file, source, offset, object.len())
                })?;
            }
            Arg::Call {
                name,
                args: inner_args,
            } => {
                // Validate the nested call — check user-defined first, then builtins.
                // Nested calls use name.len() as the span (no full call-site span available).
                validate_call_arity(
                    name,
                    inner_args.len(),
                    scope,
                    file,
                    source,
                    offset,
                    name.len(),
                )?;
                validate_var_args(inner_args, scope, file, source, offset, depth + 1)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Built-in arity checking ───────────────────────────────────────────────

    /// Build a top-level `{call(...)}` interpolation node for testing.
    fn call_node(name: &str, args: Vec<crate::ast::Arg>) -> Node {
        Node::Interpolation(crate::ast::Interpolation {
            expr: crate::ast::Expr::Call {
                name: name.to_string(),
                args,
            },
            offset: 0,
            len: name.len(),
        })
    }

    #[test]
    fn builtin_upper_zero_args_fails_arity_check() {
        // {upper()} — upper requires exactly 1 arg; 0 args must fail.
        let node = call_node("upper", vec![]);
        let mut scope = Scope::new();
        let result = validate(&[node], &mut scope, "test.mds", "");
        assert!(
            result.is_err(),
            "upper() with 0 args must fail arity check, got: {result:?}"
        );
    }

    #[test]
    fn builtin_upper_two_args_fails_arity_check() {
        // {upper(x, y)} — upper requires exactly 1 arg; 2 args must fail.
        // String literal args avoid triggering the undefined-variable check first.
        let args = vec![
            crate::ast::Arg::StringLiteral("hello".to_string()),
            crate::ast::Arg::StringLiteral("world".to_string()),
        ];
        let node = call_node("upper", args);
        let mut scope = Scope::new();
        let result = validate(&[node], &mut scope, "test.mds", "");
        assert!(
            result.is_err(),
            "upper() with 2 args must fail arity check, got: {result:?}"
        );
    }

    #[test]
    fn builtin_upper_one_literal_arg_passes_arity_check() {
        // {upper("hello")} — upper requires exactly 1 arg; 1 literal arg must pass.
        let args = vec![crate::ast::Arg::StringLiteral("hello".to_string())];
        let node = call_node("upper", args);
        let mut scope = Scope::new();
        let result = validate(&[node], &mut scope, "test.mds", "");
        assert!(
            result.is_ok(),
            "upper() with 1 literal arg must pass arity check: {result:?}"
        );
    }

    #[test]
    fn builtin_replace_two_args_fails_arity_check() {
        // {replace(s, old)} — replace requires exactly 3 args; 2 must fail.
        let args = vec![
            crate::ast::Arg::StringLiteral("hello".to_string()),
            crate::ast::Arg::StringLiteral("h".to_string()),
        ];
        let node = call_node("replace", args);
        let mut scope = Scope::new();
        let result = validate(&[node], &mut scope, "test.mds", "");
        assert!(
            result.is_err(),
            "replace() with 2 args must fail arity check, got: {result:?}"
        );
    }

    #[test]
    fn undefined_function_fails_with_error() {
        // {notabuiltin()} — completely unknown function must fail.
        let node = call_node("notabuiltin", vec![]);
        let mut scope = Scope::new();
        let result = validate(&[node], &mut scope, "test.mds", "");
        assert!(
            result.is_err(),
            "unknown function must fail validation, got: {result:?}"
        );
    }

    // ── Existing tests ────────────────────────────────────────────────────────

    #[test]
    fn define_body_with_undefined_var_fails_at_validate_time() {
        // @define greet(name): {undefined_var} @end — body references undefined var
        let body = vec![Node::Interpolation(crate::ast::Interpolation {
            expr: crate::ast::Expr::Var("undefined_var".to_string()),
            offset: 0,
            len: 13,
        })];
        let define = Node::Define(crate::ast::DefineBlock {
            name: "greet".to_string(),
            params: vec![crate::ast::Param::required("name")],
            body,
            offset: 0,
        });
        let mut scope = Scope::new();
        let result = validate(&[define], &mut scope, "test.mds", "");
        assert!(
            result.is_err(),
            "undefined var inside @define body must fail at validate time"
        );
    }

    #[test]
    fn define_body_referencing_param_passes_validation() {
        // @define greet(name): {name} @end — param is in scope, must pass.
        let body = vec![Node::Interpolation(crate::ast::Interpolation {
            expr: crate::ast::Expr::Var("name".to_string()),
            offset: 0,
            len: 4,
        })];
        let define = Node::Define(crate::ast::DefineBlock {
            name: "greet".to_string(),
            params: vec![crate::ast::Param::required("name")],
            body,
            offset: 0,
        });
        let mut scope = Scope::new();
        let result = validate(&[define], &mut scope, "test.mds", "");
        assert!(
            result.is_ok(),
            "param reference inside @define must pass: {result:?}"
        );
    }
}
