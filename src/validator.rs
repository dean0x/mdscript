use crate::ast::{Arg, Expr, Node};
use crate::error::MdsError;
use crate::scope::Scope;
use crate::value::Value;

/// Validate semantic correctness of a module AST.
/// Checks variable references, function arity, and type constraints
/// before evaluation. Block-scoped variables (e.g., @for loop vars)
/// are verified at evaluation time.
pub fn validate(nodes: &[Node], scope: &mut Scope, file: &str, source: &str) -> Result<(), MdsError> {
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
        Node::If(block) => {
            // Condition root must be a defined variable (truthiness is checked at evaluation time)
            debug_assert!(!block.condition.is_empty(), "IfBlock.condition must be non-empty (parser invariant)");
            let root = &block.condition[0];
            scope.get_var(root).ok_or_else(|| {
                MdsError::undefined_var_at(
                    root,
                    file,
                    source,
                    block.offset,
                    root.len(),
                )
            })?;
            // INVARIANT: @if does not push a scope frame. then_body and else_body are
            // validated against the same &mut Scope. This is safe because no directive
            // that modifies scope (e.g. @define, @for) is valid at block level inside
            // an @if — those are caught by the parser. If future directives that inject
            // scope bindings are added at this level, each branch must get its own
            // push()/pop() frame to prevent bindings from leaking across branches.
            validate(&block.then_body, scope, file, source)?;
            if let Some(else_body) = &block.else_body {
                validate(else_body, scope, file, source)?;
            }
            Ok(())
        }
        Node::For(block) => {
            debug_assert!(!block.iterable.is_empty(), "ForBlock.iterable must be non-empty (parser invariant)");
            let root = &block.iterable[0];
            let iterable_val = scope.get_var(root).ok_or_else(|| {
                MdsError::undefined_var_at(
                    root,
                    file,
                    source,
                    block.offset,
                    root.len(),
                )
            })?;
            // Only perform static type checks when:
            // 1. No key_var (single-var iteration should be an array)
            // 2. The iterable is a simple identifier (no dot path — can't statically resolve type)
            if block.key_var.is_none() && block.iterable.len() == 1 && !matches!(iterable_val, Value::Array(_)) {
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
            scope.push();
            if let Some(ref key_var) = block.key_var {
                scope.set_var(key_var, Value::Null);
            }
            scope.set_var(&block.var, Value::Null);
            let result = validate(&block.body, scope, file, source);
            let _ = scope.pop(); // Cannot fail — we just pushed
            result
        }
        Node::Define(def) => {
            scope.push();
            for param in &def.params {
                // Use an empty array as the placeholder for each parameter so
                // that `@for item in param:` inside the body passes the type
                // check. The actual type is enforced at call time by the evaluator.
                scope.set_var(param, Value::Array(vec![]));
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
                .ok_or_else(|| MdsError::undefined_var_at(object, file, source, offset, object.len()))
                .map(|_| ())
        }
        Expr::Call { name, args } => {
            let func = scope
                .get_function(name)
                .ok_or_else(|| MdsError::undefined_fn_at(name, file, source, offset, len))?;
            if args.len() != func.params.len() {
                return Err(MdsError::arity_at(
                    name,
                    func.params.len(),
                    args.len(),
                    file,
                    source,
                    offset,
                    len,
                ));
            }
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
            if args.len() != func.params.len() {
                return Err(MdsError::arity_at(
                    &qualified,
                    func.params.len(),
                    args.len(),
                    file,
                    source,
                    offset,
                    len,
                ));
            }
            validate_var_args(args, scope, file, source, offset, 0)
        }
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
    if depth > crate::parser::MAX_NESTING_DEPTH {
        return Err(MdsError::syntax("nested argument validation depth exceeded"));
    }
    for arg in args {
        match arg {
            Arg::StringLiteral(_) => {}
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
                // Validate the nested call as if it were a top-level Expr::Call
                let func = scope.get_function(name).ok_or_else(|| {
                    MdsError::undefined_fn_at(name, file, source, offset, name.len())
                })?;
                if inner_args.len() != func.params.len() {
                    return Err(MdsError::arity_at(
                        name,
                        func.params.len(),
                        inner_args.len(),
                        file,
                        source,
                        offset,
                        name.len(),
                    ));
                }
                validate_var_args(inner_args, scope, file, source, offset, depth + 1)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            params: vec!["name".to_string()],
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
            params: vec!["name".to_string()],
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
