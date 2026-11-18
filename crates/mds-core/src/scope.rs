use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::{DefineBlock, Param};
use crate::sourcemap::Origin;
use crate::value::Value;

/// Closure captures from the definition site.
///
/// Each capture and link is immutable once shared. A define captures before
/// it enters scope, so the links and functions form a DAG. The call stack
/// check rejects recursion by name.
#[derive(Default)]
pub struct CapturedScope {
    own_vars: Option<Arc<HashMap<String, Slot>>>,
    pub(crate) runtime: Option<Arc<VarLayer>>,
    frontmatter: Option<Arc<VarLayer>>,
    head: Option<Arc<CaptureLink>>,
}

pub(crate) struct CaptureLink {
    functions: HashMap<String, Arc<FunctionDef>>,
    namespaces: HashMap<String, Arc<NamespaceScope>>,
    prev: Option<Arc<CaptureLink>>,
    depth: usize,
}

impl std::fmt::Debug for CapturedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedScope")
            .field("own_vars", &self.own_vars.as_ref().map_or(0, |v| v.len()))
            .field("runtime", &self.runtime.as_ref().map_or(0, |v| v.len()))
            .field(
                "frontmatter",
                &self.frontmatter.as_ref().map_or(0, |v| v.len()),
            )
            .field("links", &self.head.as_ref().map_or(0, |link| link.depth))
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for CaptureLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureLink")
            .field("functions", &self.functions.len())
            .field("namespaces", &self.namespaces.len())
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

impl CapturedScope {
    fn links(&self) -> impl Iterator<Item = &CaptureLink> {
        let head = self.head.as_deref();
        std::iter::successors(head, |link| link.prev.as_deref())
            .take(head.map_or(0, |link| link.depth))
    }

    pub(crate) fn restore_into(&self, scope: &mut Scope) {
        let links: Vec<_> = self.links().collect();
        for link in links.into_iter().rev() {
            for (name, function) in &link.functions {
                scope.set_function(name, Arc::clone(function));
            }
            for (name, namespace) in &link.namespaces {
                scope.set_namespace(name, Arc::clone(namespace));
            }
        }
        for layer in self.frontmatter.iter().chain(self.runtime.iter()) {
            for (name, root) in layer.iter() {
                scope.set_var_shared(name, Arc::clone(root));
            }
        }
        if let Some(vars) = &self.own_vars {
            for (name, slot) in vars.iter() {
                scope.set_var_shared(name, Arc::clone(&slot.root));
            }
        }
    }
}

/// A function definition stored in scope.
#[derive(Debug)]
pub struct FunctionDef {
    pub params: Vec<Param>,
    pub body: Vec<crate::ast::Node>,
    /// Lexical closure captures populated by the resolver at definition time.
    pub captured: CapturedScope,
    /// Source provenance for source-map attribution (S7) and diagnostic span
    /// attribution (B1 — applies PF-012).
    ///
    /// Always populated by the resolver (unconditional since B1 dropped the
    /// `source_map_mode` gate — AC-PERF-01 relaxation for correctness).
    /// Carries the defining module's display path and raw source so that:
    /// - `invoke_function` (S8 path) can switch the `MapBuilder`'s current source to
    ///   the definition file and attribute body-output segments there.
    /// - `build_type_mismatch` can select the correct source when raising a
    ///   span-bearing error inside an imported `@define` body (PF-012).
    ///
    /// `None` only when the `FunctionDef` was constructed outside the resolver
    /// path (e.g. via `FunctionDef::from(&DefineBlock)` in tests).
    pub(crate) origin: Option<Origin>,
}

impl From<&DefineBlock> for FunctionDef {
    fn from(d: &DefineBlock) -> Self {
        FunctionDef {
            params: d.params.clone(),
            body: d.body.clone(),
            captured: CapturedScope::default(),
            origin: None,
        }
    }
}

#[cfg(test)]
pub(crate) fn captured_function_nodes(root: &FunctionDef) -> usize {
    use std::collections::HashSet;
    const MAX_NODES: usize = 1 << 12;
    let mut pending = vec![root];
    let mut functions = HashSet::new();
    let mut links = HashSet::new();
    let mut namespaces = HashSet::new();
    for _ in 0..=MAX_NODES {
        let Some(function) = pending.pop() else {
            return functions.len();
        };
        if !functions.insert(std::ptr::from_ref(function)) {
            continue;
        }
        for link in function.captured.links() {
            if !links.insert(std::ptr::from_ref(link)) {
                break;
            }
            pending.extend(link.functions.values().map(Arc::as_ref));
            for namespace in link.namespaces.values() {
                if namespaces.insert(Arc::as_ptr(namespace)) {
                    pending.extend(namespace.functions.values().map(Arc::as_ref));
                }
            }
        }
    }
    panic!("capture count exceeds the test limit of {MAX_NODES} nodes");
}

#[cfg(test)]
pub(crate) fn captured_entry_count(scope: &Scope) -> usize {
    let capture = CapturedScope {
        head: scope.head.clone(),
        ..CapturedScope::default()
    };
    let frozen: usize = capture
        .links()
        .map(|link| link.functions.len() + link.namespaces.len())
        .sum();
    frozen + scope.pending_functions.len() + scope.pending_namespaces.len()
}

pub(crate) type VarLayer = HashMap<String, Arc<Value>>;

#[derive(Debug, Clone)]
pub(crate) struct Slot {
    root: Arc<Value>,
}

/// A scope chain supporting variable and function lookup with shadowing.
#[derive(Debug, Clone)]
pub struct Scope {
    /// Stack of frames, last is innermost.
    frames: Vec<Frame>,
    runtime: Option<Arc<VarLayer>>,
    frontmatter: Option<Arc<VarLayer>>,
    vars_generation: u64,
    own_vars_snapshot: Option<OwnVarsSnapshot>,
    pending_functions: HashMap<String, Arc<FunctionDef>>,
    pending_namespaces: HashMap<String, Arc<NamespaceScope>>,
    head: Option<Arc<CaptureLink>>,
}

#[derive(Debug, Clone)]
struct OwnVarsSnapshot {
    generation: u64,
    vars: Arc<HashMap<String, Slot>>,
}

#[derive(Debug, Clone, Default)]
struct Frame {
    vars: HashMap<String, Slot>,
    /// Functions stored as `Arc` so cloning out of scope is O(1).
    functions: HashMap<String, Arc<FunctionDef>>,
    namespaces: HashMap<String, Arc<NamespaceScope>>,
}

/// A namespace scope for aliased imports.
#[derive(Debug)]
pub struct NamespaceScope {
    /// Functions stored as `Arc` so cloning out of scope is O(1).
    pub functions: HashMap<String, Arc<FunctionDef>>,
    /// The compiled prompt body of the imported module.
    pub prompt_body: Option<String>,
    /// Pre-computed source-map fragment for the module's `prompt` body.
    ///
    /// Populated when the parent compilation runs in source-map mode and the
    /// module exports a non-empty `prompt`.  `None` in all other cases
    /// (zero-cost: no allocation when source maps are disabled — AC-PERF-01).
    ///
    /// The `Arc` enables O(1) copy into the `NamespaceScope` without cloning
    /// the segment data; all `@include` call sites share one allocation.
    pub(crate) prompt_map: Option<Arc<crate::sourcemap::FragmentMap>>,
    /// `true` when the module has body text but its explicit `@export` list
    /// excludes `"prompt"`.  Lets `evaluate_include` emit a precise WARN-B
    /// ("add `@export prompt`") instead of the generic WARN-A ("no body text").
    ///
    /// `false` when the module genuinely has no body text (WARN-A) or when
    /// prompt is visible (no warning needed).
    pub(crate) prompt_suppressed_by_exports: bool,
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
            runtime: None,
            frontmatter: None,
            vars_generation: 0,
            own_vars_snapshot: None,
            pending_functions: HashMap::new(),
            pending_namespaces: HashMap::new(),
            head: None,
        }
    }

    pub(crate) fn with_layers(runtime: Arc<VarLayer>, frontmatter: Arc<VarLayer>) -> Self {
        Self {
            runtime: Some(runtime),
            frontmatter: Some(frontmatter),
            ..Self::new()
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
        self.set_var_shared(name, Arc::new(value));
    }

    pub(crate) fn set_var_shared(&mut self, name: &str, root: Arc<Value>) {
        if self.frames.len() == 1 {
            self.vars_generation = self.vars_generation.wrapping_add(1);
            if self.vars_generation == 0 {
                self.own_vars_snapshot = None;
            }
        }
        self.frames
            .last_mut()
            .expect("BUG: scope has no frames")
            .vars
            .insert(name.to_string(), Slot { root });
    }

    /// Look up a variable by walking the scope chain (innermost first).
    pub fn get_var(&self, name: &str) -> Option<&Value> {
        self.get_slot(name).map(Arc::as_ref)
    }

    pub(crate) fn get_slot(&self, name: &str) -> Option<&Arc<Value>> {
        self.frames
            .iter()
            .rev()
            .find_map(|f| f.vars.get(name).map(|slot| &slot.root))
            .or_else(|| self.runtime.as_ref()?.get(name))
            .or_else(|| self.frontmatter.as_ref()?.get(name))
    }

    /// Define a function in the current frame.
    pub fn set_function(&mut self, name: &str, func: Arc<FunctionDef>) {
        if self.frames.len() == 1 {
            self.pending_functions
                .insert(name.to_string(), Arc::clone(&func));
        }
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
    pub fn set_namespace(&mut self, alias: &str, ns: impl Into<Arc<NamespaceScope>>) {
        let ns = ns.into();
        if self.frames.len() == 1 {
            self.pending_namespaces
                .insert(alias.to_string(), Arc::clone(&ns));
        }
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
            .find_map(|f| f.namespaces.get(alias).map(Arc::as_ref))
    }

    /// Freeze the frame-zero writes since the last capture.
    pub(crate) fn capture(&mut self) -> Result<CapturedScope, crate::error::MdsError> {
        let Some(frame) = self.frames.first().filter(|_| self.frames.len() == 1) else {
            return Err(crate::error::MdsError::syntax(
                "internal error: cannot capture outside the global scope frame — this is a compiler bug, please report it",
            ));
        };
        if !self.pending_functions.is_empty() || !self.pending_namespaces.is_empty() {
            let depth = self.head.as_ref().map_or(1, |link| link.depth + 1);
            self.head = Some(Arc::new(CaptureLink {
                functions: std::mem::take(&mut self.pending_functions),
                namespaces: std::mem::take(&mut self.pending_namespaces),
                prev: self.head.take(),
                depth,
            }));
        }
        let own_vars = match &self.own_vars_snapshot {
            Some(snapshot) if snapshot.generation == self.vars_generation => {
                Some(Arc::clone(&snapshot.vars))
            }
            _ if frame.vars.is_empty() => None,
            _ => {
                let vars = Arc::new(frame.vars.clone());
                self.own_vars_snapshot = Some(OwnVarsSnapshot {
                    generation: self.vars_generation,
                    vars: Arc::clone(&vars),
                });
                Some(vars)
            }
        };
        Ok(CapturedScope {
            own_vars,
            runtime: self.runtime.clone(),
            frontmatter: self.frontmatter.clone(),
            head: self.head.clone(),
        })
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
                params: vec![crate::ast::Param::required("name")],
                body: vec![],
                captured: CapturedScope::default(),
                origin: None,
            }),
        );
        assert!(scope.get_function("greet").is_some());
        assert!(scope.get_function("unknown").is_none());
    }

    #[test]
    fn captured_scope_default() {
        let cs = CapturedScope::default();
        assert!(cs.links().next().is_none());
        assert!(cs.own_vars.is_none());
        assert!(cs.runtime.is_none());
        assert!(cs.frontmatter.is_none());
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
            prompt_map: None,
            prompt_suppressed_by_exports: false,
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
                prompt_map: None,
                prompt_suppressed_by_exports: false,
            },
        );
        scope.push();
        scope.set_namespace(
            "lib",
            NamespaceScope {
                functions: HashMap::new(),
                prompt_body: Some("inner".into()),
                prompt_map: None,
                prompt_suppressed_by_exports: false,
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

    #[test]
    fn capture_restores_vars_in_layer_order() {
        let mut scope = Scope::with_layers(
            Arc::new(HashMap::from([
                ("b".into(), Arc::new(Value::Number(2.0))),
                ("c".into(), Arc::new(Value::Number(2.0))),
            ])),
            Arc::new(HashMap::from([
                ("a".into(), Arc::new(Value::Number(1.0))),
                ("b".into(), Arc::new(Value::Number(1.0))),
                ("c".into(), Arc::new(Value::Number(1.0))),
            ])),
        );
        scope.set_var("c", Value::Number(3.0));
        let capture = scope.capture().unwrap();
        let mut call = Scope::new();
        call.push();
        capture.restore_into(&mut call);
        assert_eq!(call.get_var("a"), Some(&Value::Number(1.0)));
        assert_eq!(call.get_var("b"), Some(&Value::Number(2.0)));
        assert_eq!(call.get_var("c"), Some(&Value::Number(3.0)));
    }

    #[test]
    fn capture_restores_namespaces_from_old_and_new_links() {
        let mut scope = Scope::new();
        let old = Arc::new(make_ns());
        let new = Arc::new(make_ns());
        scope.set_namespace("a", Arc::clone(&old));
        scope.set_namespace("b", Arc::clone(&old));
        scope.capture().unwrap();
        scope.set_namespace("b", Arc::clone(&new));
        let mut call = Scope::new();
        call.push();
        scope.capture().unwrap().restore_into(&mut call);
        assert!(std::ptr::eq(call.get_namespace("a").unwrap(), old.as_ref()));
        assert!(std::ptr::eq(call.get_namespace("b").unwrap(), new.as_ref()));
    }

    #[test]
    fn capture_refuses_nested_frames() {
        let mut scope = Scope::new();
        scope.push();
        let error = scope.capture().unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot capture outside the global scope frame"));
        scope.pop().unwrap();
        assert!(scope.capture().is_ok());
    }

    fn define(scope: &mut Scope, name: &str) -> Arc<FunctionDef> {
        let function = Arc::new(FunctionDef {
            params: vec![],
            body: vec![],
            captured: scope.capture().unwrap(),
            origin: None,
        });
        scope.set_function(name, Arc::clone(&function));
        function
    }

    #[test]
    fn captures_without_writes_reuse_the_head() {
        let mut scope = Scope::new();
        assert!(scope.capture().unwrap().head.is_none());
        assert!(scope.capture().unwrap().head.is_none());

        let a = define(&mut scope, "a");
        assert!(a.captured.head.is_none());
        let b = define(&mut scope, "b");
        let head = b.captured.head.as_ref().unwrap();
        assert_eq!(head.functions.len(), 1);
        assert!(Arc::ptr_eq(&head.functions["a"], &a));
        assert!(head.prev.is_none());

        let first = scope.capture().unwrap();
        let second = scope.capture().unwrap();
        assert!(Arc::ptr_eq(
            first.head.as_ref().unwrap(),
            second.head.as_ref().unwrap()
        ));
    }

    #[test]
    fn capture_freezes_sibling_and_import_and_reuses_unchanged_vars() {
        let mut scope = Scope::new();
        scope.set_var("prompt", Value::String("text".into()));
        let imported = define(&mut Scope::new(), "imported");
        scope.set_function("earlier", Arc::clone(&imported));
        let a = define(&mut scope, "a");
        scope.set_function("imported", Arc::clone(&imported));
        let b = define(&mut scope, "b");

        let head = b.captured.head.as_ref().unwrap();
        assert!(Arc::ptr_eq(
            head.prev.as_ref().unwrap(),
            a.captured.head.as_ref().unwrap()
        ));
        assert_eq!(head.functions.len(), 2);
        assert!(head.namespaces.is_empty());
        assert!(Arc::ptr_eq(&head.functions["a"], &a));
        assert!(Arc::ptr_eq(&head.functions["imported"], &imported));
        assert!(Arc::ptr_eq(
            a.captured.own_vars.as_ref().unwrap(),
            b.captured.own_vars.as_ref().unwrap()
        ));
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
