use std::collections::HashMap;

use crate::ast::DefineBlock;
use crate::value::Value;

/// A function definition stored in scope.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub params: Vec<String>,
    pub body: Vec<crate::ast::Node>,
    /// Namespaces captured from the function's definition site (lexical scope).
    pub captured_namespaces: HashMap<String, NamespaceScope>,
    /// Functions captured from the function's definition site (lexical scope).
    pub captured_functions: HashMap<String, FunctionDef>,
    /// Variables captured from the function's definition site (lexical scope).
    pub captured_vars: HashMap<String, Value>,
}

impl From<&DefineBlock> for FunctionDef {
    fn from(d: &DefineBlock) -> Self {
        FunctionDef {
            params: d.params.clone(),
            body: d.body.clone(),
            captured_namespaces: HashMap::new(),
            captured_functions: HashMap::new(),
            captured_vars: HashMap::new(),
        }
    }
}

/// A scope chain supporting variable and function lookup with shadowing.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Stack of frames, last is innermost.
    frames: Vec<Frame>,
}

#[derive(Debug, Clone, Default)]
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
            frames: vec![Frame::default()],
        }
    }

    /// Push a new scope frame (for blocks, function calls).
    pub fn push(&mut self) {
        self.frames.push(Frame::default());
    }

    /// Pop the innermost scope frame.
    pub fn pop(&mut self) {
        debug_assert!(self.frames.len() > 1, "cannot pop the global scope frame");
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    /// Set a variable in the current (innermost) frame.
    pub fn set_var(&mut self, name: &str, value: Value) {
        self.frames
            .last_mut()
            .unwrap()
            .vars
            .insert(name.to_string(), value);
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
        self.frames
            .last_mut()
            .unwrap()
            .functions
            .insert(name.to_string(), func);
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
        self.frames
            .last_mut()
            .unwrap()
            .namespaces
            .insert(alias.to_string(), ns);
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

    /// Get all namespaces visible in the current scope (for closure capture).
    /// Outer-to-inner iteration: inner frames shadow outer frames.
    pub fn get_all_namespaces(&self) -> HashMap<String, NamespaceScope> {
        self.frames
            .iter()
            .flat_map(|f| f.namespaces.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect()
    }

    /// Get all functions visible in the current scope (for closure capture).
    /// Outer-to-inner iteration: inner frames shadow outer frames.
    pub fn get_all_functions(&self) -> HashMap<String, FunctionDef> {
        self.frames
            .iter()
            .flat_map(|f| f.functions.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect()
    }

    /// Get all variables visible in the current scope (for closure capture).
    /// Outer-to-inner iteration: inner frames shadow outer frames.
    pub fn get_all_vars(&self) -> HashMap<String, Value> {
        self.frames
            .iter()
            .flat_map(|f| f.vars.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect()
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
                params: vec!["name".into()],
                body: vec![],
                captured_namespaces: HashMap::new(),
                captured_functions: HashMap::new(),
                captured_vars: HashMap::new(),
            },
        );
        assert!(scope.get_function("greet").is_some());
        assert!(scope.get_function("unknown").is_none());
    }
}
