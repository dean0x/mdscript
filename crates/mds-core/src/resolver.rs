use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};

use crate::ast::{DefineBlock, ExportDirective, ImportDirective, Node};
use crate::error::MdsError;
use crate::evaluator::evaluate;
use crate::fs::{FileSystem, NativeFs, VirtualFs};
use crate::lexer::tokenize;
use crate::parser::parse_with_ctx;
use crate::scope::{FunctionDef, NamespaceScope, Scope};
use crate::validator;
use crate::value::Value;

/// A resolved module with its AST, exports, and prompt body.
///
/// Fields are `pub(crate)` — all external access must go through the methods
/// (`get_export`, `get_all_exports`, `get_prompt_value`, `to_namespace`) which
/// enforce export-visibility logic. Direct field access bypasses that logic.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub(crate) functions: HashMap<String, Arc<FunctionDef>>,
    pub(crate) prompt_body: Option<String>,
    pub(crate) raw_frontmatter: Option<String>,
    pub(crate) has_explicit_exports: bool,
    pub(crate) explicit_exports: HashSet<String>,
}

/// Maximum import depth to prevent stack overflow from deeply chained imports.
const MAX_IMPORT_DEPTH: usize = 64;

/// Module cache to avoid re-resolving the same file or virtual key.
///
/// Supports multiple filesystem backends via the [`FileSystem`] trait.
pub struct ModuleCache {
    fs: Box<dyn FileSystem>,
    /// Stores resolved modules in first-resolution (depth-first) order.
    /// IndexMap preserves insertion order while providing O(1) get/insert/contains_key,
    /// enabling efficient dependency-graph extraction via `dependencies()`.
    modules: IndexMap<String, Arc<ResolvedModule>>,
    /// Tracks modules currently being resolved. IndexSet provides both O(1)
    /// membership test (like HashSet) and insertion-ordered iteration (like Vec),
    /// so a separate `resolving_stack` is no longer needed.
    resolving: IndexSet<String>,
}

impl std::fmt::Debug for ModuleCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleCache")
            .field("modules_count", &self.modules.len())
            .field("resolving_count", &self.resolving.len())
            .finish_non_exhaustive()
    }
}

impl ModuleCache {
    /// Create a new `ModuleCache` backed by the native OS filesystem.
    ///
    /// Equivalent to [`ModuleCache::native`].
    pub fn new() -> Self {
        Self::native()
    }

    /// Create a `ModuleCache` backed by the native OS filesystem.
    pub fn native() -> Self {
        Self {
            fs: Box::new(NativeFs::new()),
            modules: IndexMap::new(),
            resolving: IndexSet::new(),
        }
    }

    /// Create a `ModuleCache` backed by an in-memory virtual filesystem.
    ///
    /// Useful for testing and WASM environments where OS filesystem access
    /// is unavailable.
    pub fn virtual_fs(modules: HashMap<String, String>) -> Self {
        Self {
            fs: Box::new(VirtualFs::new(modules)),
            modules: IndexMap::new(),
            resolving: IndexSet::new(),
        }
    }

    /// Create a `ModuleCache` with a custom [`FileSystem`] implementation.
    pub fn with_fs(fs: Box<dyn FileSystem>) -> Self {
        Self {
            fs,
            modules: IndexMap::new(),
            resolving: IndexSet::new(),
        }
    }

    /// Returns normalized keys of all modules resolved during compilation,
    /// in first-resolution order (depth-first traversal).
    ///
    /// This includes the entry module itself. Use this after a successful
    /// `resolve_path` / `resolve_key` / `resolve_source` call to obtain the
    /// dependency graph. Callers that want to exclude the entry point should
    /// filter it out themselves (see `compile_virtual_with_deps`).
    pub fn dependencies(&self) -> Vec<String> {
        self.modules.keys().cloned().collect()
    }

    /// Guard against excessively deep import chains.
    fn check_import_depth(&self) -> Result<(), MdsError> {
        if self.resolving.len() >= MAX_IMPORT_DEPTH {
            return Err(MdsError::import_error(format!(
                "import depth exceeds maximum of {MAX_IMPORT_DEPTH} (possible deep chain)"
            )));
        }
        Ok(())
    }

    /// Resolve a module from a filesystem path string.
    ///
    /// `path` is a UTF-8 string representation of the OS path (callers convert
    /// `&Path` to `&str` at the public API boundary via `path_to_str`).
    /// Normalizes `path` to a canonical key via the underlying [`FileSystem`],
    /// then resolves through the module cache with cycle detection and depth guarding.
    pub fn resolve_path(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        let key = self.fs.normalize("", path)?;
        self.resolve_by_key(&key, runtime_vars, warnings)
    }

    /// Resolve a module by its normalized key.
    ///
    /// This is the core resolution loop: cache check → depth check →
    /// cycle detection → read → validate type → process → cache insert.
    fn resolve_by_key(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        // Step 1: cache hit — return immediately without reading.
        if let Some(cached) = self.modules.get(key) {
            return Ok(Arc::clone(cached));
        }

        // Step 2: cycle detection — must happen before we push to `resolving`.
        if self.resolving.contains(key) {
            let cycle = build_cycle_string(&self.resolving, key);
            return Err(MdsError::circular_import(cycle));
        }

        // Step 3: depth guard.
        self.check_import_depth()?;

        // Step 4: read the file only on a cache miss.
        let source = self.fs.read(key)?;

        // Step 5: determine if markdown (for frontmatter type-key handling).
        let is_md = self.fs.is_markdown(key);

        // Step 6: validate file type.
        validate_file_type(key, &source)?;

        // Mark as resolving before recursing into process_module.
        // IndexSet preserves insertion order, so it serves as both the set (O(1) lookup)
        // and the ordered stack (for cycle path reconstruction).
        self.resolving.insert(key.to_string());

        let ctx = ModuleCtx {
            file_str: key,
            source: &source,
            base_key: key,
            runtime_vars,
        };
        let resolved = self.process_module(&ctx, is_md, warnings);

        // Unmark regardless of success or failure. resolve/unmark is strictly LIFO
        // (we always remove the last element we inserted), so pop() is O(1).
        // Safety-critical LIFO invariant: a mismatched pop would silently corrupt
        // cycle-detection state and allow unbounded recursion.
        let popped = self.resolving.pop();
        let resolved = Self::check_lifo_pop(resolved, popped, key)?;

        // Wrap in Arc, store in cache, and return a clone of the Arc (O(1)).
        let key_owned = key.to_string();
        let arc = Arc::new(resolved);
        self.modules.insert(key_owned, Arc::clone(&arc));

        Ok(arc)
    }

    /// Resolve an import from within a module identified by `base_key`.
    ///
    /// Validates the import path, normalizes it via the filesystem, then
    /// delegates to [`ModuleCache::resolve_by_key`].
    fn resolve_import_from(
        &mut self,
        base_key: &str,
        relative: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        validate_import_path(relative)?;
        let key = self.fs.normalize(base_key, relative)?;
        self.resolve_by_key(&key, runtime_vars, warnings)
    }

    /// Resolve a module by its normalized key.
    ///
    /// This is the entry point for virtual filesystems where there is no OS path.
    /// Use this with [`ModuleCache::virtual_fs`] or a custom [`FileSystem`] backend.
    pub fn resolve_key(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        self.resolve_by_key(key, runtime_vars, warnings)
    }

    /// Resolve a module from an in-memory source string.
    ///
    /// Imports within the source are resolved relative to `base_dir`.
    ///
    /// **NativeFs-only**: this method calls `canonicalize()` and `fs.set_root()`,
    /// which only make sense for OS-backed filesystems. For virtual or
    /// WASM environments use [`ModuleCache::resolve_key`] instead.
    pub fn resolve_source(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        // Canonicalize base_dir via the FileSystem abstraction so that custom
        // or virtual backends can override this behaviour (fixes issue #21).
        let canonical_str = self.fs.canonicalize(base_dir)?;
        self.fs.set_root(&canonical_str)?;

        // The base_key must look like a file path so that normalize() can call
        // parent() on it to get the directory. Append a synthetic filename to
        // the canonical directory so imports resolve relative to that directory
        // (not its parent).
        let base_key = format!("{canonical_str}/<source>");

        // Guard against re-entrant or cyclic calls that could form a cycle
        // back through this root module. Mirrors the resolving bookkeeping in
        // resolve_by_key so that cycle detection and depth checks apply to the
        // root module as well.
        self.check_import_depth()?;
        self.resolving.insert(base_key.clone());

        let ctx = ModuleCtx {
            file_str: "<source>",
            source,
            base_key: &base_key,
            runtime_vars,
        };
        let resolved = self.process_module(&ctx, false, warnings);

        let popped = self.resolving.pop();
        Self::check_lifo_pop(resolved, popped, &base_key).map(Arc::new)
    }

    /// Assert the LIFO pop invariant after `process_module`.
    ///
    /// On double-fault (module error + LIFO violation), prefer the module error
    /// (user-facing root cause) over the LIFO violation (internal compiler bug).
    fn check_lifo_pop<T>(
        module_result: Result<T, MdsError>,
        popped: Option<String>,
        expected: &str,
    ) -> Result<T, MdsError> {
        let lifo_result = if popped.as_deref() == Some(expected) {
            Ok(())
        } else {
            Err(MdsError::syntax(format!(
                "internal error: resolving stack LIFO invariant violated \
                 (expected {expected}, got {got}) — this is a compiler bug, please report it",
                got = popped.as_deref().unwrap_or("<empty>"),
            )))
        };
        match (module_result, lifo_result) {
            (Err(module_err), _) => Err(module_err),
            (Ok(_), Err(lifo_err)) => Err(lifo_err),
            (Ok(resolved), Ok(())) => Ok(resolved),
        }
    }

    /// Common module processing: tokenize, parse, build scope, evaluate.
    ///
    /// `ctx.file_str` is the display path for error messages (may be `"<source>"`).
    /// `ctx.base_key` is the normalized key used to resolve relative imports.
    /// `is_md` controls whether the `type` frontmatter key is treated as a file-type marker.
    fn process_module(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // Tokenize and parse
        let tokens = tokenize(ctx.source, ctx.file_str)?;
        let module = parse_with_ctx(&tokens, ctx.file_str, ctx.source)?;

        // Capture raw frontmatter before build_scope_from_frontmatter borrows the module.
        let raw_frontmatter = module.frontmatter.as_ref().map(|fm| fm.raw.clone());

        // Build scope from frontmatter + runtime vars
        let mut scope =
            build_scope_from_frontmatter(module.frontmatter.as_ref(), is_md, ctx.runtime_vars)?;

        // Walk the AST: collect @define functions (with closure capture), process imports/exports
        let CollectedDefs {
            functions,
            has_explicit_exports,
            explicit_exports,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        // Validate that all named exports refer to defined functions or "prompt"
        validate_exports(&explicit_exports, &functions)?;

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        // Evaluate the body to get prompt text
        let prompt_body = evaluate(&module.body, &mut scope, warnings)?;
        let prompt_body = (!prompt_body.trim().is_empty()).then_some(prompt_body);

        Ok(ResolvedModule {
            functions,
            prompt_body,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
        })
    }

    /// Walk the AST body and collect `@define` functions (with closure capture),
    /// process `@import` directives, and record `@export` / `@export...from` entries.
    ///
    /// Returns a `CollectedDefs` struct with self-documenting field names.
    fn collect_definitions_and_imports(
        &mut self,
        body: &[Node],
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<CollectedDefs, MdsError> {
        let mut defs = CollectedDefs {
            functions: HashMap::new(),
            has_explicit_exports: false,
            explicit_exports: HashSet::new(),
        };

        for node in body {
            match node {
                Node::Define(def) => collect_define(def, &mut defs, scope, ctx)?,
                Node::Import(import) => self.resolve_import(import, scope, ctx, warnings)?,
                Node::Export(export) => self.collect_export(export, &mut defs, ctx, warnings)?,
                _ => {}
            }
        }

        Ok(defs)
    }

    /// Process a single `@export` directive, updating `defs` in place.
    ///
    /// Handles the three export forms: named (`@export foo`), re-export
    /// (`@export foo from "./bar"`), and wildcard (`@export * from "./bar"`).
    fn collect_export(
        &mut self,
        export: &ExportDirective,
        defs: &mut CollectedDefs,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        defs.has_explicit_exports = true;
        match export {
            ExportDirective::Named { name } => {
                defs.explicit_exports.insert(name.clone());
            }
            ExportDirective::ReExport {
                name,
                path: import_path,
            } => {
                // Resolve the source module and bring in the function for
                // re-export only. Per spec: "@export from does not bring the
                // symbol into the current file's scope".
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self.resolve_import_from(
                    ctx.base_key,
                    import_path,
                    ctx.runtime_vars,
                    warnings,
                )?;
                let func = source_module.get_export(name).ok_or_else(|| {
                    MdsError::export_error(format!(
                        "cannot re-export '{name}': not exported from \"{import_path}\""
                    ))
                })?;
                defs.functions.insert(name.clone(), func);
                defs.explicit_exports.insert(name.clone());
            }
            ExportDirective::Wildcard { path: import_path } => {
                // Re-export all exports from the target module. These are
                // available to importers but NOT in the current file's scope.
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self.resolve_import_from(
                    ctx.base_key,
                    import_path,
                    ctx.runtime_vars,
                    warnings,
                )?;
                for (name, func) in source_module.get_all_exports() {
                    if defs.functions.contains_key(&name) {
                        return Err(MdsError::name_collision(name));
                    }
                    defs.functions.insert(name.clone(), func);
                    defs.explicit_exports.insert(name);
                }
            }
        }
        Ok(())
    }

    fn resolve_alias_import(
        &mut self,
        path: &str,
        alias: &str,
        offset: usize,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        let resolved = self
            .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        scope.set_namespace(alias, resolved.to_namespace());
        Ok(())
    }

    fn resolve_merge_import(
        &mut self,
        path: &str,
        offset: usize,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        let resolved = self
            .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        // Per spec: only functions and the prompt body are imported via merge.
        // Frontmatter variables from the imported module are NOT brought into scope.
        for (name, func) in resolved.get_all_exports() {
            if scope.get_function(&name).is_some() {
                return Err(MdsError::name_collision(name));
            }
            scope.set_function(&name, func);
        }
        if let Some(val) = resolved.get_prompt_value() {
            scope.set_var("prompt", val);
        }
        Ok(())
    }

    fn resolve_selective_import(
        &mut self,
        names: &[String],
        path: &str,
        offset: usize,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        let resolved = self
            .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        let line_len = ctx.source[offset..]
            .find('\n')
            .unwrap_or(ctx.source[offset..].len());
        let not_exported = |name: &str| {
            MdsError::import_error_at(
                format!("'{name}' is not exported from '{path}'"),
                ctx.file_str,
                ctx.source,
                offset,
                line_len,
            )
        };
        for name in names {
            if name == "prompt" {
                scope.set_var(
                    "prompt",
                    resolved
                        .get_prompt_value()
                        .ok_or_else(|| not_exported(name))?,
                );
            } else {
                scope.set_function(
                    name,
                    resolved
                        .get_export(name)
                        .ok_or_else(|| not_exported(name))?,
                );
            }
        }
        Ok(())
    }

    fn resolve_import(
        &mut self,
        import: &ImportDirective,
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        match import {
            ImportDirective::Alias {
                path,
                alias,
                offset,
            } => self.resolve_alias_import(path, alias, *offset, scope, ctx, warnings),
            ImportDirective::Merge { path, offset } => {
                self.resolve_merge_import(path, *offset, scope, ctx, warnings)
            }
            ImportDirective::Selective {
                names,
                path,
                offset,
            } => self.resolve_selective_import(names, path, *offset, scope, ctx, warnings),
        }
    }
}

impl Default for ModuleCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolvedModule {
    /// Return `true` if `name` is an available export of this module.
    ///
    /// When no explicit `@export` list is present every name is visible.
    /// When an explicit list exists only the listed names are visible.
    fn is_exported(&self, name: &str) -> bool {
        !self.has_explicit_exports || self.explicit_exports.contains(name)
    }

    /// Get a single export by name.
    ///
    /// Returns `Arc<FunctionDef>` — cloning is O(1).
    pub fn get_export(&self, name: &str) -> Option<Arc<FunctionDef>> {
        if !self.is_exported(name) {
            return None;
        }
        self.functions.get(name).cloned()
    }

    /// Get all exported functions.
    ///
    /// Returns `Arc<FunctionDef>` values — cloning is O(1).
    pub fn get_all_exports(&self) -> Vec<(String, Arc<FunctionDef>)> {
        self.functions
            .iter()
            .filter(|(name, _)| self.is_exported(name))
            .map(|(name, func)| (name.clone(), Arc::clone(func)))
            .collect()
    }

    /// Get the prompt body as a Value, if it is an available export.
    pub fn get_prompt_value(&self) -> Option<Value> {
        if self.is_exported("prompt") {
            self.prompt_body.clone().map(Value::String)
        } else {
            None
        }
    }

    /// Convert this resolved module into a namespace scope for aliased imports.
    fn to_namespace(&self) -> NamespaceScope {
        // Build the HashMap in one pass, avoiding the intermediate Vec that
        // get_all_exports() would allocate.
        let functions = self
            .functions
            .iter()
            .filter(|(name, _)| self.is_exported(name))
            .map(|(name, func)| (name.clone(), Arc::clone(func)))
            .collect();
        // Respect export visibility: prompt_body is only included in the namespace
        // when "prompt" is an available export (same rule as get_prompt_value).
        let prompt_body = if self.is_exported("prompt") {
            self.prompt_body.clone()
        } else {
            None
        };
        NamespaceScope {
            functions,
            prompt_body,
        }
    }
}

/// Collected output of the AST definition/import walk in `collect_definitions_and_imports`.
struct CollectedDefs {
    functions: HashMap<String, Arc<FunctionDef>>,
    has_explicit_exports: bool,
    explicit_exports: HashSet<String>,
}

/// Bundle of borrowed per-module context threaded through the AST walk helpers.
struct ModuleCtx<'a> {
    /// Canonical display path of the source file (e.g. the path shown in error messages).
    file_str: &'a str,
    /// Raw file content used for source-span diagnostics (offset → line/column lookup).
    source: &'a str,
    /// Normalized key of the current module; used to resolve relative `@import` paths.
    base_key: &'a str,
    /// Variables injected at call-time (e.g. via `--set` or the public API `compile` call).
    runtime_vars: &'a HashMap<String, Value>,
}

/// Process a single `@define` directive, updating `defs` and `scope` in place.
///
/// Captures the definition-site scope for lexical closure semantics so the
/// function body can resolve alias imports, sibling functions, and frontmatter
/// variables from its defining module even when called from a different module.
fn collect_define(
    def: &DefineBlock,
    defs: &mut CollectedDefs,
    scope: &mut Scope,
    ctx: &ModuleCtx<'_>,
) -> Result<(), MdsError> {
    if defs.functions.contains_key(&def.name) {
        return Err(MdsError::name_collision_at(
            &def.name,
            ctx.file_str,
            ctx.source,
            def.offset,
            def.name.len(),
        ));
    }
    let mut func = FunctionDef::from(def);
    // Capture definition-site scope for lexical closure semantics.
    func.captured.namespaces = scope.get_all_namespaces();
    // Convert Arc<FunctionDef> → owned FunctionDef for captured.functions.
    // Owned captures break potential reference cycles (A captures B captures A).
    func.captured.functions = scope
        .get_all_functions()
        .into_iter()
        .map(|(k, v)| (k, (*v).clone()))
        .collect();
    func.captured.vars = scope.get_all_vars();
    // Wrap in Arc for cheap storage and O(1) scope insertion.
    let arc = Arc::new(func);
    defs.functions.insert(def.name.clone(), Arc::clone(&arc));
    scope.set_function(&def.name, arc);
    Ok(())
}

/// Build a scope from optional frontmatter and runtime variable overrides.
///
/// Parses frontmatter YAML (if present), populates scope with variables,
/// then applies runtime_vars to override any frontmatter keys.
/// The `type` key is skipped for `.md` files (it is a file-type marker, not a template var).
fn build_scope_from_frontmatter(
    frontmatter: Option<&crate::ast::Frontmatter>,
    is_md: bool,
    runtime_vars: &HashMap<String, Value>,
) -> Result<Scope, MdsError> {
    let mut scope = Scope::new();

    if let Some(fm) = frontmatter {
        let yaml_vars = parse_frontmatter(&fm.raw)?;
        for (key, value) in yaml_vars {
            if key == "type" && is_md {
                // Skip the 'type' meta-field for .md files (it's a file-type marker)
                continue;
            }
            scope.set_var(&key, value);
        }
    }

    // Apply runtime vars (override frontmatter)
    for (key, value) in runtime_vars {
        scope.set_var(key, value.clone());
    }

    Ok(scope)
}

/// Validate that all named exports refer to defined functions or the special `"prompt"` export.
fn validate_exports(
    explicit_exports: &HashSet<String>,
    functions: &HashMap<String, Arc<FunctionDef>>,
) -> Result<(), MdsError> {
    for name in explicit_exports {
        if name != "prompt" && !functions.contains_key(name) {
            return Err(MdsError::export_error(format!(
                "cannot export '{name}': not defined in this module"
            )));
        }
    }
    Ok(())
}

/// Validate that an import path is safe and relative.
///
/// Rejects absolute paths and paths containing components that could escape
/// the project directory (e.g., null bytes).
fn validate_import_path(path: &str) -> Result<(), MdsError> {
    if !path.starts_with("./") && !path.starts_with("../") {
        return Err(MdsError::import_error(format!(
            "import path must be relative (start with './' or '../'): \"{path}\""
        )));
    }
    // Reject null bytes which could truncate paths in some OS APIs
    if path.contains('\0') {
        return Err(MdsError::import_error("import path contains null byte"));
    }
    Ok(())
}

/// Validate that a file is a valid MDS file.
///
/// Accepts the already-read source content to avoid double-reading for `.md` files.
/// Uses the normalized key (string) rather than a Path.
fn validate_file_type(key: &str, source: &str) -> Result<(), MdsError> {
    // Extract extension from the key string (split on '/' and '\\' for portability).
    let filename = key.rsplit(['/', '\\']).next().unwrap_or(key);
    // Guard against dotfiles: a filename that starts with '.' and contains no
    // further '.' (e.g. ".mds") has no extension — reject it the same way
    // Path::extension() would return None for such files.
    let ext = if filename.starts_with('.') && !filename[1..].contains('.') {
        None
    } else {
        filename.rsplit('.').next().filter(|e| *e != filename)
    };

    if ext == Some("mds") {
        return Ok(());
    }

    // For .md files, accept when frontmatter contains `type: mds`.
    if ext == Some("md") && has_type_mds_frontmatter(source) {
        return Ok(());
    }

    Err(MdsError::not_mds_file(key.to_string()))
}

/// Return `true` if `source` has a YAML frontmatter block containing `type: mds`.
///
/// Checks without a full YAML parse by scanning frontmatter lines for the key.
/// Recognises all three YAML quoting styles: bare, single-quoted, double-quoted.
fn has_type_mds_frontmatter(source: &str) -> bool {
    source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
        .and_then(|after_fence| after_fence.find("\n---").map(|end| &after_fence[..end]))
        .is_some_and(|fm| {
            fm.lines().any(|line| {
                line.trim().strip_prefix("type:").is_some_and(|v| {
                    let v = v.trim();
                    // Match bare, double-quoted, and single-quoted values,
                    // mirroring the strip_type_mds helper elsewhere.
                    v == "mds" || v == "\"mds\"" || v == "'mds'"
                })
            })
        })
}

/// Format a cycle chain like "a.mds → b.mds → a.mds" from the resolving set.
///
/// `IndexSet` preserves insertion order, so we can use it as both the set
/// and the ordered stack for cycle path reconstruction.
fn build_cycle_string(resolving: &IndexSet<String>, repeated: &str) -> String {
    let start = resolving.iter().position(|k| k == repeated).unwrap_or(0);
    resolving.as_slice()[start..]
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(repeated))
        .map(key_display_name)
        .collect::<Vec<_>>()
        .join(" \u{2192} ")
}

/// Return a short display name for a normalized key (filename, falling back to the key).
fn key_display_name(key: &str) -> &str {
    // Split on both '/' and '\\' for portability across OS and VirtualFs keys.
    key.rsplit(['/', '\\']).next().unwrap_or(key)
}

/// If `err` is a `FileNotFound` error with no source span, attach a span pointing
/// to the `@import` directive in the parent file. Other error variants are returned
/// unchanged so that cascading errors (e.g. circular imports inside the missing
/// file) still report their own locations.
fn attach_import_span(
    err: MdsError,
    path: &str,
    file_str: &str,
    source: &str,
    offset: usize,
) -> MdsError {
    // Compute the span length as the number of bytes from `offset` to the
    // end of the `@import` line (not including the newline character itself),
    // so the whole directive is underlined.
    let line_len = source[offset..]
        .find('\n')
        .unwrap_or(source[offset..].len());
    match err {
        MdsError::FileNotFound { span: None, .. } => {
            MdsError::file_not_found_at(path, file_str, source, offset, line_len)
        }
        MdsError::CircularImport {
            cycle, span: None, ..
        } => MdsError::circular_import_at(cycle, file_str, source, offset, line_len),
        other => other,
    }
}

/// Parse raw YAML frontmatter into a variable map.
/// Non-string keys are silently skipped (YAML allows integer keys; we only support string names).
fn parse_frontmatter(raw: &str) -> Result<HashMap<String, Value>, MdsError> {
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(raw).map_err(|e| MdsError::yaml_error(e.to_string()))?;

    let mut vars = HashMap::new();
    if let serde_yaml_ng::Value::Mapping(map) = yaml {
        for (key, val) in map {
            let serde_yaml_ng::Value::String(key_str) = key else {
                continue;
            };
            let value = Value::from_yaml(val)?;
            vars.insert(key_str, value);
        }
    }
    Ok(vars)
}
