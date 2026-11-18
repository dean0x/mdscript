use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{DefineBlock, Param};
use crate::limits::MAX_DEFINES_PER_MODULE;
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

enum CaptureEdge {
    Link(Arc<CaptureLink>),
    Function(Arc<FunctionDef>),
    Namespace(Arc<NamespaceScope>),
}

impl CaptureLink {
    fn take_edges(&mut self, pending: &mut Vec<CaptureEdge>) {
        pending.extend(self.prev.take().map(CaptureEdge::Link));
        pending.extend(
            std::mem::take(&mut self.functions)
                .into_values()
                .map(CaptureEdge::Function),
        );
        pending.extend(
            std::mem::take(&mut self.namespaces)
                .into_values()
                .map(CaptureEdge::Namespace),
        );
    }
}

fn drop_capture_edges(mut pending: Vec<CaptureEdge>) {
    // Each pass consumes one edge. Only the last owner can add a node's edges,
    // so the number of reachable edges bounds the work.
    while let Some(edge) = pending.pop() {
        match edge {
            CaptureEdge::Link(link) => {
                if let Some(mut link) = Arc::into_inner(link) {
                    link.take_edges(&mut pending);
                }
            }
            CaptureEdge::Function(function) => {
                if let Some(mut function) = Arc::into_inner(function) {
                    pending.extend(function.captured.head.take().map(CaptureEdge::Link));
                }
            }
            CaptureEdge::Namespace(namespace) => {
                if let Some(namespace) = Arc::into_inner(namespace) {
                    pending.extend(namespace.functions.into_values().map(CaptureEdge::Function));
                }
            }
        }
    }
}

impl Drop for CapturedScope {
    fn drop(&mut self) {
        if let Some(head) = self.head.take() {
            drop_capture_edges(vec![CaptureEdge::Link(head)]);
        }
    }
}

impl Drop for CaptureLink {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        self.take_edges(&mut pending);
        drop_capture_edges(pending);
    }
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
    #[cfg(test)]
    pub(crate) fn weak_head(&self) -> Option<std::sync::Weak<CaptureLink>> {
        self.head.as_ref().map(Arc::downgrade)
    }

    #[cfg(test)]
    pub(crate) fn weak_namespace(&self, alias: &str) -> Option<std::sync::Weak<NamespaceScope>> {
        self.links()
            .find_map(|link| link.namespaces.get(alias))
            .map(Arc::downgrade)
    }

    fn links(&self) -> impl Iterator<Item = &CaptureLink> {
        let head = self.head.as_deref();
        std::iter::successors(head, |link| link.prev.as_deref())
            .take(head.map_or(0, |link| link.depth))
    }

    fn get_slot(&self, name: &str) -> Option<&Slot> {
        self.own_vars
            .as_ref()
            .and_then(|vars| vars.get(name))
            .or_else(|| self.runtime.as_ref()?.get(name))
            .or_else(|| self.frontmatter.as_ref()?.get(name))
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
    const MAX_STEPS: usize = 1 << 19;
    let mut pending = vec![root];
    let mut functions = HashSet::new();
    let mut links = HashSet::new();
    let mut namespaces = HashSet::new();
    for _ in 0..=MAX_STEPS {
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
    panic!("capture count exceeds the test limit of {MAX_STEPS} worklist steps");
}

#[cfg(test)]
pub(crate) fn captured_entry_count(scope: &Scope) -> usize {
    let mut capture = CapturedScope::default();
    capture.head = scope.head.clone();
    let frozen: usize = capture
        .links()
        .map(|link| link.functions.len() + link.namespaces.len())
        .sum();
    frozen + scope.pending_functions.len() + scope.pending_namespaces.len()
}

#[derive(Debug, Default)]
pub(crate) struct VarLayer(HashMap<String, Slot>);

impl VarLayer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, name: String, root: Arc<Value>) {
        self.0.insert(name, Slot::from_root(root));
    }

    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    fn get(&self, name: &str) -> Option<&Slot> {
        self.0.get(name)
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}

impl FromIterator<(String, Arc<Value>)> for VarLayer {
    fn from_iter<T: IntoIterator<Item = (String, Arc<Value>)>>(iter: T) -> Self {
        Self(
            iter.into_iter()
                .map(|(name, root)| (name, Slot::from_root(root)))
                .collect(),
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Slot {
    root: Arc<Value>,
    path: Option<Arc<PathNode>>,
}

pub(crate) enum PathStep {
    Index(usize),
    Key(String),
}

struct PathNode {
    step: PathStep,
    depth: usize,
    parent: Option<Arc<Self>>,
}

impl std::fmt::Debug for PathNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathNode")
            .field("depth", &self.depth)
            .finish()
    }
}

impl From<Value> for Slot {
    fn from(value: Value) -> Self {
        Self::from_root(Arc::new(value))
    }
}

impl Slot {
    fn from_root(root: Arc<Value>) -> Self {
        Self { root, path: None }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Arc<Value> {
        &self.root
    }

    // The caller checks that the step reaches a child of the current value.
    // Each node then consumes one level of the root, which bounds drop depth.
    pub(crate) fn project(&self, step: PathStep) -> Self {
        Self {
            root: Arc::clone(&self.root),
            path: Some(Arc::new(PathNode {
                step,
                depth: self.path.as_ref().map_or(1, |node| node.depth + 1),
                parent: self.path.clone(),
            })),
        }
    }

    pub(crate) fn get(&self) -> Option<&Value> {
        let Some(head) = self.path.as_deref() else {
            return Some(&self.root);
        };
        let mut steps = Vec::with_capacity(head.depth);
        steps.extend(
            std::iter::successors(Some(head), |node| node.parent.as_deref())
                .take(head.depth)
                .map(|node| &node.step),
        );
        steps
            .into_iter()
            .rev()
            .try_fold(self.root.as_ref(), |value, step| match (value, step) {
                (Value::Array(items), PathStep::Index(i)) => items.get(*i),
                (Value::Object(fields), PathStep::Key(key)) => fields.get(key),
                _ => None,
            })
    }

    pub(crate) fn into_value(self) -> Option<Value> {
        if self.path.is_none() {
            return Some(Arc::unwrap_or_clone(self.root));
        }
        self.get().cloned()
    }
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
    callee: Option<Arc<FunctionDef>>,
}

impl Frame {
    fn capture(&self) -> Option<&CapturedScope> {
        self.callee.as_deref().map(|function| &function.captured)
    }
}

/// A module's prompt text, shared with its namespace and imported variables.
#[derive(Clone)]
pub(crate) struct PromptText(Arc<Value>);

impl From<String> for PromptText {
    fn from(text: String) -> Self {
        Self(Arc::new(Value::String(text)))
    }
}

impl PromptText {
    pub(crate) fn shared_value(&self) -> Arc<Value> {
        Arc::clone(&self.0)
    }
}

impl std::ops::Deref for PromptText {
    type Target = str;

    fn deref(&self) -> &str {
        match self.0.as_ref() {
            Value::String(text) => text,
            _ => unreachable!("PromptText contains only a string"),
        }
    }
}

impl std::fmt::Debug for PromptText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptText")
            .field("len", &self.len())
            .finish()
    }
}

/// A namespace scope for aliased imports.
#[derive(Debug)]
pub struct NamespaceScope {
    /// Functions stored as `Arc` so cloning out of scope is O(1).
    pub functions: HashMap<String, Arc<FunctionDef>>,
    /// The compiled prompt body of the imported module.
    pub(crate) prompt_body: Option<PromptText>,
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

    /// Push a new scope frame for a block.
    pub fn push(&mut self) {
        self.frames.push(Frame::default());
    }

    pub(crate) fn push_call(&mut self, callee: &Arc<FunctionDef>) {
        self.frames.push(Frame {
            callee: Some(Arc::clone(callee)),
            ..Frame::default()
        });
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
        self.set_slot(name, Slot::from_root(root));
    }

    pub(crate) fn set_slot(&mut self, name: &str, slot: Slot) {
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
            .insert(name.to_string(), slot);
    }

    /// Look up a variable by walking the scope chain (innermost first).
    pub fn get_var(&self, name: &str) -> Option<&Value> {
        self.get_slot(name).and_then(Slot::get)
    }

    pub(crate) fn get_slot(&self, name: &str) -> Option<&Slot> {
        self.frames
            .iter()
            .rev()
            .find_map(|frame| {
                frame
                    .vars
                    .get(name)
                    .or_else(|| frame.capture()?.get_slot(name))
            })
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
        self.find_binding(
            |frame| frame.functions.get(name),
            |link| {
                #[cfg(test)]
                if name == "upper" {
                    tests::note_builtin_probe();
                }
                link.functions.get(name)
            },
        )
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
        self.find_binding(
            |frame| frame.namespaces.get(alias),
            |link| link.namespaces.get(alias),
        )
        .map(Arc::as_ref)
    }

    fn find_binding<'a, T>(
        &'a self,
        in_frame: impl Fn(&'a Frame) -> Option<&'a T>,
        in_link: impl Fn(&'a CaptureLink) -> Option<&'a T>,
    ) -> Option<&'a T> {
        // The first link at each depth fits in a fixed table. Other chains can
        // have different links at the same depth, so compare by identity.
        let mut first = [std::ptr::null::<CaptureLink>(); MAX_DEFINES_PER_MODULE + 1];
        let mut other = HashSet::new();
        for frame in self.frames.iter().rev() {
            if let Some(found) = in_frame(frame) {
                return Some(found);
            }
            if let Some(capture) = frame.capture() {
                for link in capture.links() {
                    let ptr = std::ptr::from_ref(link);
                    let fresh = match first.get_mut(link.depth) {
                        Some(slot) if slot.is_null() => {
                            *slot = ptr;
                            true
                        }
                        Some(slot) if *slot == ptr => false,
                        _ => other.insert(ptr),
                    };
                    // A previous miss at this link also covered its older suffix.
                    if !fresh {
                        break;
                    }
                    #[cfg(test)]
                    tests::note_capture_probe();
                    if let Some(found) = in_link(link) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// Freeze the frame-zero writes since the last capture.
    pub(crate) fn capture(&mut self) -> Result<CapturedScope, crate::error::MdsError> {
        let Some(frame) = self.frames.first().filter(|_| self.frames.len() == 1) else {
            return Err(crate::error::MdsError::syntax(
                "internal error: cannot capture outside the global scope frame — this is a compiler bug, please report it",
            ));
        };
        if !self.pending_functions.is_empty() || !self.pending_namespaces.is_empty() {
            let depth = self.head.as_ref().map_or(0, |link| link.depth);
            if depth >= MAX_DEFINES_PER_MODULE {
                return Err(crate::error::MdsError::syntax(format!(
                    "internal error: cannot create more than {MAX_DEFINES_PER_MODULE} capture links — this is a compiler bug, please report it"
                )));
            }
            self.head = Some(Arc::new(CaptureLink {
                functions: std::mem::take(&mut self.pending_functions),
                namespaces: std::mem::take(&mut self.pending_namespaces),
                prev: self.head.take(),
                depth: depth + 1,
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
    use std::cell::Cell;

    thread_local! {
        static CAPTURE_PROBES: Cell<usize> = const { Cell::new(0) };
        static BUILTIN_PROBES: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn note_capture_probe() {
        CAPTURE_PROBES.with(|count| count.set(count.get() + 1));
    }

    pub(super) fn note_builtin_probe() {
        BUILTIN_PROBES.with(|count| count.set(count.get() + 1));
    }

    #[test]
    fn capture_lookup_builtin_miss_probes_only_distinct_links() {
        use std::fmt::Write;

        const CALL_DEPTH: usize = 120;
        const DEFINES: usize = 240;
        let mut source = String::new();
        for i in 0..(DEFINES - CALL_DEPTH) {
            writeln!(source, "@define unused{i}():\n@end").unwrap();
        }
        source.push_str("@define first():\n{{upper(\"a\")}}\n@end\n");
        for i in 1..CALL_DEPTH {
            let previous = if i == 1 {
                "first".into()
            } else {
                format!("f{}", i - 1)
            };
            writeln!(source, "@define f{i}():\n{{{{{previous}()}}}}\n@end").unwrap();
        }
        writeln!(source, "{{{{f{}()}}}}", CALL_DEPTH - 1).unwrap();

        BUILTIN_PROBES.set(0);
        let result = crate::compile_str(&source).unwrap();
        let probes = BUILTIN_PROBES.replace(0);
        let crate::CompiledOutput::Markdown(text) = result.output else {
            panic!("expected Markdown output");
        };
        assert_eq!(text.trim(), "A");
        assert_eq!(probes, DEFINES - 1);
    }

    #[test]
    fn capture_lookup_probes_each_distinct_link_once() {
        let mut left = Scope::new();
        define(&mut left, "common");
        left.capture().unwrap();
        let mut right = left.clone();
        define(&mut left, "left");
        let left_call = define(&mut left, "left_call");
        let right_fn = define(&mut right, "right");
        let right_call = define(&mut right, "right_call");

        let mut call = Scope::new();
        call.push_call(&left_call);
        call.push_call(&right_call);
        call.push_call(&left_call);

        CAPTURE_PROBES.set(0);
        assert!(call.get_function("missing").is_none());
        let function_probes = CAPTURE_PROBES.replace(0);
        assert!(call.get_namespace("missing").is_none());
        let namespace_probes = CAPTURE_PROBES.replace(0);
        assert_eq!((function_probes, namespace_probes), (3, 3));
        assert!(Arc::ptr_eq(call.get_function("right").unwrap(), &right_fn));
    }

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
                prompt_body: Some("outer".to_owned().into()),
                prompt_map: None,
                prompt_suppressed_by_exports: false,
            },
        );
        scope.push();
        scope.set_namespace(
            "lib",
            NamespaceScope {
                functions: HashMap::new(),
                prompt_body: Some("inner".to_owned().into()),
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
            Arc::new(
                [
                    ("b".into(), Arc::new(Value::Number(2.0))),
                    ("c".into(), Arc::new(Value::Number(2.0))),
                ]
                .into_iter()
                .collect(),
            ),
            Arc::new(
                [
                    ("a".into(), Arc::new(Value::Number(1.0))),
                    ("b".into(), Arc::new(Value::Number(1.0))),
                    ("c".into(), Arc::new(Value::Number(1.0))),
                ]
                .into_iter()
                .collect(),
            ),
        );
        scope.set_var("c", Value::Number(3.0));
        let callee = define(&mut scope, "callee");
        let mut call = Scope::new();
        call.push_call(&callee);
        assert_eq!(call.get_var("a"), Some(&Value::Number(1.0)));
        assert_eq!(call.get_var("b"), Some(&Value::Number(2.0)));
        assert_eq!(call.get_var("c"), Some(&Value::Number(3.0)));
    }

    #[test]
    fn call_frame_looks_up_captures_without_copying_entries() {
        let mut definition = Scope::new();
        definition.set_var("param", Value::Number(1.0));
        definition.set_var("shadowed", Value::Number(2.0));
        let captured_fn = define(&mut definition, "sibling");
        let namespace = Arc::new(make_ns());
        definition.set_namespace("lib", Arc::clone(&namespace));
        let callee = define(&mut definition, "callee");

        let mut call = Scope::new();
        call.set_var("shadowed", Value::Number(3.0));
        call.set_var("fallback", Value::Number(4.0));
        let caller_fn = define(&mut call, "sibling");
        call.push_call(&callee);
        call.set_var("param", Value::Number(5.0));

        assert_eq!(call.get_var("param"), Some(&Value::Number(5.0)));
        assert_eq!(call.get_var("shadowed"), Some(&Value::Number(2.0)));
        assert_eq!(call.get_var("fallback"), Some(&Value::Number(4.0)));
        assert!(Arc::ptr_eq(
            call.get_function("sibling").unwrap(),
            &captured_fn
        ));
        assert!(std::ptr::eq(
            call.get_namespace("lib").unwrap(),
            namespace.as_ref()
        ));
        let frame = call.frames.last().unwrap();
        assert_eq!(
            frame.vars.len(),
            1,
            "only the parameter belongs in the call frame"
        );
        assert!(frame.functions.is_empty());
        assert!(frame.namespaces.is_empty());

        call.pop().unwrap();
        assert_eq!(call.get_var("shadowed"), Some(&Value::Number(3.0)));
        assert!(Arc::ptr_eq(
            call.get_function("sibling").unwrap(),
            &caller_fn
        ));
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
        let callee = define(&mut scope, "callee");
        let mut call = Scope::new();
        call.push_call(&callee);
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

    #[test]
    fn capture_link_cap_refuses_link_257_and_keeps_pending_writes() {
        let mut scope = Scope::new();
        let function = capture_function(&mut Scope::new());
        for _ in 0..256 {
            scope.set_function("f", Arc::clone(&function));
            scope.capture().unwrap();
        }
        let at_cap = scope.capture().unwrap();
        assert_eq!(at_cap.links().count(), 256);

        scope.set_function("pending", function);
        let error = scope.capture().unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot create more than 256 capture links"));
        assert!(scope.pending_functions.contains_key("pending"));
        assert!(Arc::ptr_eq(
            scope.head.as_ref().unwrap(),
            at_cap.head.as_ref().unwrap()
        ));
    }

    fn capture_function(scope: &mut Scope) -> Arc<FunctionDef> {
        Arc::new(FunctionDef {
            params: vec![],
            body: vec![],
            captured: scope.capture().unwrap(),
            origin: None,
        })
    }

    fn define(scope: &mut Scope, name: &str) -> Arc<FunctionDef> {
        let function = capture_function(scope);
        scope.set_function(name, Arc::clone(&function));
        function
    }

    #[test]
    fn deep_capture_graph_drops_on_a_small_stack() {
        const DEPTH: usize = 200_000;
        let mut root = capture_function(&mut Scope::new());
        let first = Arc::downgrade(&root);
        for index in 1..DEPTH {
            let mut scope = Scope::new();
            scope.set_function("previous", Arc::clone(&root));
            if index % 2 == 0 {
                let mut namespace = make_ns();
                namespace.functions.insert("previous".into(), root);
                scope.set_namespace("lib", namespace);
            }
            root = capture_function(&mut scope);
        }
        assert!(captured_function_nodes(&root) >= DEPTH);
        assert!(first.upgrade().is_some());

        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let debug = format!("{:?}", root.captured);
                assert!(debug.contains("links: 1"));
                assert!(!debug.contains("previous"));
                assert!(debug.len() < 256);
                drop(root);
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(first.upgrade().is_none());
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
