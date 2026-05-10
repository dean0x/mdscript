use std::collections::HashMap;

use crate::ast::DefineBlock;
use crate::value::Value;

/// A function definition stored in scope.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<crate::ast::Node>,
}

impl From<&DefineBlock> for FunctionDef {
    fn from(d: &DefineBlock) -> Self {
        FunctionDef {
            name: d.name.clone(),
            params: d.params.clone(),
            body: d.body.clone(),
        }
    }
}

/// A scope chain supporting variable and function lookup with shadowing.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Stack of frames, last is innermost.
    frames: Vec<Frame>,
}

#[derive(Debug, Clone)]
struct Frame {
    vars: HashMap<String, Value>,
    functions: HashMap<String, FunctionDef>,
    /// Namespace imports: alias -> (functions, vars)
    namespaces: HashMap<String, NamespaceScope>,
}

/// A namespace scope for aliased imports.
#[derive(Debug, Clone)]
pub struct NamespaceScope {
    pub functions: HashMap<String, FunctionDef>,
    pub vars: HashMap<String, Value>,
    /// The compiled prompt body of the imported module.
    pub prompt_body: Option<String>,
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Scope {
    pub fn new() -> Self {
        Scope {
            frames: vec![Frame {
                vars: HashMap::new(),
                functions: HashMap::new(),
                namespaces: HashMap::new(),
            }],
        }
    }

    /// Push a new scope frame (for blocks, function calls).
    pub fn push(&mut self) {
        self.frames.push(Frame {
            vars: HashMap::new(),
            functions: HashMap::new(),
            namespaces: HashMap::new(),
        });
    }

    /// Pop the innermost scope frame.
    pub fn pop(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// Set a variable in the current (innermost) frame.
    pub fn set_var(&mut self, name: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.vars.insert(name.to_string(), value);
        }
    }

    /// Look up a variable by walking the scope chain (innermost first).
    pub fn get_var(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.vars.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// Define a function in the current frame.
    pub fn set_function(&mut self, name: &str, func: FunctionDef) {
        if let Some(frame) = self.frames.last_mut() {
            frame.functions.insert(name.to_string(), func);
        }
    }

    /// Look up a function by walking the scope chain.
    pub fn get_function(&self, name: &str) -> Option<&FunctionDef> {
        for frame in self.frames.iter().rev() {
            if let Some(f) = frame.functions.get(name) {
                return Some(f);
            }
        }
        None
    }

    /// Register a namespace (for aliased imports).
    pub fn set_namespace(&mut self, alias: &str, ns: NamespaceScope) {
        if let Some(frame) = self.frames.last_mut() {
            frame.namespaces.insert(alias.to_string(), ns);
        }
    }

    /// Look up a namespace by alias.
    pub fn get_namespace(&self, alias: &str) -> Option<&NamespaceScope> {
        for frame in self.frames.iter().rev() {
            if let Some(ns) = frame.namespaces.get(alias) {
                return Some(ns);
            }
        }
        None
    }

    /// Check if an identifier exists in any form (var, function, namespace).
    pub fn has(&self, name: &str) -> bool {
        self.get_var(name).is_some()
            || self.get_function(name).is_some()
            || self.get_namespace(name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_var_shadowing() {
        let mut scope = Scope::new();
        scope.set_var("x", Value::Number(1.0));
        scope.push();
        scope.set_var("x", Value::Number(2.0));
        assert_eq!(scope.get_var("x"), Some(&Value::Number(2.0)));
        scope.pop();
        assert_eq!(scope.get_var("x"), Some(&Value::Number(1.0)));
    }

    #[test]
    fn scope_function_lookup() {
        let mut scope = Scope::new();
        scope.set_function(
            "greet",
            FunctionDef {
                name: "greet".into(),
                params: vec!["name".into()],
                body: vec![],
            },
        );
        assert!(scope.get_function("greet").is_some());
        assert!(scope.get_function("unknown").is_none());
    }
}
