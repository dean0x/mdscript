use crate::ast::{Arg, Expr, Node};
use crate::error::MdsError;
use crate::scope::Scope;
use crate::value::Value;

/// Validate semantic correctness of a module AST.
/// Checks variable references, function arity, and type constraints
/// before evaluation. Block-scoped variables (e.g., @for loop vars)
/// are verified at evaluation time.
pub fn validate(nodes: &[Node], scope: &Scope, file: &str, source: &str) -> Result<(), MdsError> {
    for node in nodes {
        validate_node(node, scope, file, source)?;
    }
    Ok(())
}

fn validate_node(node: &Node, scope: &Scope, file: &str, source: &str) -> Result<(), MdsError> {
    match node {
        Node::Text(_) | Node::EscapedBrace => Ok(()),
        Node::Interpolation(interp) => {
            validate_expr(&interp.expr, scope, file, source, interp.offset, interp.len)
        }
        Node::If(block) => {
            // Condition must be a defined variable (truthiness is checked at evaluation time)
            scope.get_var(&block.condition).ok_or_else(|| {
                MdsError::undefined_var_at(
                    &block.condition,
                    file,
                    source,
                    block.offset,
                    block.condition.len(),
                )
            })?;
            validate(&block.then_body, scope, file, source)?;
            if let Some(else_body) = &block.else_body {
                validate(else_body, scope, file, source)?;
            }
            Ok(())
        }
        Node::For(block) => {
            let iterable_val = scope.get_var(&block.iterable).ok_or_else(|| {
                MdsError::undefined_var_at(
                    &block.iterable,
                    file,
                    source,
                    block.offset,
                    block.iterable.len(),
                )
            })?;
            if !matches!(iterable_val, Value::Array(_)) {
                return Err(MdsError::type_error_at(
                    iterable_val.type_name(),
                    file,
                    source,
                    block.offset,
                    block.iterable.len(),
                ));
            }
            let mut inner = scope.clone();
            inner.set_var(&block.var, Value::Null);
            validate(&block.body, &inner, file, source)
        }
        Node::Define(def) => {
            let mut inner = scope.clone();
            for param in &def.params {
                // Use an empty array as the placeholder for each parameter so
                // that `@for item in param:` inside the body passes the type
                // check. The actual type is enforced at call time by the evaluator.
                inner.set_var(param, Value::Array(vec![]));
            }
            validate(&def.body, &inner, file, source)
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
    if depth > 256 {
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
        let scope = Scope::new();
        let result = validate(&[define], &scope, "test.mds", "");
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
        let scope = Scope::new();
        let result = validate(&[define], &scope, "test.mds", "");
        assert!(
            result.is_ok(),
            "param reference inside @define must pass: {result:?}"
        );
    }
}
