use crate::ast::*;
use crate::error::MdsError;
use crate::scope::Scope;

/// Validate semantic correctness of a module AST.
/// Checks variable references, function arity, and type constraints
/// before evaluation. Block-scoped variables (e.g., @for loop vars)
/// are verified at evaluation time.
pub fn validate(nodes: &[Node], scope: &Scope) -> Result<(), MdsError> {
    for node in nodes {
        validate_node(node, scope)?;
    }
    Ok(())
}

fn validate_node(node: &Node, scope: &Scope) -> Result<(), MdsError> {
    match node {
        Node::Text(_) | Node::EscapedBrace => Ok(()),
        Node::Interpolation(interp) => validate_expr(&interp.expr, scope),
        Node::If(block) => {
            // Condition must be a variable (truthiness check on a value)
            if scope.get_var(&block.condition).is_none() {
                return Err(MdsError::undefined_var(&block.condition));
            }
            for node in &block.then_body {
                validate_node(node, scope)?;
            }
            if let Some(else_body) = &block.else_body {
                for node in else_body {
                    validate_node(node, scope)?;
                }
            }
            Ok(())
        }
        Node::For(block) => {
            // Iterable must exist
            if scope.get_var(&block.iterable).is_none() {
                return Err(MdsError::undefined_var(&block.iterable));
            }
            // We cannot fully validate the loop body here since the loop var
            // is block-scoped and only available at evaluation time.
            Ok(())
        }
        Node::Define(_) => {
            // Function definitions are validated when called
            Ok(())
        }
        Node::Import(_) | Node::Export(_) => {
            // Handled by resolver
            Ok(())
        }
        Node::Include(inc) => {
            // Verify the referenced namespace exists (must have been @import-ed)
            if scope.get_namespace(&inc.alias).is_none() {
                return Err(MdsError::undefined_var(&inc.alias));
            }
            Ok(())
        }
    }
}

fn validate_expr(expr: &Expr, scope: &Scope) -> Result<(), MdsError> {
    match expr {
        Expr::Var(name) => {
            if scope.get_var(name).is_none() {
                return Err(MdsError::undefined_var(name));
            }
            Ok(())
        }
        Expr::Call { name, args } => {
            match scope.get_function(name) {
                Some(func) => {
                    if args.len() != func.params.len() {
                        return Err(MdsError::arity(name, func.params.len(), args.len()));
                    }
                }
                None => {
                    return Err(MdsError::undefined_fn(name));
                }
            }
            validate_var_args(args, scope)
        }
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => {
            match scope.get_namespace(namespace) {
                Some(ns) => {
                    let qualified = format!("{namespace}.{name}");
                    match ns.functions.get(name) {
                        Some(func) => {
                            if args.len() != func.params.len() {
                                return Err(MdsError::arity(
                                    &qualified,
                                    func.params.len(),
                                    args.len(),
                                ));
                            }
                        }
                        None => {
                            return Err(MdsError::undefined_fn(&qualified));
                        }
                    }
                }
                None => {
                    return Err(MdsError::undefined_var(namespace));
                }
            }
            validate_var_args(args, scope)
        }
    }
}

/// Check that all variable arguments reference defined variables.
fn validate_var_args(args: &[Arg], scope: &Scope) -> Result<(), MdsError> {
    for arg in args {
        if let Arg::Var(var_name) = arg {
            if scope.get_var(var_name).is_none() {
                return Err(MdsError::undefined_var(var_name));
            }
        }
    }
    Ok(())
}
