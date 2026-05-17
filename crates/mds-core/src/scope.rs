use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::DefineBlock;
use crate::value::Value;

/// Closure captures bundled at function definition time.
///
/// `functions` is owned (not `Arc`) to avoid reference cycles: if function A
/// captures function B and B captures A, using `Arc<FunctionDef>` inside the
/// capture would create a cycle that leaks memory. Owned values break the cycle.
#[derive(Debug, Clone, Default)]
pub struct CapturedScope {
    /// Namespace aliases captured from the function's definition site.
    pub namespaces: HashMap<String, NamespaceScope>,
    /// Functions captured from the function's definition site (owned — avoids reference cycles).
    pub functions: HashMap<String, FunctionDef>,
    /// Variables captured from the function's definition site.
    pub vars: HashMap<String, Value>,
}

/// A function definition stored in scope.
#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub params: Vec<String>,
    pub body: Vec<crate::ast::Node>,
    /// Lexical closure captures populated by the resolver at definition time.
    pub captured: CapturedScope,
}

impl From<&DefineBlock> for FunctionDef {
    fn from(d: &DefineBlock) -> Self {
        FunctionDef {
            params: d.params.clone(),
            body: d.body.clone(),
            captured: CapturedScope::default(),
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
    /// Functions stored as `Arc` so cloning out of scope is O(1).
    functions: HashMap<String, Arc<FunctionDef>>,
    /// Namespace imports: alias -> (functions, vars)
    namespaces: HashMap<String, NamespaceScope>,
}

/// A namespace scope for aliased imports.
#[derive(Debug, Clone)]
pub struct NamespaceScope {
    /// Functions stored as `Arc` so cloning out of scope is O(1).
    pub functions: HashMap<String, Arc<FunctionDef>>,
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
    ///
    /// Returns an error if called when only the global scope frame remains.
    pub fn pop(&mut self) -> Result<(), crate::error::MdsError> {
        if self.frames.len() <= 1 {
            return Err(crate::error::MdsError::syntax(
                "internal error: cannot pop the global scope frame — this is a compiler bug, please report it",
            ));
        }
        self.frames.pop();
        Ok(())
    }

    /// Set a variable in the current (innermost) frame.
    ///
    /// The `expect` here is sound: `Scope::new()` always pushes one frame (the global
    /// frame) and `pop()` refuses to remove the last remaining frame, so `frames` is
    /// never empty. Converting to `Result` would add unwrap noise at every call site
    /// for an invariant that is structurally guaranteed by private fields.
    pub fn set_var(&mut self, name: &str, value: Value) {
        self.frames
            .last_mut()
            .expect("BUG: scope has no frames")
            .vars
            .insert(name.to_string(), value);
    }

    /// Look up a variable by walking the scope chain (innermost first).
    pub fn get_var(&self, name: &str) -> Option<&Value> {
        self.frames.iter().rev().find_map(|f| f.vars.get(name))
    }

    /// Define a function in the current frame.
    pub fn set_function(&mut self, name: &str, func: Arc<FunctionDef>) {
        self.frames
            .last_mut()
            .expect("BUG: scope has no frames")
            .functions
            .insert(name.to_string(), func);
    }

    /// Look up a function by walking the scope chain.
    pub fn get_function(&self, name: &str) -> Option<&Arc<FunctionDef>> {
        self.frames.iter().rev().find_map(|f| f.functions.get(name))
    }

    /// Register a namespace (for aliased imports).
    pub fn set_namespace(&mut self, alias: &str, ns: NamespaceScope) {
        self.frames
            .last_mut()
            .expect("BUG: scope has no frames")
            .namespaces
            .insert(alias.to_string(), ns);
    }

    /// Look up a namespace by alias.
    pub fn get_namespace(&self, alias: &str) -> Option<&NamespaceScope> {
        self.frames
            .iter()
            .rev()
            .find_map(|f| f.namespaces.get(alias))
    }

    /// Get all namespaces visible in the current scope (for closure capture).
    /// Iterates outer→inner so duplicate keys are overwritten by inner frames,
    /// preserving correct shadowing semantics.
    pub fn get_all_namespaces(&self) -> HashMap<String, NamespaceScope> {
        self.collect_all(|f| &f.namespaces)
    }

    /// Get all functions visible in the current scope (for closure capture).
    /// Iterates outer→inner so duplicate keys are overwritten by inner frames,
    /// preserving correct shadowing semantics.
    ///
    /// Returns `Arc<FunctionDef>` values — cloning is O(1).
    pub fn get_all_functions(&self) -> HashMap<String, Arc<FunctionDef>> {
        self.collect_all(|f| &f.functions)
    }

    /// Get all variables visible in the current scope (for closure capture).
    /// Iterates outer→inner so duplicate keys are overwritten by inner frames,
    /// preserving correct shadowing semantics.
    pub fn get_all_vars(&self) -> HashMap<String, Value> {
        self.collect_all(|f| &f.vars)
    }

    /// Flatten all scope frames outer→inner into a single map.
    /// Duplicate keys are overwritten by inner frames, preserving shadowing semantics.
    fn collect_all<T: Clone>(
        &self,
        get: impl Fn(&Frame) -> &HashMap<String, T>,
    ) -> HashMap<String, T> {
        self.frames
            .iter()
            .flat_map(|f| get(f).iter().map(|(k, v)| (k.clone(), v.clone())))
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
        scope.pop().unwrap();
        assert_eq!(scope.get_var("x"), Some(&Value::Number(1.0)));
    }

    #[test]
    fn scope_function_lookup() {
        let mut scope = Scope::new();
        scope.set_function(
            "greet",
            Arc::new(FunctionDef {
                params: vec!["name".into()],
                body: vec![],
                captured: CapturedScope::default(),
            }),
        );
        assert!(scope.get_function("greet").is_some());
        assert!(scope.get_function("unknown").is_none());
    }

    #[test]
    fn captured_scope_default() {
        let cs = CapturedScope::default();
        assert!(cs.namespaces.is_empty());
        assert!(cs.functions.is_empty());
        assert!(cs.vars.is_empty());
    }

    // ── pop() on the last (global) frame returns an error ────────────────────

    #[test]
    fn scope_pop_last_frame_errors() {
        let mut scope = Scope::new();
        let result = scope.pop();
        assert!(result.is_err(), "popping the global frame must return Err");
    }

    #[test]
    fn scope_pop_nested_frame_succeeds() {
        let mut scope = Scope::new();
        scope.push();
        assert!(
            scope.pop().is_ok(),
            "popping a non-global frame should succeed"
        );
        // Back to one frame — next pop must fail again.
        assert!(scope.pop().is_err());
    }

    // ── Namespace set / get ──────────────────────────────────────────────────

    fn make_ns() -> NamespaceScope {
        NamespaceScope {
            functions: HashMap::new(),
            prompt_body: None,
        }
    }

    #[test]
    fn scope_namespace_set_and_get() {
        let mut scope = Scope::new();
        scope.set_namespace("utils", make_ns());
        assert!(scope.get_namespace("utils").is_some());
        assert!(scope.get_namespace("missing").is_none());
    }

    #[test]
    fn scope_namespace_shadowing_across_frames() {
        let mut scope = Scope::new();
        scope.set_namespace(
            "lib",
            NamespaceScope {
                functions: HashMap::new(),
                prompt_body: Some("outer".into()),
            },
        );
        scope.push();
        scope.set_namespace(
            "lib",
            NamespaceScope {
                functions: HashMap::new(),
                prompt_body: Some("inner".into()),
            },
        );
        // Inner frame shadows outer.
        assert_eq!(
            scope
                .get_namespace("lib")
                .and_then(|ns| ns.prompt_body.as_deref()),
            Some("inner"),
        );
        scope.pop().unwrap();
        // Outer restored.
        assert_eq!(
            scope
                .get_namespace("lib")
                .and_then(|ns| ns.prompt_body.as_deref()),
            Some("outer"),
        );
    }

    // ── collect_all / get_all_* correctness ──────────────────────────────────

    #[test]
    fn get_all_vars_collects_all_frames() {
        let mut scope = Scope::new();
        scope.set_var("a", Value::Number(1.0));
        scope.push();
        scope.set_var("b", Value::Number(2.0));
        let all = scope.get_all_vars();
        assert!(all.contains_key("a"), "outer var visible");
        assert!(all.contains_key("b"), "inner var visible");
    }

    #[test]
    fn get_all_vars_inner_shadows_outer() {
        let mut scope = Scope::new();
        scope.set_var("x", Value::Number(1.0));
        scope.push();
        scope.set_var("x", Value::Number(99.0));
        let all = scope.get_all_vars();
        assert_eq!(
            all.get("x"),
            Some(&Value::Number(99.0)),
            "inner value should win",
        );
    }

    #[test]
    fn get_all_namespaces_collects_across_frames() {
        let mut scope = Scope::new();
        scope.set_namespace("a", make_ns());
        scope.push();
        scope.set_namespace("b", make_ns());
        let all = scope.get_all_namespaces();
        assert!(all.contains_key("a"));
        assert!(all.contains_key("b"));
    }

    // ── Variable not visible after pop ───────────────────────────────────────

    #[test]
    fn scope_var_shadowing_restored_after_pop() {
        let mut scope = Scope::new();
        scope.set_var("val", Value::Number(10.0));
        scope.push();
        scope.set_var("val", Value::Number(20.0));
        assert_eq!(scope.get_var("val"), Some(&Value::Number(20.0)));
        scope.pop().unwrap();
        assert_eq!(scope.get_var("val"), Some(&Value::Number(10.0)));
    }
}
