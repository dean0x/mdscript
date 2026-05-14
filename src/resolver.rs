use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexSet;

use crate::ast::{DefineBlock, ExportDirective, ImportDirective, Node};
use crate::error::MdsError;
use crate::evaluator::evaluate;
use crate::lexer::tokenize;
use crate::parser::parse_with_ctx;
use crate::scope::{FunctionDef, NamespaceScope, Scope};
use crate::validator;
use crate::value::Value;

/// Walk up from a directory to find the project root.
/// Looks for `.git` or `.mdsroot` markers.
/// Falls back to the given directory if no marker is found.
fn find_project_root(start: &Path) -> PathBuf {
    let mut dir = start.to_path_buf();
    for _ in 0..256 {
        for marker in [".git", ".mdsroot"] {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        if !dir.pop() {
            return start.to_path_buf();
        }
    }
    start.to_path_buf()
}

/// A resolved module with its AST, exports, and prompt body.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub functions: HashMap<String, Arc<FunctionDef>>,
    pub prompt_body: Option<String>,
    pub has_explicit_exports: bool,
    pub explicit_exports: HashSet<String>,
}

/// Maximum import depth to prevent stack overflow from deeply chained imports.
const MAX_IMPORT_DEPTH: usize = 64;

/// Maximum file size (10 MB) to prevent runaway memory use.
pub(crate) const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Module cache to avoid re-resolving the same file.
#[derive(Default)]
pub struct ModuleCache {
    modules: HashMap<PathBuf, Arc<ResolvedModule>>,
    /// Tracks modules currently being resolved. IndexSet provides both O(1)
    /// membership test (like HashSet) and insertion-ordered iteration (like Vec),
    /// so a separate `resolving_stack` is no longer needed.
    resolving: IndexSet<PathBuf>,
    /// Root directory for path-traversal prevention (set on first resolve).
    root_dir: Option<PathBuf>,
}

impl ModuleCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Canonicalize `path` and run all security checks, returning `(canonical, is_md)`.
    ///
    /// This is the first half of the old `validate_and_read_file`.  It performs every
    /// check that does NOT require reading the file — symlink detection, root_dir
    /// initialisation, import-depth guard, and path-traversal prevention.  The caller
    /// must check the cache before calling `read_validated_file`; that way cache hits
    /// pay only the cost of two `canonicalize` syscalls and no I/O.
    fn canonicalize_and_check(
        &mut self,
        path: &Path,
    ) -> Result<(PathBuf, bool), MdsError> {
        // Detect symlinks without a TOCTOU window by comparing the canonical path to
        // the path constructed from (canonical parent dir) + (original filename).
        //
        // Strategy:
        //   1. Canonicalize the parent directory of `path` — this resolves any symlinks
        //      in the directory hierarchy (e.g. /var -> /private/var on macOS) without
        //      following the final component.
        //   2. Append the filename from `path` to get the "canonical parent + raw name".
        //   3. Canonicalize the full path to get the real file location.
        //   4. If they differ, the final component was a symlink.
        //
        // Both canonicalize calls use the file-system state atomically observed at that
        // instant. This shrinks the TOCTOU window to the unavoidable OS-level race
        // while correctly handling OS symlinks in the directory hierarchy.
        let file_name = path
            .file_name()
            .ok_or_else(|| MdsError::file_not_found(path.display().to_string()))?;

        let parent = path.parent().unwrap_or(Path::new("."));
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(path.display().to_string()))?;
        let canonical_without_following_last = canonical_parent.join(file_name);

        let canonical = canonical_without_following_last
            .canonicalize()
            .map_err(|_| MdsError::file_not_found(path.display().to_string()))?;

        // If the final-component-preserved path differs from the fully-resolved path,
        // the final component was a symlink.
        if canonical != canonical_without_following_last {
            return Err(MdsError::import_error(format!(
                "symlinks are not allowed in imports: {}",
                path.display()
            )));
        }

        // Set root_dir on first resolve (project root, not just entry point directory)
        if self.root_dir.is_none() {
            let entry_dir = canonical.parent().unwrap_or(Path::new("."));
            self.root_dir = Some(find_project_root(entry_dir));
        }

        // Guard against excessively deep import chains
        if self.resolving.len() >= MAX_IMPORT_DEPTH {
            return Err(MdsError::import_error(format!(
                "import depth exceeds maximum of {MAX_IMPORT_DEPTH} (possible deep chain)"
            )));
        }

        // Prevent path traversal: resolved path must stay within the root directory
        if let Some(ref root) = self.root_dir {
            if !canonical.starts_with(root) {
                return Err(MdsError::import_error(format!(
                    "import path escapes project directory: {}",
                    canonical.display()
                )));
            }
        }

        let is_md = canonical.extension().and_then(|e| e.to_str()) == Some("md");
        Ok((canonical, is_md))
    }

    /// Read and decode the file at `canonical` (already security-checked).
    ///
    /// This is the second half of the old `validate_and_read_file`.  It is called only
    /// on cache misses, so the expensive I/O never runs for files that are already cached.
    fn read_validated_file(canonical: &Path) -> Result<String, MdsError> {
        // Read the file as bytes first, then check size (avoids TOCTOU race between
        // a separate metadata call and the actual read).
        let bytes = std::fs::read(canonical)
            .map_err(|e| MdsError::io(format!("cannot read {}: {e}", canonical.display())))?;
        if bytes.len() as u64 > MAX_FILE_SIZE {
            return Err(MdsError::resource_limit(format!(
                "file too large ({} bytes, max {} bytes): {}",
                bytes.len(),
                MAX_FILE_SIZE,
                canonical.display()
            )));
        }
        String::from_utf8(bytes).map_err(|e| {
            MdsError::io(format!("invalid UTF-8 in {}: {e}", canonical.display()))
        })
    }

    /// Resolve a module from file path. Handles caching and cycle detection.
    pub fn resolve(
        &mut self,
        path: &Path,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        // Step 1: canonicalize + security checks (no file read yet).
        let (canonical, is_md) = self.canonicalize_and_check(path)?;

        // Step 2: cache hit — return immediately without reading the file.
        if let Some(cached) = self.modules.get(&canonical) {
            return Ok(Arc::clone(cached));
        }

        // Step 3: cycle detection — must happen before we push to `resolving`.
        if self.resolving.contains(&canonical) {
            let cycle = build_cycle_string(&self.resolving, &canonical);
            return Err(MdsError::circular_import(cycle));
        }

        // Step 4: read the file only on a cache miss.
        let source = Self::read_validated_file(&canonical)?;

        // Validate file type (uses already-read source for .md frontmatter check)
        validate_file_type(&canonical, &source)?;

        let file_str = canonical.display().to_string();
        let base_dir = canonical.parent().unwrap_or(Path::new(".")).to_path_buf();

        // Mark as resolving before recursing into process_module.
        // IndexSet preserves insertion order, so it serves as both the set (O(1) lookup)
        // and the ordered stack (for cycle path reconstruction).
        self.resolving.insert(canonical.clone());

        let resolved =
            self.process_module(&source, &file_str, &base_dir, is_md, runtime_vars, warnings);

        // Unmark regardless of success or failure. resolve/unmark is strictly LIFO
        // (we always remove the last element we inserted), so pop() is O(1).
        // Safety-critical LIFO invariant: a mismatched pop would silently corrupt
        // cycle-detection state and allow unbounded recursion. Enforce in release
        // mode — cost is negligible at MAX_IMPORT_DEPTH = 64.
        let popped = self.resolving.pop();
        assert_eq!(popped.as_ref(), Some(&canonical), "resolving unmark must be LIFO");

        let resolved = resolved?;

        // Wrap in Arc, store in cache, and return a clone of the Arc (O(1)).
        let arc = Arc::new(resolved);
        self.modules.insert(canonical, Arc::clone(&arc));

        Ok(arc)
    }

    /// Resolve a module from an in-memory source string.
    /// Imports within the source are resolved relative to `base_dir`.
    pub fn resolve_source(
        &mut self,
        source: &str,
        base_dir: &Path,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        // Set root_dir on first use so path-traversal checks work for imports.
        // Canonicalize to match the canonical paths used by resolve(), ensuring
        // starts_with checks are consistent even when base_dir contains `.` or `..`.
        if self.root_dir.is_none() {
            self.root_dir = Some(base_dir.canonicalize().map_err(|e| {
                MdsError::io(format!(
                    "cannot resolve base directory {}: {e}",
                    base_dir.display()
                ))
            })?);
        }
        self.process_module(source, "<source>", base_dir, false, runtime_vars, warnings)
            .map(Arc::new)
    }

    /// Common module processing: tokenize, parse, build scope, evaluate.
    ///
    /// Orchestrates the full pipeline in ~25 lines; detailed logic lives in helpers.
    fn process_module(
        &mut self,
        source: &str,
        file_str: &str,
        base_dir: &Path,
        is_md: bool,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // Tokenize and parse
        let tokens = tokenize(source, file_str)?;
        let module = parse_with_ctx(&tokens, file_str, source)?;

        // Build scope from frontmatter + runtime vars
        let mut scope = build_scope_from_frontmatter(module.frontmatter.as_ref(), is_md, runtime_vars)?;

        // Walk the AST: collect @define functions (with closure capture), process imports/exports
        let ctx = ModuleCtx { file_str, source, base_dir, runtime_vars };
        let CollectedDefs { functions, has_explicit_exports, explicit_exports } = self
            .collect_definitions_and_imports(&module.body, &mut scope, &ctx, warnings)?;

        // Validate that all named exports refer to defined functions or "prompt"
        validate_exports(&explicit_exports, &functions)?;

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &scope, file_str, source)?;

        // Evaluate the body to get prompt text
        let prompt_body = evaluate(&module.body, &mut scope, warnings)?;
        let prompt_body = (!prompt_body.trim().is_empty()).then_some(prompt_body);

        Ok(ResolvedModule {
            functions,
            prompt_body,
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
                validate_import_path(import_path)?;
                let resolved_path = resolve_path(ctx.base_dir, import_path);
                let source_module = self.resolve(&resolved_path, ctx.runtime_vars, warnings)?;
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
                validate_import_path(import_path)?;
                let resolved_path = resolve_path(ctx.base_dir, import_path);
                let source_module = self.resolve(&resolved_path, ctx.runtime_vars, warnings)?;
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
            } => {
                validate_import_path(path)?;
                let import_path = resolve_path(ctx.base_dir, path);
                let resolved = self
                    .resolve(&import_path, ctx.runtime_vars, warnings)
                    .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, *offset))?;
                scope.set_namespace(alias, resolved.to_namespace());
            }
            ImportDirective::Merge { path, offset } => {
                validate_import_path(path)?;
                let import_path = resolve_path(ctx.base_dir, path);
                let resolved = self
                    .resolve(&import_path, ctx.runtime_vars, warnings)
                    .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, *offset))?;
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
            }
            ImportDirective::Selective {
                names,
                path,
                offset,
            } => {
                validate_import_path(path)?;
                let import_path = resolve_path(ctx.base_dir, path);
                let resolved = self
                    .resolve(&import_path, ctx.runtime_vars, warnings)
                    .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, *offset))?;
                let line_len = ctx.source[*offset..]
                    .find('\n')
                    .unwrap_or(ctx.source[*offset..].len());
                let not_exported = |name: &str| {
                    MdsError::import_error_at(
                        format!("'{name}' is not exported from '{path}'"),
                        ctx.file_str,
                        ctx.source,
                        *offset,
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
            }
        }
        Ok(())
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
    /// Directory that contains the source file; used to resolve relative `@import` paths.
    base_dir: &'a Path,
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

/// Resolve a relative path against a base directory.
/// Per spec: only relative paths are allowed (must start with "./" or "../").
fn resolve_path(base_dir: &Path, relative: &str) -> PathBuf {
    base_dir.join(relative)
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
/// Accepts the already-read source content to avoid double-reading for `.md` files.
fn validate_file_type(path: &Path, source: &str) -> Result<(), MdsError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    if ext == "mds" {
        return Ok(());
    }

    // For .md files, accept when frontmatter contains `type: mds`.
    if ext == "md" {
        let found = source
            .strip_prefix("---\n")
            .or_else(|| source.strip_prefix("---\r\n"))
            .and_then(|after_fence| after_fence.find("\n---").map(|end| &after_fence[..end]))
            .is_some_and(|fm| {
                // Check each line for `type: mds` without a full YAML parse.
                fm.lines().any(|line| {
                    line.trim()
                        .strip_prefix("type:")
                        .is_some_and(|v| v.trim() == "mds")
                })
            });
        if found {
            return Ok(());
        }
    }

    Err(MdsError::not_mds_file(path.display().to_string()))
}

/// Format a cycle chain like "a.mds → b.mds → a.mds" from the resolving set.
///
/// `IndexSet` preserves insertion order, so we can use it as both the set
/// and the ordered stack for cycle path reconstruction.
fn build_cycle_string(resolving: &IndexSet<PathBuf>, repeated: &Path) -> String {
    let start = resolving
        .iter()
        .position(|p| p == repeated)
        .unwrap_or(0);
    resolving.as_slice()[start..]
        .iter()
        .map(PathBuf::as_path)
        .chain(std::iter::once(repeated))
        .map(path_display_name)
        .collect::<Vec<_>>()
        .join(" \u{2192} ")
}

/// Return a short display name for a path (filename, falling back to full path, then "?").
fn path_display_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .or_else(|| p.to_str())
        .unwrap_or("?")
        .to_string()
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
        MdsError::CircularImport { cycle, span: None, .. } => {
            MdsError::circular_import_at(cycle, file_str, source, offset, line_len)
        }
        other => other,
    }
}

/// Parse raw YAML frontmatter into a variable map.
/// Non-string keys are silently skipped (YAML allows integer keys; we only support string names).
fn parse_frontmatter(raw: &str) -> Result<HashMap<String, Value>, MdsError> {
    let yaml: serde_yml::Value =
        serde_yml::from_str(raw).map_err(|e| MdsError::yaml_error(e.to_string()))?;

    let mut vars = HashMap::new();
    if let serde_yml::Value::Mapping(map) = yaml {
        for (key, val) in map {
            let serde_yml::Value::String(key_str) = key else {
                continue;
            };
            let value = Value::from_yaml(val)?;
            vars.insert(key_str, value);
        }
    }
    Ok(vars)
}
