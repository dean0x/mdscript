use std::collections::HashSet;

use crate::ast::*;
use crate::error::MdsError;
use crate::scope::{FunctionDef, Scope};
use crate::value::Value;

/// Maximum call depth to prevent stack overflow from deeply nested calls.
const MAX_CALL_DEPTH: usize = 128;

/// Evaluate a module body into a final rendered string.
pub fn evaluate(nodes: &[Node], scope: &mut Scope) -> Result<String, MdsError> {
    let mut call_stack: HashSet<String> = HashSet::new();
    evaluate_nodes(nodes, scope, &mut call_stack)
}

fn evaluate_nodes(
    nodes: &[Node],
    scope: &mut Scope,
    call_stack: &mut HashSet<String>,
) -> Result<String, MdsError> {
    let mut output = String::new();

    for node in nodes {
        match node {
            Node::Text(t) => {
                output.push_str(&t.text);
            }
            Node::EscapedBrace => {
                output.push('{');
            }
            Node::Interpolation(interp) => {
                let result = evaluate_expr(&interp.expr, scope, call_stack)?;
                output.push_str(&result);
            }
            Node::If(block) => {
                let result = evaluate_if(block, scope, call_stack)?;
                output.push_str(&result);
            }
            Node::For(block) => {
                let result = evaluate_for(block, scope, call_stack)?;
                output.push_str(&result);
            }
            Node::Define(block) => {
                // Register function in scope
                let func = FunctionDef::from(block);
                scope.set_function(&block.name, func);
            }
            Node::Import(_) | Node::Export(_) => {
                // Handled by resolver, skip during evaluation
            }
            Node::Include(inc) => {
                let result = evaluate_include(inc, scope)?;
                output.push_str(&result);
            }
        }
    }

    Ok(output)
}

fn evaluate_expr(
    expr: &Expr,
    scope: &mut Scope,
    call_stack: &mut HashSet<String>,
) -> Result<String, MdsError> {
    match expr {
        Expr::Var(name) => {
            let value = scope
                .get_var(name)
                .ok_or_else(|| MdsError::undefined_var(name))?;
            Ok(value.to_string())
        }
        Expr::Call { name, args } => {
            let resolved_args = resolve_args(args, scope)?;
            call_function(name, &resolved_args, scope, call_stack)
        }
        Expr::QualifiedCall {
            namespace,
            name,
            args,
        } => {
            let resolved_args = resolve_args(args, scope)?;
            call_qualified_function(namespace, name, &resolved_args, scope, call_stack)
        }
    }
}

fn resolve_args(args: &[Arg], scope: &Scope) -> Result<Vec<Value>, MdsError> {
    args.iter()
        .map(|arg| match arg {
            Arg::StringLiteral(s) => Ok(Value::String(s.clone())),
            Arg::Var(name) => scope
                .get_var(name)
                .cloned()
                .ok_or_else(|| MdsError::undefined_var(name)),
        })
        .collect()
}

fn call_function(
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    call_stack: &mut HashSet<String>,
) -> Result<String, MdsError> {
    // Check for recursion
    if call_stack.contains(name) {
        return Err(MdsError::Recursion {
            name: name.to_string(),
        });
    }
    // Guard against excessive call depth (A -> B -> C -> ... )
    if call_stack.len() >= MAX_CALL_DEPTH {
        return Err(MdsError::Recursion {
            name: format!("{name} (call depth exceeds {MAX_CALL_DEPTH})"),
        });
    }

    let func = scope
        .get_function(name)
        .ok_or_else(|| MdsError::undefined_fn(name))?
        .clone();

    // Check arity
    if args.len() != func.params.len() {
        return Err(MdsError::arity(name, func.params.len(), args.len()));
    }

    // Create function scope
    scope.push();
    for (param, value) in func.params.iter().zip(args.iter()) {
        scope.set_var(param, value.clone());
    }

    call_stack.insert(name.to_string());
    let result = evaluate_nodes(&func.body, scope, call_stack);
    call_stack.remove(name);

    scope.pop();
    result
}

fn call_qualified_function(
    namespace: &str,
    name: &str,
    args: &[Value],
    scope: &mut Scope,
    call_stack: &mut HashSet<String>,
) -> Result<String, MdsError> {
    let qualified_name = format!("{namespace}.{name}");

    // Check for recursion
    if call_stack.contains(&qualified_name) {
        return Err(MdsError::Recursion {
            name: qualified_name,
        });
    }
    // Guard against excessive call depth
    if call_stack.len() >= MAX_CALL_DEPTH {
        return Err(MdsError::Recursion {
            name: format!("{qualified_name} (call depth exceeds {MAX_CALL_DEPTH})"),
        });
    }

    let ns = scope
        .get_namespace(namespace)
        .ok_or_else(|| MdsError::undefined_var(namespace))?;

    let func = ns
        .functions
        .get(name)
        .ok_or_else(|| MdsError::undefined_fn(&qualified_name))?
        .clone();

    // Check arity
    if args.len() != func.params.len() {
        return Err(MdsError::arity(
            &qualified_name,
            func.params.len(),
            args.len(),
        ));
    }

    // Create function scope with the namespace's scope visible
    scope.push();
    for (param, value) in func.params.iter().zip(args.iter()) {
        scope.set_var(param, value.clone());
    }

    call_stack.insert(qualified_name.clone());
    let result = evaluate_nodes(&func.body, scope, call_stack);
    call_stack.remove(&qualified_name);

    scope.pop();
    result
}

fn evaluate_if(
    block: &IfBlock,
    scope: &mut Scope,
    call_stack: &mut HashSet<String>,
) -> Result<String, MdsError> {
    let value = scope
        .get_var(&block.condition)
        .ok_or_else(|| MdsError::undefined_var(&block.condition))?;

    if value.is_truthy() {
        evaluate_nodes(&block.then_body, scope, call_stack)
    } else if let Some(else_body) = &block.else_body {
        evaluate_nodes(else_body, scope, call_stack)
    } else {
        Ok(String::new())
    }
}

fn evaluate_for(
    block: &ForBlock,
    scope: &mut Scope,
    call_stack: &mut HashSet<String>,
) -> Result<String, MdsError> {
    let iterable = scope
        .get_var(&block.iterable)
        .ok_or_else(|| MdsError::undefined_var(&block.iterable))?;

    // Validate it's an array before cloning the items.
    let type_name = iterable.type_name();
    let items = iterable
        .as_array()
        .ok_or_else(|| MdsError::type_error(type_name))?
        .clone();

    let mut output = String::new();

    for item in items {
        scope.push();
        scope.set_var(&block.var, item);
        let rendered = evaluate_nodes(&block.body, scope, call_stack)?;
        output.push_str(&rendered);
        scope.pop();
    }

    Ok(output)
}

fn evaluate_include(inc: &IncludeDirective, scope: &Scope) -> Result<String, MdsError> {
    let ns = scope
        .get_namespace(&inc.alias)
        .ok_or_else(|| MdsError::undefined_var(&inc.alias))?;

    if ns.prompt_body.is_none() {
        eprintln!(
            "warning: @include {} produces empty string (module has no body text)",
            inc.alias
        );
    }

    Ok(ns.prompt_body.as_deref().unwrap_or("").to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::TextNode;

    #[test]
    fn evaluate_text() {
        let nodes = vec![Node::Text(TextNode {
            text: "Hello world!".to_string(),
            offset: 0,
        })];
        let mut scope = Scope::new();
        let result = evaluate(&nodes, &mut scope).unwrap();
        assert_eq!(result, "Hello world!");
    }

    #[test]
    fn evaluate_variable_interpolation() {
        let nodes = vec![
            Node::Text(TextNode {
                text: "Hello ".to_string(),
                offset: 0,
            }),
            Node::Interpolation(Interpolation {
                expr: Expr::Var("name".to_string()),
                offset: 6,
                len: 4,
            }),
            Node::Text(TextNode {
                text: "!".to_string(),
                offset: 12,
            }),
        ];
        let mut scope = Scope::new();
        scope.set_var("name", Value::String("Alice".to_string()));
        let result = evaluate(&nodes, &mut scope).unwrap();
        assert_eq!(result, "Hello Alice!");
    }

    #[test]
    fn evaluate_undefined_var_error() {
        let nodes = vec![Node::Interpolation(Interpolation {
            expr: Expr::Var("unknown".to_string()),
            offset: 0,
            len: 7,
        })];
        let mut scope = Scope::new();
        let result = evaluate(&nodes, &mut scope);
        assert!(result.is_err());
    }

    #[test]
    fn evaluate_if_truthy() {
        let nodes = vec![Node::If(IfBlock {
            condition: "flag".to_string(),
            then_body: vec![Node::Text(TextNode {
                text: "yes".to_string(),
                offset: 0,
            })],
            else_body: Some(vec![Node::Text(TextNode {
                text: "no".to_string(),
                offset: 0,
            })]),
            offset: 0,
        })];
        let mut scope = Scope::new();
        scope.set_var("flag", Value::Boolean(true));
        assert_eq!(evaluate(&nodes, &mut scope).unwrap(), "yes");
    }

    #[test]
    fn evaluate_if_falsy() {
        let nodes = vec![Node::If(IfBlock {
            condition: "flag".to_string(),
            then_body: vec![Node::Text(TextNode {
                text: "yes".to_string(),
                offset: 0,
            })],
            else_body: Some(vec![Node::Text(TextNode {
                text: "no".to_string(),
                offset: 0,
            })]),
            offset: 0,
        })];
        let mut scope = Scope::new();
        scope.set_var("flag", Value::Boolean(false));
        assert_eq!(evaluate(&nodes, &mut scope).unwrap(), "no");
    }

    #[test]
    fn evaluate_for_loop() {
        let nodes = vec![Node::For(ForBlock {
            var: "item".to_string(),
            iterable: "items".to_string(),
            body: vec![
                Node::Text(TextNode {
                    text: "- ".to_string(),
                    offset: 0,
                }),
                Node::Interpolation(Interpolation {
                    expr: Expr::Var("item".to_string()),
                    offset: 2,
                    len: 4,
                }),
                Node::Text(TextNode {
                    text: "\n".to_string(),
                    offset: 8,
                }),
            ],
            offset: 0,
        })];
        let mut scope = Scope::new();
        scope.set_var(
            "items",
            Value::Array(vec![
                Value::String("apple".into()),
                Value::String("banana".into()),
            ]),
        );
        let result = evaluate(&nodes, &mut scope).unwrap();
        assert_eq!(result, "- apple\n- banana\n");
    }

    #[test]
    fn evaluate_function_call() {
        let nodes = vec![
            Node::Define(DefineBlock {
                name: "greet".to_string(),
                params: vec!["name".to_string()],
                body: vec![
                    Node::Text(TextNode {
                        text: "Hello ".to_string(),
                        offset: 0,
                    }),
                    Node::Interpolation(Interpolation {
                        expr: Expr::Var("name".to_string()),
                        offset: 6,
                        len: 4,
                    }),
                    Node::Text(TextNode {
                        text: "!".to_string(),
                        offset: 12,
                    }),
                ],
                offset: 0,
            }),
            Node::Interpolation(Interpolation {
                expr: Expr::Call {
                    name: "greet".to_string(),
                    args: vec![Arg::StringLiteral("Bob".to_string())],
                },
                offset: 20,
                len: 12,
            }),
        ];
        let mut scope = Scope::new();
        let result = evaluate(&nodes, &mut scope).unwrap();
        assert_eq!(result, "Hello Bob!");
    }

    #[test]
    fn evaluate_escaped_brace() {
        let nodes = vec![
            Node::Text(TextNode {
                text: "Use ".to_string(),
                offset: 0,
            }),
            Node::EscapedBrace,
            Node::Text(TextNode {
                text: "name} for interpolation".to_string(),
                offset: 6,
            }),
        ];
        let mut scope = Scope::new();
        let result = evaluate(&nodes, &mut scope).unwrap();
        assert_eq!(result, "Use {name} for interpolation");
    }
}
