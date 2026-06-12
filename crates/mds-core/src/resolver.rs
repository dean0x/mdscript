use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};

use crate::ast::{BlockNode, DefineBlock, ExportDirective, ImportDirective, Node};
use crate::error::MdsError;
use crate::evaluator::{evaluate, evaluate_messages, EvalMessage};
use crate::fs::{FileSystem, NativeFs, VirtualFs};
use crate::lexer::tokenize;
use crate::limits::{MAX_BLOCKS_PER_MODULE, MAX_FRONTMATTER_IMPORTS, MAX_FRONTMATTER_MERGE_DEPTH};
use crate::parser::is_valid_identifier;
use crate::parser::parse_with_ctx;
use crate::scope::{FunctionDef, NamespaceScope, Scope};
use crate::validator;
use crate::value::Value;

/// A resolved module with its AST, exports, and prompt body.
///
/// Fields are `pub(crate)` — all external access must go through the methods
/// (`get_export`, `get_all_exports`, `get_prompt_value`, `to_namespace`) which
/// enforce export-visibility logic. Direct field access bypasses that logic.
///
/// # Template Inheritance Fields (Phase 2)
///
/// - `effective_skeleton`: the root-ancestor body as a shared `Arc<[Node]>`. For a
///   non-extending module this is the module's own body (built once; Arc-shared across
///   all extending descendants). For an extending module it is `Arc::clone` of the
///   base's skeleton — never a deep-clone of the `Vec<Node>` (DoS guard, P1).
///
/// - `effective_blocks`: name → fully-overridden `BlockNode`. For non-extending modules
///   it is seeded from the module's own `@block` declarations. For extending modules it
///   is a clone of `base.effective_blocks` with the child's overrides applied (most-
///   derived wins, diamond-inheritance safe — NEVER mutate the cached base map).
///
/// - `frontmatter_values`: the module's parsed YAML mapping. Reserved-key splitting is
///   deferred to Phase 3 (`deep_merge_yaml` refactor); Phase 3 can refine this without
///   re-architecting the field.
///
/// - `extends_path`: the raw `@extends` path string if this was a child template.
///
/// # Cache-poisoning invariant (A1)
///
/// A file may be resolved as a skeleton base before it is also compiled as a standalone
/// entry point (or vice-versa). The cache key is the normalized file key in both cases.
/// A skeleton entry (`is_skeleton = true`) is collect-only: `process_module_skeleton`
/// does NOT run standalone validate/evaluate, so `prompt_body = None`. A standalone entry
/// (`is_skeleton = false`) carries the rendered `prompt_body`.
///
/// `prompt_body.is_none()` is NOT a reliable skeleton signal — `process_module`
/// (non-skeleton) also yields `None` for an empty/whitespace-only body. The explicit
/// `is_skeleton` flag is the discriminator.
///
/// Caching rules:
/// - Standalone-first then base: the standalone entry already has everything a base needs
///   (`effective_skeleton` / `effective_blocks` are populated on every entry), so extending
///   children reuse it via Arc-sharing. The cache hit returns it as-is.
/// - Skeleton-first then standalone: returning the skeleton entry would yield EMPTY output
///   for the base (it was never evaluated). `resolve_by_key` detects `is_skeleton` on the
///   cache hit, performs the full compile, and upgrades the entry in place — reusing the
///   skeleton's `effective_skeleton` / `effective_blocks` Arcs so descendants that already
///   `Arc::clone`'d them keep pointer-identity.
#[derive(Debug, Clone)]
pub struct ResolvedModule {
    pub(crate) functions: HashMap<String, Arc<FunctionDef>>,
    pub(crate) prompt_body: Option<String>,
    pub(crate) raw_frontmatter: Option<String>,
    pub(crate) has_explicit_exports: bool,
    pub(crate) explicit_exports: HashSet<String>,
    /// Root-ancestor body, Arc-shared across all descendants (never deep-cloned).
    /// For non-extending modules: own body. For extending: Arc::clone of base's skeleton.
    pub(crate) effective_skeleton: Arc<[Node]>,
    /// Fully-overridden block map for this subtree. Seeded from own @block declarations
    /// (non-extending) or clone(base.effective_blocks)+child overrides (extending).
    pub(crate) effective_blocks: IndexMap<String, Arc<BlockNode>>,
    /// Parsed YAML frontmatter mapping. Reserved-key splitting deferred to Phase 3.
    pub(crate) frontmatter_values: Option<serde_yaml_ng::Mapping>,
    /// The raw @extends path, if this was a child template.
    // Used by Phase 3 (reserved-key exclusion for the `extends` key) and Phase 5 (diagnostics).
    #[allow(dead_code)]
    pub(crate) extends_path: Option<String>,
    /// `true` when this entry was produced by `process_module_skeleton` (resolved as an
    /// `@extends` base: collect-only, NO standalone validate/evaluate, `prompt_body = None`).
    ///
    /// Cache-poisoning guard (A1): a skeleton entry must NOT be returned to a caller that
    /// needs a fully-rendered standalone module. `resolve_by_key` detects this flag on a
    /// cache hit and upgrades the entry to a full compile, so the SAME file resolved first
    /// as a base and later as a standalone target yields correct output. (Messages mode
    /// never caches its entry module — `resolve_key_messages` always re-computes — so the
    /// poisoning window only exists on the text/`resolve_by_key` path.)
    pub(crate) is_skeleton: bool,
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
        // Step 1: cache hit — return immediately without reading, UNLESS the cached
        // entry is a skeleton (resolved as an @extends base: collect-only, never
        // validated/evaluated standalone, prompt_body = None). A skeleton entry must
        // not be served to a standalone-compile caller — that would yield empty output
        // for the base. Fall through to a full compile and upgrade the entry in place,
        // reusing the skeleton's Arcs so existing descendants keep Arc-sharing (A1).
        if let Some(cached) = self.modules.get(key) {
            if !cached.is_skeleton {
                return Ok(Arc::clone(cached));
            }
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
        let mut resolved = Self::check_lifo_pop(resolved, popped, key)?;

        // A1 upgrade: if a skeleton entry was previously cached for this key, reuse its
        // Arc-shared skeleton/blocks so descendants that already Arc::clone'd them keep
        // pointer-identity. The freshly compiled `resolved` carries the correct
        // prompt_body (the skeleton-vs-standalone difference is solely validate/evaluate).
        if let Some(prev) = self.modules.get(key) {
            if prev.is_skeleton {
                resolved.effective_skeleton = Arc::clone(&prev.effective_skeleton);
                resolved.effective_blocks = prev.effective_blocks.clone();
            }
        }

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

    /// Resolve a module by its normalized virtual key in messages mode.
    ///
    /// Like [`resolve_key`] but runs `evaluate_messages` instead of `evaluate`,
    /// returning structured `EvalMessage` values from `@message` blocks.
    ///
    /// Mirrors `resolve_source_messages`: a single `process_module_messages` pass
    /// over the entry module (no prior text-mode evaluation).  Imported sub-modules
    /// are resolved through the normal cache (`resolve_by_key`) inside
    /// `collect_definitions_and_imports`, so they are evaluated only once.
    pub fn resolve_key_messages(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<EvalMessage>, MdsError> {
        // Cycle detection: if this key is already on the resolving stack it forms
        // a circular import that must be rejected.
        if self.resolving.contains(key) {
            let cycle = build_cycle_string(&self.resolving, key);
            return Err(MdsError::circular_import(cycle));
        }

        self.check_import_depth()?;

        let source = self.fs.read(key)?;
        let is_md = self.fs.is_markdown(key);
        validate_file_type(key, &source)?;

        self.resolving.insert(key.to_string());

        let ctx = ModuleCtx {
            file_str: key,
            source: &source,
            base_key: key,
            runtime_vars,
        };
        let result = self.process_module_messages(&ctx, is_md, warnings);

        let popped = self.resolving.pop();
        Self::check_lifo_pop(result, popped, key)
    }

    /// Resolve a module from an in-memory source string in messages mode.
    ///
    /// Like [`resolve_source`] but runs `evaluate_messages` instead of `evaluate`.
    pub fn resolve_source_messages(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<EvalMessage>, MdsError> {
        let canonical_str = self.fs.canonicalize(base_dir)?;
        self.fs.set_root(&canonical_str)?;
        let base_key = format!("{canonical_str}/<source>");
        self.check_import_depth()?;
        self.resolving.insert(base_key.clone());
        let ctx = ModuleCtx {
            file_str: "<source>",
            source,
            base_key: &base_key,
            runtime_vars,
        };
        let result = self.process_module_messages(&ctx, false, warnings);
        let popped = self.resolving.pop();
        Self::check_lifo_pop(result, popped, &base_key)
    }

    /// Common messages-mode processing: tokenize, parse, build scope, collect messages.
    ///
    /// Shares setup with `process_module` but calls `evaluate_messages` at the end.
    /// When the parsed module has an `@extends` directive the shared extends pipeline
    /// (`resolve_extends_components`) builds `final_body` and `scope` identically to
    /// text mode — then the `has_message_block` guard and `evaluate_messages` are called
    /// on `final_body` (NOT `module.body`), so @message blocks inside base @block
    /// defaults are correctly detected (avoids PF-004 divergence, decision #8).
    fn process_module_messages(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<EvalMessage>, MdsError> {
        let tokens = tokenize(ctx.source, ctx.file_str)?;
        let module = parse_with_ctx(&tokens, ctx.file_str, ctx.source)?;

        // ── Extends branch (decision #8) ─────────────────────────────────────
        // When the child has @extends, delegate to the shared extends pipeline so that:
        // - PF-004 (avoids PF-004): oversized-base guard fires via resolve_by_key_skeleton.
        // - has_message_block is checked against final_body (base+overrides spliced), not module.body.
        // - Scope and final_body are assembled identically to text mode (no drift).
        if let Some(ext) = module.extends.clone() {
            let frontmatter_values = parse_frontmatter_mapping(module.frontmatter.as_ref())?;
            let ExtendsComponents {
                final_body,
                mut scope,
                ..
            } =
                self.resolve_extends_components(&module, &ext, ctx, &frontmatter_values, warnings)?;

            // Check @message presence against final_body (NOT module.body): a base whose
            // @message blocks live inside @block defaults is correctly detected after splice.
            // (ADR-016: re-validate dynamically-assembled content at the leaf.)
            if !has_message_block(&final_body) {
                return Err(MdsError::syntax(
                    "compile_messages requires at least one @message block, \
                     but none were found in the template",
                ));
            }

            return evaluate_messages(&final_body, &mut scope, warnings);
        }

        // ── Standalone (non-extending) path ──────────────────────────────────

        let (mut scope, fm_imports) =
            build_scope_from_frontmatter(module.frontmatter.as_ref(), is_md, ctx.runtime_vars)?;

        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            explicit_exports,
            ..
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        // Validate that all named exports refer to defined functions or "prompt" —
        // mirrors process_module exactly so @export <undefined> errors in messages mode
        // the same way it does in text mode (avoids PF-004: alternate path bypassing a check).
        validate_exports(&explicit_exports, &functions)?;

        // Register collected functions in scope for @define calls within @message bodies.
        for (name, func) in &functions {
            scope.set_function(name, Arc::clone(func));
        }

        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        // Check that at least one @message block exists before evaluating.
        if !has_message_block(&module.body) {
            return Err(MdsError::syntax(
                "compile_messages requires at least one @message block, \
                 but none were found in the template",
            ));
        }

        evaluate_messages(&module.body, &mut scope, warnings)
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

        // Parse frontmatter YAML once for both scope building and storage.
        let frontmatter_values = parse_frontmatter_mapping(module.frontmatter.as_ref())?;

        // Branch: child template (@extends) vs. standalone module.
        if let Some(ext) = module.extends.clone() {
            return self.process_module_extends(
                module,
                ext,
                ctx,
                is_md,
                raw_frontmatter,
                frontmatter_values,
                warnings,
            );
        }

        // ── Standalone (non-extending) path ──────────────────────────────────

        // Build scope from frontmatter + runtime vars; extract any frontmatter imports.
        let (mut scope, fm_imports) =
            build_scope_from_frontmatter(module.frontmatter.as_ref(), is_md, ctx.runtime_vars)?;

        // Resolve frontmatter imports BEFORE body imports (per spec, ADR-014).
        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        // Walk the AST: collect @define functions (with closure capture), process imports/exports
        let CollectedDefs {
            functions,
            has_explicit_exports,
            explicit_exports,
            block_names,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        // Validate that all named exports refer to defined functions or "prompt"
        validate_exports(&explicit_exports, &functions)?;

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        // Evaluate the body to get prompt text
        let prompt_body = evaluate(&module.body, &mut scope, warnings)?;
        let prompt_body = (!prompt_body.trim().is_empty()).then_some(prompt_body);

        // Build effective_skeleton from this module's own body (Arc-shared, no deep-clone, P1).
        let effective_skeleton: Arc<[Node]> = Arc::from(module.body.as_slice());

        // Build effective_blocks from this module's own @block declarations.
        let effective_blocks = block_names
            .iter()
            .filter_map(|name| {
                module.body.iter().find_map(|n| {
                    if let Node::Block(b) = n {
                        if b.name == *name {
                            return Some((name.clone(), Arc::new(b.clone())));
                        }
                    }
                    None
                })
            })
            .collect::<IndexMap<_, _>>();

        Ok(ResolvedModule {
            functions,
            prompt_body,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            frontmatter_values,
            extends_path: None,
            is_skeleton: false,
        })
    }

    /// Shared extends-pipeline: steps 3a-3e are identical for text and messages modes.
    ///
    /// Builds the `final_body` (splice of base skeleton with effective block overrides)
    /// and the `scope` (deep-merged frontmatter + FM imports + functions) needed by
    /// both `process_module_extends` (text) and `process_module_messages` (messages).
    ///
    /// Callers differ only in the terminal step (step 3f):
    /// - Text mode:     `validate` → `evaluate(&final_body, …)`
    /// - Messages mode: `has_message_block` guard → `evaluate_messages(&final_body, …)`
    ///
    /// Factoring here enforces that BOTH modes go through the same PF-004-safe
    /// `resolve_by_key_skeleton` path for the base, and share one copy of the
    /// scope-construction pipeline (ADR-016: re-validate at the leaf; decision #3/7).
    fn resolve_extends_components(
        &mut self,
        module: &crate::ast::Module,
        ext: &crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        frontmatter_values: &Option<serde_yaml_ng::Mapping>,
        warnings: &mut Vec<String>,
    ) -> Result<ExtendsComponents, MdsError> {
        // ── Step 3a: validate and resolve the base in skeleton mode ──────────
        validate_import_path(&ext.path)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;

        let base_key = self
            .fs
            .normalize(ctx.base_key, &ext.path)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;

        // PF-004 (avoids PF-004): resolve through resolve_by_key_skeleton so cycle
        // detection, MAX_IMPORT_DEPTH, dependency tracking, and MAX_FILE_SIZE all apply.
        // This guard holds for BOTH text and messages modes — they share this path.
        let base = self
            .resolve_by_key_skeleton(&base_key, ctx.runtime_vars, warnings)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;

        // ── Step 3b: child-only-blocks check ─────────────────────────────────
        // Every top-level node in module.body must be Node::Block or whitespace-only Text.
        // (Frontmatter and @extends are already split out of module.body by the parser.)
        check_child_only_blocks(&module.body, ctx)?;

        // ── Step 3c: build effective_blocks from base, applying child overrides ──
        // Clone base's map first (diamond-inheritance safe — never mutate cached base).
        let effective_blocks = apply_block_overrides(&base.effective_blocks, &module.body, ctx)?;

        // effective_skeleton is the root ancestor's body (Arc::clone — O(1), no deep-copy, P1).
        let effective_skeleton = Arc::clone(&base.effective_skeleton);

        // ── Step 3d: build merged scope (Phase 3: deep merge + per-file FM imports) ──
        // Applies decision #3 (base < child < runtime) and decision #7 (reserved-key
        // exclusion, array wholesale replace, both sets of FM imports resolved per-file).

        // 3d-i: Extract frontmatter imports from BOTH base and child BEFORE the deep merge.
        //
        // Base imports resolve relative to the BASE file (using base_key as ctx.base_key).
        // Child imports resolve relative to the CHILD file (using ctx.base_key as usual).
        // Both sets are resolved; a duplicate alias across base+child → mds::name_collision
        // (ADR-014; consistent with the existing namespace-collision handling).
        let base_fm_imports: Vec<FrontmatterImport> = base
            .frontmatter_values
            .as_ref()
            .and_then(|m| m.get("imports"))
            .map(parse_frontmatter_imports_from_yaml)
            .transpose()?
            .unwrap_or_default();

        let child_fm_imports: Vec<FrontmatterImport> = frontmatter_values
            .as_ref()
            .and_then(|m| m.get("imports"))
            .map(parse_frontmatter_imports_from_yaml)
            .transpose()?
            .unwrap_or_default();

        // 3d-ii: Deep-merge base and child frontmatter value Mappings.
        // Empty mapping used when either side has no frontmatter.
        // Reserved keys (imports, type, extends) are excluded by deep_merge_yaml.
        // Precedence: base < child (child overrides base on collision).
        let empty_mapping = serde_yaml_ng::Mapping::new();
        let base_mapping = base.frontmatter_values.as_ref().unwrap_or(&empty_mapping);
        let child_mapping = frontmatter_values.as_ref().unwrap_or(&empty_mapping);
        let merged_mapping = deep_merge_yaml(base_mapping, child_mapping, 0)?;

        // 3d-iii: Build scope from the merged mapping. Runtime vars applied LAST
        // (base < child < runtime, F7, decision #3).
        let mut scope = build_scope_from_merged_mapping(&merged_mapping, ctx.runtime_vars)?;

        // 3d-iv: Resolve base frontmatter imports against base_key (ADR-014 ordering,
        // PF-004 safe via resolve_frontmatter_imports → resolve_import_from).
        // Use a minimal ctx pointing to the base file.
        let base_ctx = ModuleCtx {
            file_str: &base_key,
            source: "",
            base_key: &base_key,
            runtime_vars: ctx.runtime_vars,
        };
        self.resolve_frontmatter_imports(&base_fm_imports, &mut scope, &base_ctx, warnings)?;

        // 3d-v: Resolve child frontmatter imports against child key (ctx.base_key).
        // Duplicate alias across base+child → mds::name_collision (same error as today).
        self.resolve_frontmatter_imports(&child_fm_imports, &mut scope, ctx, warnings)?;

        // 3d-vi: Merge base functions into scope (F12: base default block calling a base @define).
        // Collision with child frontmatter-imported functions → name_collision.
        for (name, func) in &base.functions {
            if scope.get_function(name).is_some() {
                return Err(MdsError::name_collision(name.clone()));
            }
            scope.set_function(name, Arc::clone(func));
        }

        // Collect child's own definitions from its body (currently zero @define after
        // child-only-blocks check, but structurally correct).
        let CollectedDefs {
            functions: child_functions,
            has_explicit_exports,
            explicit_exports,
            block_names: _,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        // Merge child-defined functions over base (child wins).
        let mut functions = base.functions.clone();
        for (name, func) in child_functions {
            functions.insert(name, func);
        }

        validate_exports(&explicit_exports, &functions)?;

        // ── Step 3e: splice final_body ────────────────────────────────────────
        // Linear O(S+B) pass over the skeleton. Each Block in the skeleton is replaced
        // by its effective body from effective_blocks (O(1) lookup). Non-Block nodes
        // pass through verbatim. Between-block spacing (Text nodes) is preserved (decision #9, F11).
        let final_body = splice_skeleton(&effective_skeleton, &effective_blocks);

        Ok(ExtendsComponents {
            final_body,
            scope,
            functions,
            effective_skeleton,
            effective_blocks,
            has_explicit_exports,
            explicit_exports,
        })
    }

    /// Evaluate an extending child template in text mode.
    ///
    /// Delegates the shared pipeline (steps 3a-3e) to `resolve_extends_components`,
    /// then runs `validator::validate` + `evaluate` on `final_body` (step 3f).
    ///
    /// Decision #2: base is NEVER validated/evaluated standalone — deferred to leaf.
    /// PF-004: base is read via resolve_by_key_skeleton (FileSystem trait, never std::fs).
    #[allow(clippy::too_many_arguments)]
    fn process_module_extends(
        &mut self,
        module: crate::ast::Module,
        ext: crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        _is_md: bool,
        raw_frontmatter: Option<String>,
        frontmatter_values: Option<serde_yaml_ng::Mapping>,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        let ExtendsComponents {
            final_body,
            mut scope,
            functions,
            effective_skeleton,
            effective_blocks,
            has_explicit_exports,
            explicit_exports,
        } = self.resolve_extends_components(&module, &ext, ctx, &frontmatter_values, warnings)?;

        // ── Step 3f: validate + evaluate on final_body ────────────────────────
        // Operates on final_body, NOT module.body. This is what makes E12 work:
        // a base default block referencing an undefined var is caught HERE against
        // the merged leaf scope. (ADR-016: re-validate dynamically-assembled content.)
        validator::validate(&final_body, &mut scope, ctx.file_str, ctx.source)?;

        let prompt_body = evaluate(&final_body, &mut scope, warnings)?;
        let prompt_body = (!prompt_body.trim().is_empty()).then_some(prompt_body);

        Ok(ResolvedModule {
            functions,
            prompt_body,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            frontmatter_values,
            extends_path: Some(ext.path),
            is_skeleton: false,
        })
    }

    /// Resolve a module in skeleton mode: tokenize → parse → collect only (no validate/evaluate).
    ///
    /// Uses the same module cache and resolving stack as resolve_by_key, so cycle detection
    /// (mds::circular_import), MAX_IMPORT_DEPTH, dependency tracking, and the MAX_FILE_SIZE
    /// guard all apply automatically (decision #1, PF-004).
    ///
    /// Cache-poisoning invariant: both skeleton and full-compile entries are stored under the
    /// same normalized key. The first resolution wins. See ResolvedModule doc comment for details.
    fn resolve_by_key_skeleton(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        // Cache hit — return immediately (full or skeleton entry, both are valid bases).
        if let Some(cached) = self.modules.get(key) {
            return Ok(Arc::clone(cached));
        }

        // Cycle detection — same resolving stack as resolve_by_key (decision #1, E5).
        if self.resolving.contains(key) {
            let cycle = build_cycle_string(&self.resolving, key);
            return Err(MdsError::circular_import(cycle));
        }

        // Depth guard (E6).
        self.check_import_depth()?;

        // PF-004: read via FileSystem trait — NEVER std::fs.
        let source = self.fs.read(key)?;
        let is_md = self.fs.is_markdown(key);
        validate_file_type(key, &source)?;

        self.resolving.insert(key.to_string());

        let ctx = ModuleCtx {
            file_str: key,
            source: &source,
            base_key: key,
            runtime_vars,
        };
        let resolved = self.process_module_skeleton(&ctx, is_md, warnings);

        let popped = self.resolving.pop();
        let resolved = Self::check_lifo_pop(resolved, popped, key)?;

        let arc = Arc::new(resolved);
        self.modules.insert(key.to_string(), Arc::clone(&arc));
        Ok(arc)
    }

    /// Tokenize → parse → collect (functions/blocks/frontmatter), NO validate/evaluate.
    ///
    /// Called when this file is a base for @extends. The resulting ResolvedModule has
    /// prompt_body = None. All fields required by extending children are populated.
    fn process_module_skeleton(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        let tokens = tokenize(ctx.source, ctx.file_str)?;
        let module = parse_with_ctx(&tokens, ctx.file_str, ctx.source)?;

        let raw_frontmatter = module.frontmatter.as_ref().map(|fm| fm.raw.clone());
        // own_fm_values: this module's raw frontmatter (not yet merged with ancestors).
        let own_fm_values = parse_frontmatter_mapping(module.frontmatter.as_ref())?;

        // Build scope for @define closure capture (base functions must be available).
        let (mut scope, fm_imports) =
            build_scope_from_frontmatter(module.frontmatter.as_ref(), is_md, ctx.runtime_vars)?;
        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            has_explicit_exports,
            explicit_exports,
            block_names,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        // Multi-level chain (A←B←C): B may itself extend A.
        // B's effective_skeleton = A's effective_skeleton (Arc::clone, O(1) fold).
        // B's effective_blocks = clone(A.effective_blocks) + B's overrides (most-derived wins, F3).
        //
        // Phase 3: B's frontmatter_values must be the transitive deep-merge of A's accumulated FM
        // with B's own FM (A < B), so that when C later merges against B, it gets A+B+C.
        let (effective_skeleton, effective_blocks, frontmatter_values) =
            if let Some(ext) = module.extends.as_ref() {
                validate_import_path(&ext.path).map_err(|e| {
                    attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset)
                })?;
                let grandparent_key = self.fs.normalize(ctx.base_key, &ext.path).map_err(|e| {
                    attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset)
                })?;
                let grandparent = self
                    .resolve_by_key_skeleton(&grandparent_key, ctx.runtime_vars, warnings)
                    .map_err(|e| {
                        attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset)
                    })?;

                // Child-only-blocks check for this intermediate base (3b).
                check_child_only_blocks(&module.body, ctx)?;

                let eff_blocks =
                    apply_block_overrides(&grandparent.effective_blocks, &module.body, ctx)?;

                // Phase 3: transitive FM merge: grandparent.frontmatter_values < own_fm_values.
                // This produces the accumulated FM for this intermediate base, so a leaf
                // descending from it gets the full transitive chain without re-traversing.
                let empty = serde_yaml_ng::Mapping::new();
                let gp_fm = grandparent.frontmatter_values.as_ref().unwrap_or(&empty);
                let own_fm = own_fm_values.as_ref().unwrap_or(&empty);
                let merged_fm = deep_merge_yaml(gp_fm, own_fm, 0)?;
                let accumulated_fm = if merged_fm.is_empty() {
                    None
                } else {
                    Some(merged_fm)
                };

                (
                    Arc::clone(&grandparent.effective_skeleton),
                    eff_blocks,
                    accumulated_fm,
                )
            } else {
                // Root base: own body is the skeleton; blocks seeded from own @block declarations.
                let eff_skeleton: Arc<[Node]> = Arc::from(module.body.as_slice());
                let eff_blocks = module
                    .body
                    .iter()
                    .filter_map(|n| {
                        if let Node::Block(b) = n {
                            block_names
                                .contains(&b.name)
                                .then(|| (b.name.clone(), Arc::new(b.clone())))
                        } else {
                            None
                        }
                    })
                    .collect::<IndexMap<_, _>>();
                // Root base: frontmatter_values is its own raw FM (no ancestors).
                (eff_skeleton, eff_blocks, own_fm_values)
            };

        Ok(ResolvedModule {
            functions,
            prompt_body: None,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            frontmatter_values,
            extends_path: module.extends.map(|e| e.path),
            is_skeleton: true,
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
            block_names: HashSet::new(),
        };

        let mut block_count: usize = 0;
        for node in body {
            match node {
                Node::Define(def) => collect_define(def, &mut defs, scope, ctx)?,
                Node::Import(import) => self.resolve_import(import, scope, ctx, warnings)?,
                Node::Export(export) => self.collect_export(export, &mut defs, ctx, warnings)?,
                Node::Block(block) => {
                    block_count += 1;
                    collect_block(block, &mut defs, block_count, ctx)?;
                }
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
        if scope.get_namespace(alias).is_some() {
            return Err(MdsError::name_collision(alias.to_string()));
        }
        let resolved = self
            .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        scope.set_namespace(alias, resolved.to_namespace());
        Ok(())
    }

    /// Resolve all frontmatter imports, populating `scope` in the same order
    /// as the declarations. Frontmatter imports run before body imports.
    fn resolve_frontmatter_imports(
        &mut self,
        imports: &[FrontmatterImport],
        scope: &mut Scope,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(), MdsError> {
        for (i, imp) in imports.iter().enumerate() {
            match imp {
                FrontmatterImport::Alias { path, alias } => {
                    if scope.get_namespace(alias).is_some() {
                        return Err(MdsError::name_collision(alias.to_string()));
                    }
                    let resolved = self
                        .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    scope.set_namespace(alias, resolved.to_namespace());
                }
                FrontmatterImport::Merge { path } => {
                    let resolved = self
                        .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
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
                FrontmatterImport::Selective { path, names } => {
                    let resolved = self
                        .resolve_import_from(ctx.base_key, path, ctx.runtime_vars, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    let not_exported = |name: &str| {
                        MdsError::import_error(format!(
                            "'{name}' is not exported from '{path}' (in frontmatter imports[{i}])"
                        ))
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
        }
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
    /// Tracks declared `@block` names for duplicate-block and block-vs-function collision detection.
    ///
    /// Shared with `collect_define` so that a `@block foo:` and a `@define foo()` in the same
    /// module surface as `mds::name_collision` (same namespace — decision #10).
    block_names: HashSet<String>,
}

/// Shared output of [`ModuleCache::resolve_extends_components`].
///
/// Steps 3a-3e (base resolution, child-only-blocks check, effective-blocks construction,
/// scope merge, and skeleton splice) are identical for text and messages modes. This struct
/// carries those results so the two terminal steps differ only in the final evaluate call:
/// - Text mode:     `validator::validate` → `evaluate(&final_body, …)`
/// - Messages mode: `has_message_block` guard → `evaluate_messages(&final_body, …)`
struct ExtendsComponents {
    /// Spliced final body: base skeleton with effective block bodies inlined.
    final_body: Vec<Node>,
    /// Merged scope (base < child < runtime), with FM imports and functions loaded.
    scope: Scope,
    /// Merged function map (base functions + child overrides).
    functions: HashMap<String, Arc<FunctionDef>>,
    /// Root ancestor skeleton, Arc-shared (O(1), no deep-clone).
    effective_skeleton: Arc<[Node]>,
    /// Fully-overridden block map for this subtree.
    effective_blocks: IndexMap<String, Arc<BlockNode>>,
    /// Whether the child declared any `@export` directives.
    has_explicit_exports: bool,
    /// Named exports from the child.
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

/// Return `true` when the AST body contains at least one `@message` block
/// (possibly nested inside `@if` or `@for` — a shallow scan is enough for the
/// "no messages at all" guard; evaluation handles deeper nesting).
fn has_message_block(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Message(_) => true,
        Node::If(block) => {
            has_message_block(&block.then_body)
                || block
                    .elseif_branches
                    .iter()
                    .any(|(_, body)| has_message_block(body))
                || block
                    .else_body
                    .as_deref()
                    .map(has_message_block)
                    .unwrap_or(false)
        }
        Node::For(block) => has_message_block(&block.body),
        // A @block's default body may contain @message blocks.
        Node::Block(block) => has_message_block(&block.body),
        _ => false,
    })
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
    if defs.functions.contains_key(&def.name) || defs.block_names.contains(&def.name) {
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

/// Process a single `@block` directive, updating the `block_names` set in `defs`.
///
/// Detects:
/// - Duplicate `@block` names within the same module (mds::name_collision).
/// - `@block` name colliding with a `@define` function name (mds::name_collision).
///   This is intentional: blocks and functions share the same namespace (decision #10).
/// - `MAX_BLOCKS_PER_MODULE` cap (mds::resource_limit).
///
/// Note: `count` is the running total of @block nodes seen so far (1-indexed after increment
/// at the call site), used to enforce the per-module cap.
fn collect_block(
    block: &BlockNode,
    defs: &mut CollectedDefs,
    count: usize,
    ctx: &ModuleCtx<'_>,
) -> Result<(), MdsError> {
    if count > MAX_BLOCKS_PER_MODULE {
        return Err(MdsError::resource_limit(format!(
            "module has more than {MAX_BLOCKS_PER_MODULE} @block declarations"
        )));
    }
    // Check for duplicate @block name or collision with an existing @define.
    if defs.block_names.contains(&block.name) || defs.functions.contains_key(&block.name) {
        return Err(MdsError::name_collision_at(
            &block.name,
            ctx.file_str,
            ctx.source,
            block.offset,
            block.name.len(),
        ));
    }
    defs.block_names.insert(block.name.clone());
    Ok(())
}

/// Build a scope from optional frontmatter and runtime variable overrides.
///
/// Parses frontmatter YAML (if present), populates scope with variables,
/// then applies runtime_vars to override any frontmatter keys.
/// The `type` key is skipped for `.md` files (it is a file-type marker, not a template var).
///
/// For MDS files (`.mds` or `.md` with `type: mds`), the `imports` key is extracted
/// and returned as a `Vec<FrontmatterImport>` rather than being set as a variable.
/// For plain `.md` files, `imports` is treated as a regular variable.
///
/// Returns `(scope, fm_imports)`.
fn build_scope_from_frontmatter(
    frontmatter: Option<&crate::ast::Frontmatter>,
    is_md: bool,
    runtime_vars: &HashMap<String, Value>,
) -> Result<(Scope, Vec<FrontmatterImport>), MdsError> {
    let mut scope = Scope::new();
    let mut fm_imports: Vec<FrontmatterImport> = Vec::new();

    // A .mds file is always MDS; a .md file is MDS only when its frontmatter
    // contains `type: mds`. Determine this early — it gates both frontmatter
    // parsing and the runtime_vars guard below.
    let is_mds = !is_md || frontmatter.is_some_and(|fm| has_type_mds_frontmatter_raw(&fm.raw));

    if let Some(fm) = frontmatter {
        // Parse YAML once to avoid double-parsing
        let yaml: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&fm.raw).map_err(|e| MdsError::yaml_error(e.to_string()))?;

        if let serde_yaml_ng::Value::Mapping(map) = yaml {
            for (key, val) in map {
                let serde_yaml_ng::Value::String(key_str) = key else {
                    continue;
                };
                if key_str == "type" && is_md {
                    // Skip the 'type' meta-field for .md files (it's a file-type marker)
                    continue;
                }
                if key_str == "imports" {
                    if is_mds {
                        // Parse as structured import declarations, not a scope variable
                        fm_imports = parse_frontmatter_imports_from_yaml(&val)?;
                    } else {
                        // Plain .md: treat `imports` as a regular variable
                        let value = Value::from_yaml(val)?;
                        scope.set_var(&key_str, value);
                    }
                    continue;
                }
                let value = Value::from_yaml(val)?;
                scope.set_var(&key_str, value);
            }
        }
    }

    // Apply runtime vars (override frontmatter)
    for (key, value) in runtime_vars {
        if key == "imports" && is_mds {
            // MDS files (.mds or .md with type:mds) treat `imports` as a reserved
            // key; block --set imports=... for them.
            return Err(MdsError::import_error(
                "'imports' is a reserved frontmatter key for MDS files and cannot be set \
                 via --set",
            ));
        }
        scope.set_var(key, value.clone());
    }

    Ok((scope, fm_imports))
}

/// Deep-merge two YAML `Mapping`s for template inheritance frontmatter.
///
/// Semantics (decision #7):
/// - When BOTH values at a key are `Mapping`, recursively merge key-by-key.
/// - Otherwise child wins (scalar over scalar, scalar over map, map over scalar).
/// - Arrays/sequences REPLACE WHOLESALE — no element-level merge.
/// - Key ORDER: base-then-child (determinism A6). Keys present in base keep their
///   original position; their value may be replaced by the merged/child value.
///   Child-only keys are appended in child order after all base keys.
/// - Reserved keys (`imports`, `type`, `extends`) are excluded from the output —
///   they are not value data (decision #7). Callers handle them separately.
/// - Recursion is bounded by `MAX_FRONTMATTER_MERGE_DEPTH`; exceeding it returns
///   `mds::resource_limit` (P4 — no stack overflow).
///
/// The `depth` argument starts at 0 and is incremented on each recursive call.
fn deep_merge_yaml(
    base: &serde_yaml_ng::Mapping,
    child: &serde_yaml_ng::Mapping,
    depth: usize,
) -> Result<serde_yaml_ng::Mapping, MdsError> {
    if depth > MAX_FRONTMATTER_MERGE_DEPTH {
        return Err(MdsError::resource_limit(format!(
            "frontmatter merge depth exceeds maximum of {MAX_FRONTMATTER_MERGE_DEPTH}"
        )));
    }

    // Reserved keys excluded from the merged output.
    const RESERVED: &[&str] = &["imports", "type", "extends"];

    let mut result = serde_yaml_ng::Mapping::new();

    // Phase 1: walk base keys in order.
    // Each base key keeps its position; value is replaced if child also has that key.
    for (base_key, base_val) in base {
        // Skip reserved keys and non-string keys.
        let serde_yaml_ng::Value::String(key_str) = base_key else {
            continue;
        };
        if RESERVED.contains(&key_str.as_str()) {
            continue;
        }

        let merged_val = if let Some(child_val) = child.get(base_key) {
            // Both have this key: recurse if both are Mapping, else child wins.
            match (base_val, child_val) {
                (serde_yaml_ng::Value::Mapping(bm), serde_yaml_ng::Value::Mapping(cm)) => {
                    let merged_map = deep_merge_yaml(bm, cm, depth + 1)?;
                    serde_yaml_ng::Value::Mapping(merged_map)
                }
                // Child wins for all other combinations (including arrays — replace wholesale).
                (_, other) => other.clone(),
            }
        } else {
            // Base-only key: include as-is.
            base_val.clone()
        };

        result.insert(base_key.clone(), merged_val);
    }

    // Phase 2: append child-only keys in child order.
    for (child_key, child_val) in child {
        let serde_yaml_ng::Value::String(key_str) = child_key else {
            continue;
        };
        if RESERVED.contains(&key_str.as_str()) {
            continue;
        }
        // Skip keys already added from base.
        if result.contains_key(child_key) {
            continue;
        }
        result.insert(child_key.clone(), child_val.clone());
    }

    Ok(result)
}

/// Build a scope from a pre-merged `Mapping` and runtime variable overrides.
///
/// Used by the template inheritance path after `deep_merge_yaml` has already
/// excluded reserved keys (`imports`, `type`, `extends`). The mapping is pure
/// value data — no reserved-key handling needed here.
///
/// Runtime vars are applied LAST so precedence is: base < child < runtime (F7).
fn build_scope_from_merged_mapping(
    mapping: &serde_yaml_ng::Mapping,
    runtime_vars: &HashMap<String, Value>,
) -> Result<Scope, MdsError> {
    let mut scope = Scope::new();

    for (key, val) in mapping {
        let serde_yaml_ng::Value::String(key_str) = key else {
            continue;
        };
        let value = Value::from_yaml(val.clone())?;
        scope.set_var(key_str, value);
    }

    // Runtime vars override everything (base < child < runtime, F7, decision #3).
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

/// Return `true` if a frontmatter line declares `type: mds` at the top level.
///
/// Only non-indented lines are matched, consistent with `strip_reserved_keys`
/// which guards with `!starts_with(char::is_whitespace)`. Recognises bare,
/// single-quoted, and double-quoted YAML values.
fn is_type_mds_line(line: &str) -> bool {
    !line.starts_with(char::is_whitespace)
        && line
            .strip_prefix("type:")
            .is_some_and(|v| matches!(v.trim(), "mds" | "\"mds\"" | "'mds'"))
}

/// Return `true` if `source` has a YAML frontmatter block containing `type: mds`.
///
/// Checks without a full YAML parse by scanning frontmatter lines for the key.
fn has_type_mds_frontmatter(source: &str) -> bool {
    source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))
        .and_then(|after_fence| after_fence.find("\n---").map(|end| &after_fence[..end]))
        .is_some_and(|fm| fm.lines().any(is_type_mds_line))
}

/// Return `true` if raw frontmatter content (without `---` fences) contains `type: mds`.
///
/// This is the counterpart of [`has_type_mds_frontmatter`] that works on `fm.raw`
/// (the already-extracted frontmatter body) rather than the full source.
fn has_type_mds_frontmatter_raw(raw: &str) -> bool {
    raw.lines().any(is_type_mds_line)
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

/// Attach "(in frontmatter imports[i])" context to errors that have no source span.
///
/// Errors that already carry a span (e.g. cascading errors inside the imported file)
/// are returned unchanged so they continue to report their own locations.
fn attach_frontmatter_index(err: MdsError, i: usize) -> MdsError {
    match err {
        MdsError::FileNotFound {
            path, span: None, ..
        } => MdsError::import_error(format!(
            "file not found: \"{path}\" (in frontmatter imports[{i}])"
        )),
        MdsError::CircularImport {
            cycle, span: None, ..
        } => MdsError::import_error(format!(
            "circular import detected: {cycle} (in frontmatter imports[{i}])"
        )),
        MdsError::ImportError {
            message,
            span: None,
            ..
        } if !message.contains("in frontmatter") => {
            MdsError::import_error(format!("{message} (in frontmatter imports[{i}])"))
        }
        other => other,
    }
}

// ── Template inheritance helpers ──────────────────────────────────────────────

/// Parse the frontmatter YAML into a `serde_yaml_ng::Mapping` for storage.
///
/// Returns `None` when there is no frontmatter or when the YAML is not a mapping.
/// Called once per module to avoid double-parsing.
fn parse_frontmatter_mapping(
    frontmatter: Option<&crate::ast::Frontmatter>,
) -> Result<Option<serde_yaml_ng::Mapping>, MdsError> {
    let Some(fm) = frontmatter else {
        return Ok(None);
    };
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&fm.raw).map_err(|e| MdsError::yaml_error(e.to_string()))?;
    if let serde_yaml_ng::Value::Mapping(map) = yaml {
        Ok(Some(map))
    } else {
        Ok(None)
    }
}

/// Return the byte offset of a node's first token.
///
/// Used to attach error spans to stray top-level nodes in a child template body.
/// Falls back to 0 for node types that don't carry an explicit offset.
fn node_offset(node: &Node) -> usize {
    match node {
        Node::Text(_) | Node::EscapedBrace => 0,
        Node::Interpolation(i) => i.offset,
        Node::If(b) => b.offset,
        Node::For(b) => b.offset,
        Node::Define(b) => b.offset,
        Node::Import(i) => match i {
            crate::ast::ImportDirective::Alias { offset, .. }
            | crate::ast::ImportDirective::Merge { offset, .. }
            | crate::ast::ImportDirective::Selective { offset, .. } => *offset,
        },
        Node::Export(_) => 0,
        Node::Include(i) => i.offset,
        Node::Message(m) => m.offset,
        Node::Block(b) => b.offset,
    }
}

/// Splice the skeleton body by replacing each `@block` placeholder with its
/// effective body (from the `effective_blocks` override map).
///
/// Linear O(S+B) pass: S = skeleton nodes, B = total block body nodes.
/// Between-block spacing (Text nodes) is preserved verbatim (decision #9, F11).
/// Validate that every body node is a `@block` or whitespace-only `@text`.
///
/// Called for both leaf children and intermediate bases in `@extends` chains.
/// Returns `Err(mds::extends)` on the first stray node.
fn check_child_only_blocks(body: &[Node], ctx: &ModuleCtx<'_>) -> Result<(), MdsError> {
    for node in body {
        match node {
            Node::Block(_) => {}
            Node::Text(t) if t.text.trim().is_empty() => {}
            other => {
                let offset = node_offset(other);
                let line_len = ctx.source[offset..]
                    .find('\n')
                    .unwrap_or(ctx.source[offset..].len());
                return Err(MdsError::extends_error_at(
                    "an extending template may contain only @block overrides",
                    ctx.file_str,
                    ctx.source,
                    offset,
                    line_len,
                ));
            }
        }
    }
    Ok(())
}

/// Clone `parent_blocks` and apply the `@block` overrides from `body`.
///
/// Clones the parent map first so the cached parent entry is never mutated
/// (diamond-inheritance correctness, F5).  Returns `Err(mds::extends)` if a
/// child block name is not present in the parent map (E4: unknown override).
fn apply_block_overrides(
    parent_blocks: &IndexMap<String, Arc<BlockNode>>,
    body: &[Node],
    ctx: &ModuleCtx<'_>,
) -> Result<IndexMap<String, Arc<BlockNode>>, MdsError> {
    let mut blocks = parent_blocks.clone();
    for node in body {
        if let Node::Block(b) = node {
            // Decision #6 / F4/E4: child may only override blocks declared by the root base.
            if !blocks.contains_key(&b.name) {
                return Err(MdsError::extends_error_at(
                    "only the root template may declare @block placeholders",
                    ctx.file_str,
                    ctx.source,
                    b.offset,
                    b.name.len(),
                ));
            }
            // Most-derived wins.
            blocks.insert(b.name.clone(), Arc::new(b.clone()));
        }
    }
    Ok(blocks)
}

fn splice_skeleton(
    skeleton: &[Node],
    effective_blocks: &IndexMap<String, Arc<BlockNode>>,
) -> Vec<Node> {
    let mut result = Vec::with_capacity(skeleton.len());
    for node in skeleton {
        if let Node::Block(skeleton_block) = node {
            // Look up the effective block (override or base default) — O(1).
            if let Some(eff_block) = effective_blocks.get(&skeleton_block.name) {
                // Inline the effective body (edges already stripped at parse time).
                result.extend(eff_block.body.clone());
            } else {
                // Unknown block name (shouldn't happen after validation, but safe fallback).
                result.extend(skeleton_block.body.clone());
            }
        } else {
            // Non-block skeleton nodes pass through verbatim.
            result.push(node.clone());
        }
    }
    result
}

// ── Frontmatter imports ───────────────────────────────────────────────────────

/// A single import declaration from YAML frontmatter.
///
/// Three forms mirror the body `@import` directive:
/// - **Alias**: `{ path: "./lib.mds", as: lib }` — imported under a namespace alias.
/// - **Merge**: `{ path: "./lib.mds" }` — all exports merged into the current scope.
/// - **Selective**: `{ path: "./lib.mds", names: [greet, farewell] }` — named exports only.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FrontmatterImport {
    Alias { path: String, alias: String },
    Merge { path: String },
    Selective { path: String, names: Vec<String> },
}

impl FrontmatterImport {
    pub(crate) fn path(&self) -> &str {
        match self {
            Self::Alias { path, .. } | Self::Merge { path } | Self::Selective { path, .. } => path,
        }
    }
}

/// Parse the `imports` key from an already-parsed YAML value.
///
/// `imports_val` must be a YAML Sequence; each element must be a Mapping with
/// a required `path` string key and at most one of `as` (alias) or `names` (selective).
pub(crate) fn parse_frontmatter_imports_from_yaml(
    imports_val: &serde_yaml_ng::Value,
) -> Result<Vec<FrontmatterImport>, MdsError> {
    let serde_yaml_ng::Value::Sequence(seq) = imports_val else {
        return Err(MdsError::import_error(
            "imports must be a YAML sequence (in frontmatter)",
        ));
    };

    if seq.len() > MAX_FRONTMATTER_IMPORTS {
        return Err(MdsError::resource_limit(format!(
            "imports exceeds maximum of {MAX_FRONTMATTER_IMPORTS} entries (in frontmatter)"
        )));
    }

    seq.iter()
        .enumerate()
        .map(|(index, entry)| parse_single_import_entry(entry, index))
        .collect()
}

/// Parse one entry from the `imports` YAML sequence.
///
/// `index` is used solely for error messages.
fn parse_single_import_entry(
    entry: &serde_yaml_ng::Value,
    index: usize,
) -> Result<FrontmatterImport, MdsError> {
    let err =
        |msg: &str| MdsError::import_error(format!("imports[{index}]: {msg} (in frontmatter)"));

    let serde_yaml_ng::Value::Mapping(map) = entry else {
        return Err(err("each entry must be a mapping"));
    };

    // Validate all keys first: reject non-string keys and unknown field names.
    for (k, _) in map {
        let serde_yaml_ng::Value::String(key_str) = k else {
            return Err(err("keys must be strings"));
        };
        match key_str.as_str() {
            "path" | "as" | "names" => {}
            other => return Err(err(&format!("unknown key '{other}'"))),
        }
    }

    // Extract path (required)
    let path_val = map
        .get("path")
        .ok_or_else(|| err("missing required key 'path'"))?;
    let serde_yaml_ng::Value::String(path) = path_val else {
        return Err(err("'path' must be a string"));
    };
    let path = path.clone();

    // Validate path via the same rules as body @import
    validate_import_path(&path).map_err(|_| {
        err(&format!(
            "invalid path \"{path}\": must start with './' or '../'"
        ))
    })?;

    match (map.get("as"), map.get("names")) {
        (Some(_), Some(_)) => Err(err("'as' and 'names' are mutually exclusive")),
        (Some(as_v), None) => parse_alias_entry(as_v, path, &err),
        (None, Some(names_v)) => parse_selective_entry(names_v, path, &err),
        (None, None) => Ok(FrontmatterImport::Merge { path }),
    }
}

/// Parse the alias (`as`) form of a frontmatter import entry.
fn parse_alias_entry(
    as_v: &serde_yaml_ng::Value,
    path: String,
    err: &impl Fn(&str) -> MdsError,
) -> Result<FrontmatterImport, MdsError> {
    let serde_yaml_ng::Value::String(alias) = as_v else {
        return Err(err("'as' must be a string"));
    };
    if !is_valid_identifier(alias) {
        return Err(err(&format!(
            "invalid identifier '{alias}' for 'as': must start with a letter or '_' \
             and contain only alphanumeric characters or '_'"
        )));
    }
    Ok(FrontmatterImport::Alias {
        path,
        alias: alias.clone(),
    })
}

/// Parse the selective (`names`) form of a frontmatter import entry.
fn parse_selective_entry(
    names_v: &serde_yaml_ng::Value,
    path: String,
    err: &impl Fn(&str) -> MdsError,
) -> Result<FrontmatterImport, MdsError> {
    let serde_yaml_ng::Value::Sequence(names_seq) = names_v else {
        return Err(err("'names' must be a sequence"));
    };
    if names_seq.is_empty() {
        return Err(err("names cannot be empty"));
    }
    let mut names = Vec::with_capacity(names_seq.len());
    let mut seen = HashSet::with_capacity(names_seq.len());
    for name_val in names_seq {
        let serde_yaml_ng::Value::String(name) = name_val else {
            return Err(err("each name in 'names' must be a string"));
        };
        // "prompt" is a special export name — allowed without identifier validation
        if name != "prompt" && !is_valid_identifier(name) {
            return Err(err(&format!(
                "invalid identifier '{name}' in 'names': must start with a letter or \
                 '_' and contain only alphanumeric characters or '_'"
            )));
        }
        if !seen.insert(name.as_str()) {
            return Err(err(&format!("duplicate name '{name}' in 'names'")));
        }
        names.push(name.clone());
    }
    Ok(FrontmatterImport::Selective { path, names })
}

/// Parse frontmatter imports from a raw YAML string.
///
/// Returns an empty `Vec` if the `imports` key is absent. Propagates any
/// parse or validation error from [`parse_frontmatter_imports_from_yaml`].
pub(crate) fn parse_frontmatter_imports(raw: &str) -> Result<Vec<FrontmatterImport>, MdsError> {
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(raw).map_err(|e| MdsError::yaml_error(e.to_string()))?;

    let serde_yaml_ng::Value::Mapping(ref map) = yaml else {
        return Ok(vec![]);
    };

    let Some(imports_val) = map.get("imports") else {
        return Ok(vec![]);
    };

    parse_frontmatter_imports_from_yaml(imports_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to build a YAML Value from inline YAML text
    fn yaml(s: &str) -> serde_yaml_ng::Value {
        serde_yaml_ng::from_str(s).expect("valid yaml in test")
    }

    // ── parse_frontmatter_imports_from_yaml ───────────────────────────────────

    #[test]
    fn parse_fm_import_alias() {
        let v = yaml("- path: ./lib.mds\n  as: lib\n");
        let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
        assert_eq!(
            result,
            vec![FrontmatterImport::Alias {
                path: "./lib.mds".into(),
                alias: "lib".into(),
            }]
        );
    }

    #[test]
    fn parse_fm_import_merge() {
        let v = yaml("- path: ./lib.mds\n");
        let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
        assert_eq!(
            result,
            vec![FrontmatterImport::Merge {
                path: "./lib.mds".into()
            }]
        );
    }

    #[test]
    fn parse_fm_import_selective() {
        let v = yaml("- path: ./lib.mds\n  names: [greet, farewell]\n");
        let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
        assert_eq!(
            result,
            vec![FrontmatterImport::Selective {
                path: "./lib.mds".into(),
                names: vec!["greet".into(), "farewell".into()],
            }]
        );
    }

    #[test]
    fn parse_fm_import_multiple() {
        let v = yaml(
            "- path: ./a.mds\n  as: a\n\
             - path: ./b.mds\n\
             - path: ./c.mds\n  names: [f]\n",
        );
        let result = parse_frontmatter_imports_from_yaml(&v).expect("should parse");
        assert_eq!(result.len(), 3);
        assert!(matches!(result[0], FrontmatterImport::Alias { .. }));
        assert!(matches!(result[1], FrontmatterImport::Merge { .. }));
        assert!(matches!(result[2], FrontmatterImport::Selective { .. }));
    }

    #[test]
    fn parse_fm_import_empty_array() {
        let v = yaml("[]");
        let result = parse_frontmatter_imports_from_yaml(&v).expect("empty array is ok");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_fm_no_imports_key() {
        let result =
            parse_frontmatter_imports("name: Alice\ngreeting: hello\n").expect("no imports key");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_fm_err_missing_path() {
        let v = yaml("- as: lib\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("missing path should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("missing required key 'path'") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_path_not_string() {
        let v = yaml("- path: 42\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("non-string path should fail");
        let msg = err.to_string();
        assert!(msg.contains("'path' must be a string"), "got: {msg}");
    }

    #[test]
    fn parse_fm_err_invalid_as_id() {
        let v = yaml("- path: ./lib.mds\n  as: 123bad\n");
        let err =
            parse_frontmatter_imports_from_yaml(&v).expect_err("invalid identifier should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid identifier") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_as_and_names() {
        let v = yaml("- path: ./lib.mds\n  as: lib\n  names: [f]\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("mutually exclusive");
        let msg = err.to_string();
        assert!(
            msg.contains("mutually exclusive") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_unknown_key() {
        let v = yaml("- path: ./lib.mds\n  foo: bar\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("unknown key should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown key 'foo'") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_not_array() {
        let v = yaml("path: ./lib.mds\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("not array should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("must be a YAML sequence") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_empty_names() {
        let v = yaml("- path: ./lib.mds\n  names: []\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("empty names should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("names cannot be empty") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_absolute_path() {
        let v = yaml("- path: /absolute/path.mds\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("absolute path should fail");
        let msg = err.to_string();
        assert!(msg.contains("in frontmatter"), "got: {msg}");
    }

    #[test]
    fn parse_fm_err_exceeds_limit() {
        // Build a sequence with MAX_FRONTMATTER_IMPORTS + 1 entries
        let entry = "- path: ./lib.mds\n";
        let many = entry.repeat(MAX_FRONTMATTER_IMPORTS + 1);
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&many).expect("valid yaml");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("should exceed limit");
        let msg = err.to_string();
        assert!(msg.contains("exceeds maximum"), "got: {msg}");
    }

    #[test]
    fn parse_fm_prompt_name_in_selective() {
        // "prompt" is a special name — allowed without identifier validation
        let v = yaml("- path: ./lib.mds\n  names: [prompt]\n");
        let result = parse_frontmatter_imports_from_yaml(&v).expect("prompt is allowed");
        assert_eq!(
            result,
            vec![FrontmatterImport::Selective {
                path: "./lib.mds".into(),
                names: vec!["prompt".into()],
            }]
        );
    }

    #[test]
    fn parse_fm_err_duplicate_names() {
        // Duplicate names in the selective names list must be rejected.
        let v = yaml("- path: ./lib.mds\n  names: [greet, greet]\n");
        let err = parse_frontmatter_imports_from_yaml(&v).expect_err("duplicate names should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("duplicate name 'greet'") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn parse_fm_err_non_string_key() {
        // Non-string YAML keys (e.g. integer keys) must be rejected explicitly.
        // Construct a YAML mapping with an integer key via the serde_yaml_ng API
        // since inline YAML always coerces to string keys.
        let mut map = serde_yaml_ng::Mapping::new();
        map.insert(
            serde_yaml_ng::Value::String("path".into()),
            serde_yaml_ng::Value::String("./lib.mds".into()),
        );
        map.insert(
            serde_yaml_ng::Value::Number(42.into()),
            serde_yaml_ng::Value::String("something".into()),
        );
        let seq = serde_yaml_ng::Value::Sequence(vec![serde_yaml_ng::Value::Mapping(map)]);
        let err =
            parse_frontmatter_imports_from_yaml(&seq).expect_err("non-string key should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("keys must be strings") && msg.contains("in frontmatter"),
            "got: {msg}"
        );
    }

    #[test]
    fn has_type_mds_frontmatter_raw_ignores_indented() {
        // Indented `type: mds` inside a nested YAML object must not be detected
        // as the file-type marker (only top-level non-indented keys should match).
        assert!(
            !has_type_mds_frontmatter_raw("config:\n  type: mds\n  key: val\n"),
            "indented type:mds should not trigger detection"
        );
        assert!(
            has_type_mds_frontmatter_raw("type: mds\nconfig:\n  type: other\n"),
            "top-level type:mds should trigger detection"
        );
    }

    #[test]
    fn has_type_mds_frontmatter_ignores_indented() {
        // Same as above but for the full-source variant.
        assert!(
            !has_type_mds_frontmatter("---\nconfig:\n  type: mds\n---\nbody\n"),
            "indented type:mds should not trigger detection in full-source variant"
        );
        assert!(
            has_type_mds_frontmatter("---\ntype: mds\nconfig:\n  type: other\n---\nbody\n"),
            "top-level type:mds should trigger detection in full-source variant"
        );
    }

    // ── Phase 1: @block collision and resource-limit tests ────────────────────

    #[test]
    fn block_duplicate_name_collision() {
        // Two @block declarations with the same name → mds::name_collision.
        let src = "@block foo:\nbody1\n@end\n@block foo:\nbody2\n@end\n";
        let result = crate::compile_str(src);
        assert!(result.is_err(), "duplicate @block name must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("'foo'") || msg.contains("foo"),
            "error should mention the colliding name: {msg}"
        );
    }

    #[test]
    fn block_vs_define_name_collision() {
        // @block and @define sharing the same name → mds::name_collision.
        let src = "@define foo():\ncontent\n@end\n@block foo:\nbody\n@end\n";
        let result = crate::compile_str(src);
        assert!(result.is_err(), "@block vs @define collision must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("'foo'") || msg.contains("foo"),
            "error should mention the colliding name: {msg}"
        );
    }

    #[test]
    fn define_vs_block_name_collision() {
        // @define declared after a @block with the same name → mds::name_collision.
        let src = "@block foo:\nbody\n@end\n@define foo():\ncontent\n@end\n";
        let result = crate::compile_str(src);
        assert!(result.is_err(), "@define vs @block collision must fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("'foo'") || msg.contains("foo"),
            "error should mention the colliding name: {msg}"
        );
    }

    #[test]
    fn block_max_per_module_cap() {
        // Declaring more than MAX_BLOCKS_PER_MODULE @blocks in one module → resource_limit.
        // Build a source with 257 @block declarations (one over the 256 cap).
        let mut src = String::new();
        for i in 0..=256usize {
            src.push_str(&format!("@block blk{i}:\nbody\n@end\n"));
        }
        let result = crate::compile_str(&src);
        assert!(
            result.is_err(),
            "exceeding MAX_BLOCKS_PER_MODULE should fail with resource_limit"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("resource limit") || msg.contains("256") || msg.contains("block"),
            "error should mention resource limit or block count: {msg}"
        );
    }

    #[test]
    fn block_exactly_at_max_allowed() {
        // Exactly MAX_BLOCKS_PER_MODULE (256) @block declarations should compile.
        let mut src = String::new();
        for i in 0..256usize {
            src.push_str(&format!("@block blk{i}:\nbody\n@end\n"));
        }
        let result = crate::compile_str(&src);
        assert!(
            result.is_ok(),
            "exactly 256 @blocks should succeed, got: {result:?}"
        );
    }

    // ── Phase 2: Template inheritance ─────────────────────────────────────────

    /// Helper: create a VirtualFs-backed ModuleCache from a &[(&str, &str)] slice.
    fn virtual_cache(files: &[(&str, &str)]) -> ModuleCache {
        ModuleCache::virtual_fs(
            files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    /// Helper: compile a VirtualFs entry and return the output string.
    fn compile_virtual(files: &[(&str, &str)], entry: &str) -> Result<String, MdsError> {
        let map: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::compile_virtual(map, entry, None)
    }

    /// Helper: check (validate only, no output) a VirtualFs entry.
    fn check_virtual(files: &[(&str, &str)], entry: &str) -> Result<(), MdsError> {
        let map: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::check_virtual(map, entry, None)
    }

    // ── F1: issue worked example (headline test) ──────────────────────────────

    #[test]
    fn f1_worked_example_byte_exact() {
        // base.mds: skeleton with @block placeholders
        // child.mds: overrides instructions+tools, inherits output_format default
        // role=data analysis from child frontmatter
        let base = concat!(
            "You are a {role} assistant.\n",
            "\n",
            "@block instructions:\n",
            "Analyze data carefully.\n",
            "@end\n",
            "@block tools:\n",
            "@end\n",
            "@block output_format:\n",
            "Respond in plain text.\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "role: data analysis\n",
            "---\n",
            "@extends \"./base.mds\"\n",
            "@block instructions:\n",
            "Perform statistical analysis.\n",
            "@end\n",
            "@block tools:\n",
            "You have access to: Python, R\n",
            "@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let result = compile_virtual(&files, "child.mds");
        assert!(result.is_ok(), "F1 compile failed: {:?}", result.err());
        let output = result.unwrap();

        // Must contain base skeleton text with child's frontmatter variable
        assert!(
            output.contains("You are a data analysis assistant."),
            "F1: base skeleton text not rendered: {output}"
        );
        // Must contain overridden blocks from child
        assert!(
            output.contains("Perform statistical analysis."),
            "F1: child instructions block not rendered: {output}"
        );
        assert!(
            output.contains("You have access to: Python, R"),
            "F1: child tools block not rendered: {output}"
        );
        // Must contain base default for un-overridden block
        assert!(
            output.contains("Respond in plain text."),
            "F1: base default output_format block not rendered: {output}"
        );
    }

    // ── F2: standalone base compiles fine rendering its own defaults ──────────

    #[test]
    fn f2_standalone_base_compiles_with_defaults() {
        let base = concat!(
            "---\n",
            "role: general\n",
            "---\n",
            "You are a {role} assistant.\n",
            "@block instructions:\n",
            "Help the user.\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "role: specialist\n",
            "---\n",
            "@extends \"./base.mds\"\n",
            "@block instructions:\n",
            "Provide expert advice.\n",
            "@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];

        // Compile base standalone — must render its own defaults
        let base_out = compile_virtual(&files, "base.mds");
        assert!(
            base_out.is_ok(),
            "F2: standalone base compile failed: {:?}",
            base_out.err()
        );
        let base_str = base_out.unwrap();
        assert!(
            base_str.contains("Help the user."),
            "F2: base default not rendered standalone: {base_str}"
        );
        assert!(
            base_str.contains("You are a general assistant."),
            "F2: base standalone role not rendered: {base_str}"
        );

        // Compile child — must use child overrides and NOT poison base standalone
        let child_out = compile_virtual(&files, "child.mds");
        assert!(
            child_out.is_ok(),
            "F2: child compile failed: {:?}",
            child_out.err()
        );
        let child_str = child_out.unwrap();
        assert!(
            child_str.contains("Provide expert advice."),
            "F2: child override not rendered: {child_str}"
        );
        assert!(
            child_str.contains("You are a specialist assistant."),
            "F2: child role not rendered: {child_str}"
        );
    }

    // ── F2 cache non-poisoning: same base file as skeleton base AND standalone ─

    #[test]
    fn f2_cache_nonpoisoning_base_then_child() {
        // Compile the base FIRST (as standalone), THEN compile child.
        // The cached entry for base must serve the child's skeleton needs.
        let base = "You are a {role} assistant.\n@block instructions:\nDefault.\n@end\n";
        let child = concat!(
            "---\nrole: expert\n---\n",
            "@extends \"./base.mds\"\n",
            "@block instructions:\nExpert advice.\n@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let mut cache = virtual_cache(&files);
        let mut warnings = vec![];

        // Compile base standalone (no role var — will fail on {role} unless runtime vars set)
        // For this test, compile the child first (skeleton base resolution caches base),
        // then assert base standalone also works from same cache.
        let child_result = cache.resolve_key("child.mds", &Default::default(), &mut warnings);
        assert!(
            child_result.is_ok(),
            "cache non-poison: child should compile: {:?}",
            child_result.err()
        );

        // Now compile base standalone — should work independently (cache returns entry).
        // Base has {role} undefined without frontmatter, so it would fail standalone unless
        // cached entry with skeleton (prompt_body=None) is returned. We use a base WITH frontmatter.
        let base_with_fm = "---\nrole: default\n---\nYou are a {role}.\n@block b:\nBody.\n@end\n";
        let child2 = concat!(
            "---\nrole: override\n---\n",
            "@extends \"./base2.mds\"\n",
            "@block b:\nOverride.\n@end\n",
        );
        let files2 = [("base2.mds", base_with_fm), ("child2.mds", child2)];
        let mut cache2 = virtual_cache(&files2);
        let mut w = vec![];

        // Both in same process/cache: resolve base standalone first
        let base_out = cache2.resolve_key("base2.mds", &Default::default(), &mut w);
        assert!(
            base_out.is_ok(),
            "cache2: standalone base should succeed: {:?}",
            base_out.err()
        );

        // Then resolve child (base is already cached)
        let child_out = cache2.resolve_key("child2.mds", &Default::default(), &mut w);
        assert!(
            child_out.is_ok(),
            "cache2: child after cached base should succeed: {:?}",
            child_out.err()
        );
        let child_mod = child_out.unwrap();
        assert!(
            child_mod
                .prompt_body
                .as_deref()
                .unwrap_or("")
                .contains("Override."),
            "cache2: child should use override block"
        );
    }

    #[test]
    fn f2_cache_nonpoisoning_skeleton_then_standalone_reverse_order() {
        // A1 (reverse of f2_cache_nonpoisoning_base_then_child): resolve the CHILD first,
        // which caches the base as a SKELETON (prompt_body=None, never validated/evaluated
        // standalone). A subsequent standalone resolve of that SAME base from the SAME cache
        // must NOT return the empty skeleton entry — it must render the base's own defaults.
        let base = "---\nrole: default\n---\nYou are a {role}.\n@block b:\nBody.\n@end\n";
        let child = concat!(
            "---\nrole: override\n---\n",
            "@extends \"./base.mds\"\n",
            "@block b:\nOverride.\n@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let mut cache = virtual_cache(&files);
        let mut w = vec![];

        // Resolve child FIRST — this caches base as a SKELETON (prompt_body=None).
        let child_out = cache.resolve_key("child.mds", &Default::default(), &mut w);
        assert!(
            child_out.is_ok(),
            "child should compile: {:?}",
            child_out.err()
        );

        // Now resolve base standalone from the SAME cache — must render its own defaults.
        let base_out = cache
            .resolve_key("base.mds", &Default::default(), &mut w)
            .expect("base standalone should compile");
        let body = base_out.prompt_body.as_deref().unwrap_or("<NONE>");
        assert!(
            body.contains("You are a default.") && body.contains("Body."),
            "A1: base standalone after skeleton-cache must render its defaults, got: {body:?}"
        );

        // Arc-sharing must survive the upgrade: the child (resolved earlier from the skeleton)
        // and the upgraded standalone base must still share the same effective_skeleton Arc.
        let child_again = cache
            .resolve_key("child.mds", &Default::default(), &mut w)
            .expect("child re-resolve");
        assert!(
            Arc::ptr_eq(
                &child_again.effective_skeleton,
                &base_out.effective_skeleton
            ),
            "A1: skeleton upgrade must preserve Arc-sharing with descendants"
        );
    }

    // ── Task-1 regression: compute_line_column UTF-8 boundary-safe ──────────
    //
    // Root cause: `compute_line_column` previously panicked with
    // "byte index N is not a char boundary" when a base-template span offset
    // (computed against the base source) was reused against the child source
    // containing multibyte UTF-8 characters.  After the fix the error is
    // returned gracefully (e.g. mds::undefined_var) without a panic.

    #[test]
    fn task1_compile_virtual_no_panic_multibyte_child_source() {
        // Base has an undefined variable — validation will fire a span.
        // The base offset lands at byte 16 in the base source ("@block content:\n"
        // = 16 bytes, then "{undefined_var}").  The child source contains a
        // multibyte character (Japanese ああ = 6 bytes each = 6 bytes for "あ").
        // If the base offset (16) is used against the child source, byte 16 may
        // land mid-codepoint → previously panicked, now returns graceful error.
        let base = "@block content:\n{undefined_var}\n@end\n";
        let child = "@extends \"./ああb.mds\"\n";
        // Note: the filesystem key must match the path literal in the child.
        let files = [("ああb.mds", base), ("child.mds", child)];
        let result = compile_virtual(&files, "child.mds");
        // Must NOT panic — any Err is acceptable (undefined_var or similar).
        assert!(
            result.is_err(),
            "task1: should error (undefined variable), not succeed: {:?}",
            result.ok()
        );
        let err = result.unwrap_err();
        let code = err.serialize().code;
        // The error must be a graceful mds:: error, not a panic.
        assert!(
            code.starts_with("mds::"),
            "task1: expected an mds:: error code, got: {code}"
        );
    }

    #[test]
    fn task1_check_virtual_no_panic_multibyte_child_source() {
        // Same scenario via check_virtual — validates only, no evaluate.
        let base = "@block content:\n{undefined_var}\n@end\n";
        let child = "@extends \"./ああb.mds\"\n";
        let files = [("ああb.mds", base), ("child.mds", child)];
        let result = check_virtual(&files, "child.mds");
        assert!(
            result.is_err(),
            "task1 check_virtual: should error (undefined variable), not succeed"
        );
        let err = result.unwrap_err();
        let code = err.serialize().code;
        assert!(
            code.starts_with("mds::"),
            "task1 check_virtual: expected an mds:: error code, got: {code}"
        );
    }

    // ── F3: multi-level chain A←B←C, most-derived wins ──────────────────────

    #[test]
    fn f3_multilevel_most_derived_wins() {
        let a = concat!(
            "@block content:\n",
            "From A.\n",
            "@end\n",
            "@block footer:\n",
            "Footer A.\n",
            "@end\n",
        );
        let b = concat!(
            "@extends \"./a.mds\"\n",
            "@block content:\n",
            "From B.\n",
            "@end\n",
        );
        let c = concat!(
            "@extends \"./b.mds\"\n",
            "@block content:\n",
            "From C.\n",
            "@end\n",
        );
        let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];

        // C overrides content → "From C." + footer default from A = "Footer A."
        let c_out = compile_virtual(&files, "c.mds").expect("F3: C should compile");
        assert!(
            c_out.contains("From C."),
            "F3: C content should be most-derived: {c_out}"
        );
        assert!(
            c_out.contains("Footer A."),
            "F3: footer should fall through to A default: {c_out}"
        );
        assert!(
            !c_out.contains("From A.") && !c_out.contains("From B."),
            "F3: C should override B which overrode A: {c_out}"
        );

        // B overrides content → "From B." + footer default from A = "Footer A."
        let b_out = compile_virtual(&files, "b.mds").expect("F3: B should compile");
        assert!(
            b_out.contains("From B."),
            "F3: B content should beat A's default: {b_out}"
        );
        assert!(
            b_out.contains("Footer A."),
            "F3: B footer should fall through to A default: {b_out}"
        );

        // A standalone → its own defaults
        let a_out = compile_virtual(&files, "a.mds").expect("F3: A should compile");
        assert!(
            a_out.contains("From A.") && a_out.contains("Footer A."),
            "F3: A standalone should render own defaults: {a_out}"
        );
    }

    // ── F5: diamond inheritance — B and C both extend A; A's cached blocks must not be polluted ─

    #[test]
    fn f5_diamond_inheritance_cache_not_polluted() {
        // A is the base. B and C both extend A.
        // B overrides `shared_block`. C does NOT override `shared_block`.
        // Compiling B then C in one process must not leak B's override into C.
        let a = "@block shared_block:\nFrom A.\n@end\n";
        let b = "@extends \"./a.mds\"\n@block shared_block:\nFrom B.\n@end\n";
        let c = "@extends \"./a.mds\"\n";

        let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
        let mut cache = virtual_cache(&files);
        let mut warnings = vec![];

        // Compile B first
        let b_resolved = cache.resolve_key("b.mds", &Default::default(), &mut warnings);
        assert!(
            b_resolved.is_ok(),
            "F5: B should compile: {:?}",
            b_resolved.err()
        );
        let b_body = b_resolved.unwrap().prompt_body.clone().unwrap_or_default();
        assert!(
            b_body.contains("From B."),
            "F5: B should contain its override: {b_body}"
        );

        // Compile C (uses SAME cache, A already cached)
        let c_resolved = cache.resolve_key("c.mds", &Default::default(), &mut warnings);
        assert!(
            c_resolved.is_ok(),
            "F5: C should compile: {:?}",
            c_resolved.err()
        );
        let c_body = c_resolved.unwrap().prompt_body.clone().unwrap_or_default();
        assert!(
            c_body.contains("From A."),
            "F5: C should use A's default (not B's override): {c_body}"
        );
        assert!(
            !c_body.contains("From B."),
            "F5: C must NOT have B's override (cache poisoning): {c_body}"
        );
    }

    // ── F12: base default block calls a base @define → resolves ───────────────

    #[test]
    fn f12_base_define_resolves_in_child() {
        let base = concat!(
            "@define greet(name):\n",
            "Hello, {name}!\n",
            "@end\n",
            "@block content:\n",
            "{greet(\"World\")}\n",
            "@end\n",
        );
        let child = "@extends \"./base.mds\"\n";
        let files = [("base.mds", base), ("child.mds", child)];

        let result = compile_virtual(&files, "child.mds");
        assert!(
            result.is_ok(),
            "F12: child compile failed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(
            output.contains("Hello, World!"),
            "F12: base @define should resolve in child: {output}"
        );
    }

    // ── E3: stray child content → mds::extends ────────────────────────────────

    #[test]
    fn e3_stray_child_content_error() {
        let base = "@block b:\nDefault.\n@end\n";
        let child = concat!(
            "@extends \"./base.mds\"\n",
            "This is stray text!\n",
            "@block b:\nOverride.\n@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];

        let err = compile_virtual(&files, "child.mds")
            .expect_err("E3: stray text should produce an error");
        let serialized = err.serialize();
        assert_eq!(
            serialized.code, "mds::extends",
            "E3: error code should be mds::extends: {serialized:?}"
        );
        assert!(
            serialized.message.contains("only @block overrides"),
            "E3: message should mention @block overrides: {}",
            serialized.message
        );

        // A5: check_virtual must produce the same error
        let check_err = check_virtual(&files, "child.mds")
            .expect_err("E3 A5: check must also reject stray text");
        assert_eq!(
            check_err.serialize().code,
            "mds::extends",
            "E3 A5: check error code should be mds::extends"
        );
    }

    // ── E4 / F4: unknown override → mds::extends ─────────────────────────────

    #[test]
    fn e4_unknown_override_error() {
        let base = "@block known:\nDefault.\n@end\n";
        let child = concat!(
            "@extends \"./base.mds\"\n",
            "@block known:\nOK.\n@end\n",
            "@block unknown_block:\nBad.\n@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];

        let err = compile_virtual(&files, "child.mds")
            .expect_err("E4: unknown override should produce an error");
        let serialized = err.serialize();
        assert_eq!(
            serialized.code, "mds::extends",
            "E4: error code should be mds::extends: {serialized:?}"
        );
        assert!(
            serialized
                .message
                .contains("only the root template may declare"),
            "E4: message should mention root template: {}",
            serialized.message
        );

        // A5: check_virtual must produce the same error
        let check_err = check_virtual(&files, "child.mds")
            .expect_err("E4 A5: check must also reject unknown override");
        assert_eq!(
            check_err.serialize().code,
            "mds::extends",
            "E4 A5: check error code should be mds::extends"
        );
    }

    // ── F4/E4 intermediate: intermediate template may not declare new @block ────
    //
    // AC: In an A←B←C chain, only the root (A) may declare @block placeholders.
    // B is an intermediate — it extends A but is itself extended by C. If B
    // introduces a brand-new @block name absent from A, both compiling B standalone
    // and compiling the leaf C must reject with mds::extends.

    #[test]
    fn f4_intermediate_new_block_rejected() {
        // A = root base with one declared @block.
        let a = "@block known:\nRoot default.\n@end\n";
        // B = intermediate: extends A, overrides the known block (valid), but also
        // declares a NEW @block name that A never declared (invalid).
        let b = concat!(
            "@extends \"./a.mds\"\n",
            "@block known:\nB override.\n@end\n",
            "@block new_in_b:\nThis must be rejected.\n@end\n",
        );
        // C = leaf extending B (valid chain if B were valid).
        let c = "@extends \"./b.mds\"\n@block known:\nC override.\n@end\n";
        let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];

        // Compiling B directly must fail.
        let err_b = compile_virtual(&files, "b.mds")
            .expect_err("F4-intermediate: new @block in intermediate B must be rejected");
        let serialized_b = err_b.serialize();
        assert_eq!(
            serialized_b.code, "mds::extends",
            "F4-intermediate: B compile error code must be mds::extends: {serialized_b:?}"
        );
        assert!(
            serialized_b
                .message
                .contains("only the root template may declare"),
            "F4-intermediate: B error message must mention root template: {}",
            serialized_b.message
        );
        assert!(
            serialized_b.span.is_some(),
            "F4-intermediate: B error must carry a span"
        );

        // Compiling the leaf C must also fail (B is invalid, so C cannot build on it).
        let err_c = compile_virtual(&files, "c.mds")
            .expect_err("F4-intermediate: leaf C on invalid intermediate B must be rejected");
        let serialized_c = err_c.serialize();
        assert_eq!(
            serialized_c.code, "mds::extends",
            "F4-intermediate: C compile error code must be mds::extends: {serialized_c:?}"
        );
        assert!(
            serialized_c
                .message
                .contains("only the root template may declare"),
            "F4-intermediate: C error message must mention root template: {}",
            serialized_c.message
        );

        // check_virtual on B must produce the same error (A5 parity).
        let check_err_b = check_virtual(&files, "b.mds")
            .expect_err("F4-intermediate: check_virtual on B must also reject");
        assert_eq!(
            check_err_b.serialize().code,
            "mds::extends",
            "F4-intermediate: check_virtual B error code must be mds::extends"
        );
    }

    // ── E5: circular inheritance → mds::circular_import ──────────────────────

    #[test]
    fn e5_circular_inheritance_a_to_b_to_a() {
        // A extends B, B extends A → circular
        let a = "@extends \"./b.mds\"\n@block b:\nA override.\n@end\n";
        let b_content = "@block b:\nB default.\n@end\n";
        // Note: we can only test the cycle detected case; the above won't compile
        // because a.mds extends b.mds and b.mds is a root base (not extending).
        // For a true A→B→A cycle:
        let a2 = "@extends \"./b2.mds\"\n";
        let b2 = "@extends \"./a2.mds\"\n";
        let files2 = [("a2.mds", a2), ("b2.mds", b2)];

        let err = compile_virtual(&files2, "a2.mds")
            .expect_err("E5: circular @extends should produce an error");
        let serialized = err.serialize();
        assert_eq!(
            serialized.code, "mds::circular_import",
            "E5: should surface as mds::circular_import: {serialized:?}"
        );

        // Self-extension: @extends "./self.mds"
        let self_ext = "@extends \"./self.mds\"\n";
        let files_self = [("self.mds", self_ext)];
        let err_self = compile_virtual(&files_self, "self.mds")
            .expect_err("E5: self-extension should produce circular_import");
        let serialized_self = err_self.serialize();
        assert_eq!(
            serialized_self.code, "mds::circular_import",
            "E5: self-extension should surface as mds::circular_import: {serialized_self:?}"
        );

        // Unused variables — just to avoid dead_code warnings in test
        let _ = (a, b_content, files2);
    }

    // ── E5: uses valid circular detection with files that have blocks ─────────

    #[test]
    fn e5_circular_two_hop() {
        // A extends B extends A (proper 2-hop cycle)
        // A has a @block so it's a valid root base syntax-wise
        let a = "@extends \"./b.mds\"\n";
        let b = "@extends \"./a.mds\"\n@block blk:\nB.\n@end\n";
        // This won't work because a.mds has no @block — let's use a root base C that both extend
        // A extends B, B extends A — since neither has @block declarations at root,
        // the cycle is detected before block validation.
        let files = [("a.mds", a), ("b.mds", b)];
        let err = compile_virtual(&files, "a.mds").expect_err("E5: two-hop cycle should error");
        let code = err.serialize().code;
        assert_eq!(
            code, "mds::circular_import",
            "E5: two-hop cycle should be circular_import: {code}"
        );
    }

    // ── E6: 65-deep chain → import-depth error ────────────────────────────────

    #[test]
    fn e6_depth_limit_exceeded() {
        // Build a chain of 66 files: file0 extends file1 extends ... extends file65
        // file65 is the root base with @block declarations.
        let depth = 66usize; // one more than MAX_IMPORT_DEPTH (64)
        let mut files: Vec<(String, String)> = Vec::new();

        // Root base
        let root_src = "@block content:\nRoot.\n@end\n".to_string();
        files.push((format!("file{depth}.mds"), root_src));

        // Each intermediate extends the next
        for i in (0..depth).rev() {
            let src = format!("@extends \"./file{}.mds\"\n", i + 1);
            files.push((format!("file{i}.mds"), src));
        }

        let file_refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let err = compile_virtual(&file_refs, "file0.mds")
            .expect_err("E6: depth > 64 should produce an error");
        let code = err.serialize().code;
        // Should be import error or resource_limit (depth exceeded)
        assert!(
            code == "mds::import"
                || code == "mds::resource_limit"
                || code == "mds::circular_import",
            "E6: depth-exceeded error should be import/resource_limit/circular_import: {code}"
        );
    }

    // ── E10: missing base → file-not-found with span ──────────────────────────

    #[test]
    fn e10_missing_base_file_not_found() {
        let child = "@extends \"./missing.mds\"\n";
        let files = [("child.mds", child)];
        let err = compile_virtual(&files, "child.mds")
            .expect_err("E10: missing base should produce file-not-found");
        let serialized = err.serialize();
        assert_eq!(
            serialized.code, "mds::file_not_found",
            "E10: should be file_not_found: {serialized:?}"
        );
    }

    // ── E11: parse error in base propagates with base's location ─────────────

    #[test]
    fn e11_parse_error_in_base_propagates() {
        // Base has a syntax error: @if without condition
        let base = "@block b:\n@if :\nbad\n@end\n@end\n";
        let child = "@extends \"./base.mds\"\n@block b:\nOK.\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let err = compile_virtual(&files, "child.mds")
            .expect_err("E11: parse error in base should propagate");
        let code = err.serialize().code;
        assert!(
            code == "mds::syntax" || code == "mds::extends",
            "E11: parse error should be syntax or extends: {code}"
        );
    }

    // ── E12: base default block with undefined var → validation error at leaf ──

    #[test]
    fn e12_base_default_undefined_var_caught_at_leaf() {
        // Base has a default block referencing {undefined_var} which is NOT in the
        // base's frontmatter and NOT provided by the child. This should produce an
        // undefined-var error (caught against the merged scope at the leaf).
        let base = "@block content:\n{undefined_var}\n@end\n";
        let child = "@extends \"./base.mds\"\n"; // No frontmatter, no runtime vars

        let files = [("base.mds", base), ("child.mds", child)];
        let err = compile_virtual(&files, "child.mds")
            .expect_err("E12: undefined var in base default should error at leaf");
        let serialized = err.serialize();
        assert!(
            serialized.code == "mds::undefined_var" || serialized.code == "mds::syntax",
            "E12: should be undefined_var (or syntax): {serialized:?}"
        );

        // A5: check_virtual must also reject this
        let check_err = check_virtual(&files, "child.mds")
            .expect_err("E12 A5: check must also reject undefined var in base default");
        assert!(
            check_err.serialize().code == "mds::undefined_var"
                || check_err.serialize().code == "mds::syntax",
            "E12 A5: check should be undefined_var/syntax: {:?}",
            check_err.serialize()
        );
    }

    // ── A2: dependency ordering — base FIRST, before body imports ────────────

    #[test]
    fn a2_dependency_ordering_base_first() {
        let base = "@block b:\nBase.\n@end\n";
        let lib = "@define helper():\nHelper.\n@end\n";
        let child = concat!("@extends \"./base.mds\"\n", "@block b:\n@end\n",);
        // We test via compile_virtual_with_deps which returns the dependency list.
        let files: std::collections::HashMap<String, String> = [
            ("base.mds".to_string(), base.to_string()),
            ("lib.mds".to_string(), lib.to_string()),
            ("child.mds".to_string(), child.to_string()),
        ]
        .into_iter()
        .collect();

        let result = crate::compile_virtual_with_deps(files, "child.mds", None);
        assert!(result.is_ok(), "A2: should compile: {:?}", result.err());
        let output = result.unwrap();
        // base.mds must appear in dependencies (it's a dependency of child.mds)
        assert!(
            output.dependencies.contains(&"base.mds".to_string()),
            "A2: base.mds should be in dependencies: {:?}",
            output.dependencies
        );
        // base.mds must appear BEFORE any body imports (scan_imports puts extends first)
        if let Some(base_idx) = output.dependencies.iter().position(|d| d == "base.mds") {
            // If there are body imports, they must come after base
            // For this test case there are no body imports, but the order is correct.
            assert!(
                base_idx == 0,
                "A2: base.mds should be first dependency: {:?}",
                output.dependencies
            );
        }
    }

    // ── P1: effective_skeleton is Arc<[Node]>, no deep-clone ─────────────────

    #[test]
    fn p1_effective_skeleton_is_arc_shared() {
        // Verify that after resolving a child, both the base and child share the
        // same Arc<[Node]> skeleton (pointer equality).
        let base = "@block b:\nBase.\n@end\n";
        let child = "@extends \"./base.mds\"\n@block b:\nChild.\n@end\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let mut cache = virtual_cache(&files);
        let mut warnings = vec![];

        // Resolve base first (as skeleton via child resolution)
        let child_resolved = cache
            .resolve_key("child.mds", &Default::default(), &mut warnings)
            .expect("P1: child should compile");
        let base_resolved = cache
            .resolve_key("base.mds", &Default::default(), &mut warnings)
            .expect("P1: base should compile");

        // Both should share the same Arc<[Node]> skeleton (Arc::ptr_eq)
        let child_skeleton = &child_resolved.effective_skeleton;
        let base_skeleton = &base_resolved.effective_skeleton;
        assert!(
            Arc::ptr_eq(child_skeleton, base_skeleton),
            "P1: child and base must share the same Arc<[Node]> skeleton (ptr_eq)"
        );
    }

    // ── MdsError::Extends serialize() wired correctly ─────────────────────────

    #[test]
    fn extends_error_serialize_code() {
        let err = MdsError::extends_error_at("test message", "child.mds", "source", 0, 5);
        let serialized = err.serialize();
        assert_eq!(
            serialized.code, "mds::extends",
            "extends error code: {serialized:?}"
        );
        assert!(
            serialized.span.is_some(),
            "extends error should have a span"
        );
    }

    // ── Phase 3: deep_merge_yaml unit tests ───────────────────────────────────

    fn mapping(pairs: &[(&str, serde_yaml_ng::Value)]) -> serde_yaml_ng::Mapping {
        let mut m = serde_yaml_ng::Mapping::new();
        for (k, v) in pairs {
            m.insert(serde_yaml_ng::Value::String(k.to_string()), v.clone());
        }
        m
    }

    fn str_val(s: &str) -> serde_yaml_ng::Value {
        serde_yaml_ng::Value::String(s.to_string())
    }

    fn seq_val(items: &[serde_yaml_ng::Value]) -> serde_yaml_ng::Value {
        serde_yaml_ng::Value::Sequence(items.to_vec())
    }

    fn map_val(pairs: &[(&str, serde_yaml_ng::Value)]) -> serde_yaml_ng::Value {
        serde_yaml_ng::Value::Mapping(mapping(pairs))
    }

    #[test]
    fn deep_merge_yaml_nested_key_by_key() {
        // Both base and child have a nested Mapping at the same key → recursively merged.
        let base = mapping(&[(
            "outer",
            map_val(&[("base_only", str_val("keep")), ("shared", str_val("base"))]),
        )]);
        let child = mapping(&[(
            "outer",
            map_val(&[("shared", str_val("child")), ("child_only", str_val("new"))]),
        )]);
        let result = deep_merge_yaml(&base, &child, 0).expect("deep merge should succeed");
        let outer = match result.get("outer").expect("outer key present") {
            serde_yaml_ng::Value::Mapping(m) => m.clone(),
            other => panic!("expected Mapping, got {other:?}"),
        };
        assert_eq!(
            outer.get("base_only"),
            Some(&str_val("keep")),
            "base-only key survives"
        );
        assert_eq!(
            outer.get("shared"),
            Some(&str_val("child")),
            "child overrides shared key"
        );
        assert_eq!(
            outer.get("child_only"),
            Some(&str_val("new")),
            "child-only key added"
        );
    }

    #[test]
    fn deep_merge_yaml_child_leaf_override() {
        // Scalar in child replaces scalar in base.
        let base = mapping(&[("a", str_val("base")), ("b", str_val("base_b"))]);
        let child = mapping(&[("a", str_val("child"))]);
        let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
        assert_eq!(
            result.get("a"),
            Some(&str_val("child")),
            "child overrides base scalar"
        );
        assert_eq!(
            result.get("b"),
            Some(&str_val("base_b")),
            "base-only key preserved"
        );
    }

    #[test]
    fn deep_merge_yaml_base_only_key_survives() {
        // Key present only in base must appear in the merged output.
        let base = mapping(&[("only_base", str_val("value")), ("shared", str_val("x"))]);
        let child = mapping(&[("shared", str_val("y"))]);
        let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
        assert_eq!(
            result.get("only_base"),
            Some(&str_val("value")),
            "base-only key survives"
        );
        assert_eq!(
            result.get("shared"),
            Some(&str_val("y")),
            "shared key = child wins"
        );
    }

    #[test]
    fn deep_merge_yaml_array_wholesale_replace() {
        // Arrays are replaced wholesale — no element-level merge.
        let base = mapping(&[("tags", seq_val(&[str_val("a"), str_val("b")]))]);
        let child = mapping(&[("tags", seq_val(&[str_val("c")]))]);
        let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
        assert_eq!(
            result.get("tags"),
            Some(&seq_val(&[str_val("c")])),
            "child array replaces base array wholesale"
        );
    }

    #[test]
    fn deep_merge_yaml_reserved_keys_excluded() {
        // imports, type, extends must be excluded from the merged output.
        let base = mapping(&[
            ("imports", seq_val(&[])),
            ("type", str_val("mds")),
            ("extends", str_val("./parent.mds")),
            ("real_key", str_val("keep")),
        ]);
        let child = mapping(&[
            ("imports", seq_val(&[])),
            ("type", str_val("mds")),
            ("extends", str_val("./other.mds")),
            ("child_key", str_val("added")),
        ]);
        let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
        assert!(result.get("imports").is_none(), "imports must be excluded");
        assert!(result.get("type").is_none(), "type must be excluded");
        assert!(result.get("extends").is_none(), "extends must be excluded");
        assert_eq!(result.get("real_key"), Some(&str_val("keep")));
        assert_eq!(result.get("child_key"), Some(&str_val("added")));
    }

    #[test]
    fn deep_merge_yaml_key_order_base_then_child() {
        // Base keys come first, then child-only keys, preserving base key order (A6).
        let base = mapping(&[("a", str_val("1")), ("b", str_val("2"))]);
        let child = mapping(&[("c", str_val("3")), ("a", str_val("a_child"))]);
        let result = deep_merge_yaml(&base, &child, 0).expect("merge ok");
        let keys: Vec<&str> = result
            .iter()
            .filter_map(|(k, _)| {
                if let serde_yaml_ng::Value::String(s) = k {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .collect();
        // Expected order: a (from base), b (from base), c (child-only appended)
        assert_eq!(keys, ["a", "b", "c"], "key order: base-then-child (A6)");
        assert_eq!(
            result.get("a"),
            Some(&str_val("a_child")),
            "shared key = child wins"
        );
    }

    #[test]
    fn deep_merge_yaml_depth_cap_succeeds_at_cap() {
        // Build a nested mapping exactly MAX_FRONTMATTER_MERGE_DEPTH deep.
        // The call at depth=0 with nesting of cap levels should succeed (cap is the limit check,
        // we need cap+1 to exceed it).
        let cap = MAX_FRONTMATTER_MERGE_DEPTH;
        // Build a base mapping nested cap levels deep, then call with depth=0.
        // The deepest merge call will be at depth=cap (cap nested recursive calls), which
        // should still succeed because the check is `depth > cap`.
        fn nested_map(depth: usize, cap: usize) -> serde_yaml_ng::Mapping {
            let mut m = serde_yaml_ng::Mapping::new();
            if depth < cap {
                m.insert(
                    serde_yaml_ng::Value::String("n".to_string()),
                    serde_yaml_ng::Value::Mapping(nested_map(depth + 1, cap)),
                );
            } else {
                m.insert(
                    serde_yaml_ng::Value::String("leaf".to_string()),
                    serde_yaml_ng::Value::String("base".to_string()),
                );
            }
            m
        }
        let deep_base = nested_map(0, cap);
        // Child has same structure with a different leaf
        let mut deep_child_inner = serde_yaml_ng::Mapping::new();
        deep_child_inner.insert(
            serde_yaml_ng::Value::String("leaf".to_string()),
            serde_yaml_ng::Value::String("child".to_string()),
        );
        // We don't need child to be as deep — the deepest base map merged with
        // an empty child at depth=cap still succeeds.
        let result = deep_merge_yaml(&deep_base, &serde_yaml_ng::Mapping::new(), 0);
        assert!(result.is_ok(), "depth=cap should succeed: {result:?}");
    }

    #[test]
    fn deep_merge_yaml_depth_cap_plus_one_errors() {
        // Calling deep_merge_yaml with depth = MAX_FRONTMATTER_MERGE_DEPTH + 1 must
        // return mds::resource_limit (P4, no stack overflow).
        let base = serde_yaml_ng::Mapping::new();
        let child = serde_yaml_ng::Mapping::new();
        let result = deep_merge_yaml(&base, &child, MAX_FRONTMATTER_MERGE_DEPTH + 1);
        assert!(result.is_err(), "depth cap+1 must error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("resource limit") || msg.contains("depth"),
            "error should mention resource limit or depth: {msg}"
        );
    }

    // ── Phase 3: integration tests via compile_virtual ────────────────────────

    #[test]
    fn f6_deep_frontmatter_merge_nested_object() {
        // F6: nested key merged key-by-key; child leaf overrides; base-only key visible.
        let base = concat!(
            "---\n",
            "config:\n",
            "  model: gpt-4\n",
            "  temperature: 0.7\n",
            "base_only: \"from base\"\n",
            "---\n",
            "@block content:\n",
            "model={config.model} temp={config.temperature} base={base_only}\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "config:\n",
            "  temperature: 0.3\n",
            "  extra: added\n",
            "---\n",
            "@extends \"./base.mds\"\n",
            "@block content:\n",
            "model={config.model} temp={config.temperature} extra={config.extra} base={base_only}\n",
            "@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let result = compile_virtual(&files, "child.mds").expect("F6: deep merge should compile");
        assert!(
            result.contains("model=gpt-4"),
            "base-only nested key visible: {result}"
        );
        assert!(
            result.contains("temp=0.3"),
            "child override applied: {result}"
        );
        assert!(
            result.contains("extra=added"),
            "child-only nested key visible: {result}"
        );
        assert!(
            result.contains("base=from base"),
            "base top-level key visible: {result}"
        );
    }

    #[test]
    fn f6_deep_frontmatter_merge_array_wholesale_replace() {
        // F6/decision #7: arrays in frontmatter are replaced wholesale, not merged.
        let base = concat!(
            "---\n",
            "tools:\n",
            "  - python\n",
            "  - rust\n",
            "---\n",
            "@block content:\n",
            "@for tool in tools:\n",
            "{tool}\n",
            "@end\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "tools:\n",
            "  - typescript\n",
            "---\n",
            "@extends \"./base.mds\"\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let result =
            compile_virtual(&files, "child.mds").expect("F6: array replace should compile");
        assert!(
            result.contains("typescript"),
            "child array replaces base: {result}"
        );
        assert!(
            !result.contains("python"),
            "base array not in child result: {result}"
        );
        assert!(
            !result.contains("rust"),
            "base array not in child result: {result}"
        );
    }

    #[test]
    fn f6_base_only_key_visible_in_child() {
        // F6: key present only in base FM is visible to child scope.
        let base = concat!(
            "---\n",
            "only_in_base: \"secret_from_base\"\n",
            "---\n",
            "@block content:\n",
            "{only_in_base}\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "child_var: hello\n",
            "---\n",
            "@extends \"./base.mds\"\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let result = compile_virtual(&files, "child.mds").expect("F6: base-only key visible");
        assert!(
            result.contains("secret_from_base"),
            "base-only key visible in child: {result}"
        );
    }

    #[test]
    fn f7_runtime_override_precedence() {
        // F7: runtime --set overrides merged frontmatter (base < child < runtime).
        // We test at the ResolvedModule level to check the rendered body directly,
        // without the raw_frontmatter fence (which always shows the child's raw FM).
        let base = concat!(
            "---\n",
            "role: base_role\n",
            "---\n",
            "@block content:\n",
            "{role}\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "role: child_role\n",
            "---\n",
            "@extends \"./base.mds\"\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];

        // Without runtime override: child wins over base.
        {
            let mut cache = virtual_cache(&files);
            let resolved = cache
                .resolve_key("child.mds", &Default::default(), &mut vec![])
                .expect("F7: should compile without runtime override");
            let body = resolved.prompt_body.as_deref().unwrap_or("");
            assert!(
                body.contains("child_role"),
                "child overrides base without runtime: body={body}"
            );
            assert!(
                !body.contains("base_role"),
                "base value not present when child overrides: body={body}"
            );
        }

        // With runtime override: runtime wins over child.
        {
            let mut runtime_vars = HashMap::new();
            runtime_vars.insert(
                "role".to_string(),
                Value::String("runtime_role".to_string()),
            );
            let mut cache = virtual_cache(&files);
            let resolved = cache
                .resolve_key("child.mds", &runtime_vars, &mut vec![])
                .expect("F7: should compile with runtime override");
            let body = resolved.prompt_body.as_deref().unwrap_or("");
            assert!(
                body.contains("runtime_role"),
                "runtime overrides child: body={body}"
            );
            assert!(
                !body.contains("child_role"),
                "child value not present when runtime overrides: body={body}"
            );
            assert!(
                !body.contains("base_role"),
                "base value not present when runtime overrides: body={body}"
            );
        }
    }

    #[test]
    fn f8_base_default_block_use_base_fm_alias() {
        // F8: a base default block can use a function from a base frontmatter import alias.
        // Base has `imports: [{path: ./shared.mds, as: shared}]` in its FM.
        // Base default block uses {shared.greeting("World")} interpolation.
        let shared = "@define greeting(name):\nHello {name}!\n@end\n";
        let base = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./shared.mds\n",
            "    as: shared\n",
            "---\n",
            "@block content:\n",
            "{shared.greeting(\"World\")}\n",
            "@end\n",
        );
        let child = "@extends \"./base.mds\"\n";
        let files = [
            ("shared.mds", shared),
            ("base.mds", base),
            ("child.mds", child),
        ];
        let result = compile_virtual(&files, "child.mds")
            .expect("F8: base FM import alias in base default block");
        assert!(
            result.contains("Hello World!"),
            "base FM alias usable in base default block: {result}"
        );
    }

    #[test]
    fn f8_child_can_use_own_fm_import_alias() {
        // F8: child's own frontmatter import alias is available in its block overrides.
        let lib = "@define greet(x):\nHi {x}\n@end\n";
        let base = "@block msg:\nDefault message\n@end\n";
        let child = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "@extends \"./base.mds\"\n",
            "@block msg:\n",
            "{lib.greet(\"child\")}\n",
            "@end\n",
        );
        let files = [("lib.mds", lib), ("base.mds", base), ("child.mds", child)];
        let result =
            compile_virtual(&files, "child.mds").expect("F8: child FM import alias in child block");
        assert!(
            result.contains("Hi child"),
            "child FM alias usable in child block override: {result}"
        );
    }

    #[test]
    fn f8_duplicate_alias_base_and_child_error() {
        // F8/ADR-014: same alias in both base and child frontmatter imports → mds::name_collision.
        let lib = "@define foo():\nfoo\n@end\n";
        let base = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: mylib\n",
            "---\n",
            "@block content:\n",
            "base content\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: mylib\n",
            "---\n",
            "@extends \"./base.mds\"\n",
        );
        let files = [("lib.mds", lib), ("base.mds", base), ("child.mds", child)];
        let result = compile_virtual(&files, "child.mds");
        assert!(result.is_err(), "duplicate alias base+child must error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mylib") || msg.contains("name") || msg.contains("collision"),
            "error should mention the colliding alias: {msg}"
        );
    }

    #[test]
    fn a6_determinism_double_compile_byte_identical() {
        // A6: compiling the same multi-level chain twice produces byte-identical output.
        let a = concat!(
            "---\n",
            "x: 1\n",
            "y: 2\n",
            "---\n",
            "@block content:\n",
            "{x},{y}\n",
            "@end\n",
        );
        let b = concat!(
            "---\n",
            "y: 99\n",
            "z: 3\n",
            "---\n",
            "@extends \"./a.mds\"\n",
        );
        let c = concat!("---\n", "z: 100\n", "---\n", "@extends \"./b.mds\"\n",);
        let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
        let result1 = compile_virtual(&files, "c.mds").expect("A6 first compile");
        let result2 = compile_virtual(&files, "c.mds").expect("A6 second compile");
        assert_eq!(
            result1, result2,
            "A6: double compile must be byte-identical"
        );
    }

    #[test]
    fn a6_for_loop_over_deep_merged_fm_stable_order() {
        // A6: @for over a deep-merged object iterates in stable base-then-child key order.
        // The base has keys a, b in its labels object; child adds key c.
        // deep_merge produces labels with keys a, b, c in that order (base-then-child).
        // MDS uses @for k, v in obj: for objects.
        let base = concat!(
            "---\n",
            "labels:\n",
            "  a: \"first\"\n",
            "  b: \"second\"\n",
            "---\n",
            "@block content:\n",
            "@for k, v in labels:\n",
            "{k}={v};\n",
            "@end\n",
            "@end\n",
        );
        // child extends base and specifies all three keys in labels (a+b from base merged
        // with c from child → a, b first from base position, c appended by child order).
        let child = concat!(
            "---\n",
            "labels:\n",
            "  a: \"first\"\n",
            "  b: \"second\"\n",
            "  c: \"third\"\n",
            "---\n",
            "@extends \"./base.mds\"\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let result = compile_virtual(&files, "child.mds").expect("A6 stable key order");
        // Verify all three keys present
        assert!(result.contains("a=first"), "key a present: {result}");
        assert!(result.contains("b=second"), "key b present: {result}");
        assert!(result.contains("c=third"), "key c present: {result}");
        // Verify order: a before b before c
        let pos_a = result.find("a=first").expect("a in result");
        let pos_b = result.find("b=second").expect("b in result");
        let pos_c = result.find("c=third").expect("c in result");
        assert!(
            pos_a < pos_b,
            "a before b (stable base-then-child order): {result}"
        );
        assert!(
            pos_b < pos_c,
            "b before c (stable base-then-child order): {result}"
        );
    }

    #[test]
    fn p4_fm_merge_depth_bound_resource_limit() {
        // P4: deep_merge_yaml at depth > MAX_FRONTMATTER_MERGE_DEPTH returns mds::resource_limit.
        let result = deep_merge_yaml(
            &serde_yaml_ng::Mapping::new(),
            &serde_yaml_ng::Mapping::new(),
            MAX_FRONTMATTER_MERGE_DEPTH + 1,
        );
        assert!(result.is_err(), "P4: depth cap+1 must error");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("resource limit") || msg.contains("depth"),
            "P4: error should be resource_limit: {msg}"
        );
    }

    #[test]
    fn regression_non_extending_file_fm_unchanged() {
        // Regression: a non-extending file with frontmatter imports still works identically.
        // This confirms the standalone build_scope_from_frontmatter path is unchanged.
        let lib = "@define greet(name):\nHello {name}!\n@end\n";
        let standalone = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "greeting: World\n",
            "---\n",
            "{lib.greet(greeting)}\n",
        );
        let files = [("lib.mds", lib), ("standalone.mds", standalone)];
        let result = compile_virtual(&files, "standalone.mds")
            .expect("regression: standalone FM imports should still work");
        assert!(
            result.contains("Hello World!"),
            "standalone FM import regression: {result}"
        );
    }

    #[test]
    fn f4_child_emits_only_own_raw_frontmatter() {
        // decision #7 / output emission: extending child emits only its own raw_frontmatter.
        // Base frontmatter is an input to scope, not output.
        let base = concat!(
            "---\n",
            "base_secret: only_in_base\n",
            "---\n",
            "@block content:\n",
            "{base_secret}\n",
            "@end\n",
        );
        let child = concat!(
            "---\n",
            "child_var: in_child\n",
            "---\n",
            "@extends \"./base.mds\"\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let mut cache = virtual_cache(&files);
        let mut warnings = vec![];

        let child_resolved = cache
            .resolve_key("child.mds", &Default::default(), &mut warnings)
            .expect("output emission test should compile");

        // raw_frontmatter in the resolved module is the child's raw FM (not base's).
        if let Some(ref raw_fm) = child_resolved.raw_frontmatter {
            assert!(
                !raw_fm.contains("base_secret"),
                "child output must NOT contain base frontmatter: {raw_fm}"
            );
            assert!(
                raw_fm.contains("child_var"),
                "child output must contain child's own frontmatter: {raw_fm}"
            );
        }
        // The compiled output uses the merged scope (base_secret visible to blocks)
        let output = child_resolved.prompt_body.as_deref().unwrap_or("");
        assert!(
            output.contains("only_in_base"),
            "merged scope used: base var rendered in block: {output}"
        );
    }

    #[test]
    fn f3_multilevel_deep_merge_transitive() {
        // A←B←C: deep merge is transitive: A's FM < B's FM < C's FM.
        // A has a=1, b=2. B overrides b=99, adds c=3. C overrides c=100.
        // Result: a=1 (from A), b=99 (from B), c=100 (from C).
        let a = concat!(
            "---\n",
            "a: \"from_a\"\n",
            "b: \"from_a_b\"\n",
            "---\n",
            "@block content:\n",
            "a={a} b={b} c={c}\n",
            "@end\n",
        );
        let b = concat!(
            "---\n",
            "b: \"from_b\"\n",
            "c: \"from_b_c\"\n",
            "---\n",
            "@extends \"./a.mds\"\n",
        );
        let c = concat!(
            "---\n",
            "c: \"from_c\"\n",
            "---\n",
            "@extends \"./b.mds\"\n",
        );
        let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
        let result = compile_virtual(&files, "c.mds").expect("F3 multilevel deep merge");
        assert!(
            result.contains("a=from_a"),
            "a=from_a (root only): {result}"
        );
        assert!(
            result.contains("b=from_b"),
            "b=from_b (B overrides A): {result}"
        );
        assert!(
            result.contains("c=from_c"),
            "c=from_c (C overrides B): {result}"
        );
    }

    // ── Phase 4: messages-mode inheritance ───────────────────────────────────

    /// Helper: compile a VirtualFs entry in messages mode.
    fn compile_messages_virtual_helper(
        files: &[(&str, &str)],
        entry: &str,
    ) -> Result<Vec<crate::Message>, MdsError> {
        let map: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::compile_messages_virtual(map, entry, None).map(|output| output.messages)
    }

    // ── F9: messages mode — @message-structured base + child @block override ──
    //
    // Layout: @block is at top-level (base skeleton), @message is inside the @block body.
    // @block cannot appear inside @message (parser enforces top-level only).

    #[test]
    fn f9_messages_mode_block_override_compiles_to_message_array() {
        // Base: @block at top level, @message inside the block body (default).
        // Child: overrides the block — the @message in the override surfaces in output.
        let base = concat!(
            "@block msg:\n",
            "@message user:\n",
            "Default question.\n",
            "@end\n",
            "@end\n",
        );
        let child = concat!(
            "@extends \"./base.mds\"\n",
            "@block msg:\n",
            "@message user:\n",
            "Child override question.\n",
            "@end\n",
            "@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let messages = compile_messages_virtual_helper(&files, "child.mds")
            .expect("F9: messages-mode inheritance should compile");
        assert_eq!(
            messages.len(),
            1,
            "F9: expected 1 message, got {messages:?}"
        );
        assert_eq!(messages[0].role, "user", "F9: expected role=user");
        assert!(
            messages[0].content.contains("Child override question."),
            "F9: child block override should appear in message content: {:?}",
            messages[0].content,
        );
    }

    #[test]
    fn f9_messages_mode_default_block_in_message_body() {
        // @message inside a base default block (un-overridden by child) surfaces in output.
        // @block is top-level; @message is inside the @block body.
        let base = concat!(
            "@block intro:\n",
            "@message system:\n",
            "You are a helpful assistant.\n",
            "@end\n",
            "@end\n",
        );
        let child = "@extends \"./base.mds\"\n";
        let files = [("base.mds", base), ("child.mds", child)];
        let messages = compile_messages_virtual_helper(&files, "child.mds")
            .expect("F9: @message inside un-overridden base default block should surface");
        assert_eq!(
            messages.len(),
            1,
            "F9: expected 1 message, got {messages:?}"
        );
        assert_eq!(messages[0].role, "system");
        assert!(
            messages[0].content.contains("You are a helpful assistant."),
            "F9: message from base default block: {:?}",
            messages[0].content,
        );
    }

    // ── E13: messages mode — base with no @message → clear error ─────────────

    #[test]
    fn e13_messages_mode_base_no_message_block_clear_error() {
        // Base has @block placeholders but no @message — compiling child in
        // messages mode should return the existing "no @message block" guard error.
        let base = concat!(
            "You are an assistant.\n",
            "@block instructions:\n",
            "Do things carefully.\n",
            "@end\n",
        );
        let child = concat!(
            "@extends \"./base.mds\"\n",
            "@block instructions:\n",
            "Do things quickly.\n",
            "@end\n",
        );
        let files = [("base.mds", base), ("child.mds", child)];
        let err = compile_messages_virtual_helper(&files, "child.mds")
            .expect_err("E13: no @message in final_body → should error");
        let msg = err.to_string();
        assert!(
            msg.contains("@message") || msg.contains("message"),
            "E13: error should mention @message: {msg}"
        );
        // Must be mds::syntax (existing guard) not an internal panic.
        let code = err.serialize().code;
        assert_eq!(
            code, "mds::syntax",
            "E13: error code should be mds::syntax: {code}"
        );
    }

    // ── F10 (messages half): empty block renders empty in messages mode ────────

    #[test]
    fn f10_messages_mode_empty_block_renders_empty() {
        // An @block with no default and no child override renders empty — surrounding
        // @message content intact. @block is at top level; @message is a sibling,
        // not a parent (parser rejects @block inside @message).
        let base = concat!(
            // A @message block with literal surrounding text — no @block inside message.
            "@message user:\n",
            "Before.\n",
            "@end\n",
            // The @block placeholder at top level: empty default body.
            "@block gap:\n",
            "@end\n",
            // Another @message for content after the gap placeholder.
            "@message user:\n",
            "After.\n",
            "@end\n",
        );
        // Child overrides the gap block with an empty body (same as default).
        let child = concat!("@extends \"./base.mds\"\n", "@block gap:\n", "@end\n",);
        let files = [("base.mds", base), ("child.mds", child)];
        let messages = compile_messages_virtual_helper(&files, "child.mds")
            .expect("F10 messages: empty block should not break compilation");
        // Two @message blocks: Before. and After.
        assert_eq!(
            messages.len(),
            2,
            "F10: expected 2 messages, got {messages:?}"
        );
        let first_content = &messages[0].content;
        let second_content = &messages[1].content;
        assert!(
            first_content.contains("Before."),
            "F10: first message must contain 'Before.': {first_content}"
        );
        assert!(
            second_content.contains("After."),
            "F10: second message must contain 'After.': {second_content}"
        );
    }

    // ── P5: deep-chain performance guard (TEXT + MESSAGES, < 2 s) ────────────

    #[test]
    fn p5_deep_chain_32_levels_text_and_messages_under_2s() {
        // Build a 32-level chain: file0 @extends file1 @extends ... @extends file31
        // file31 is the root with @block + @message content.
        // Both text and messages compilation must complete in < 2 s with no OOM.
        let depth = 32usize;
        let mut files: Vec<(String, String)> = Vec::new();

        // Root base: lightweight skeleton with @block containing @message — @block at
        // top-level, @message inside it. Both text and messages modes work with this.
        let root_src = concat!(
            "@block content:\n",
            "@message user:\n",
            "Hello from root.\n",
            "@end\n",
            "@end\n",
        )
        .to_string();
        files.push((format!("file{depth}.mds"), root_src));

        // Each level just extends the next — no frontmatter, no overrides.
        for i in (0..depth).rev() {
            let src = format!("@extends \"./file{}.mds\"\n", i + 1);
            files.push((format!("file{i}.mds"), src));
        }

        let file_refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let start = std::time::Instant::now();
        let text_result = compile_virtual(&file_refs, "file0.mds");
        let messages_result = compile_messages_virtual_helper(&file_refs, "file0.mds");
        let elapsed = start.elapsed();

        assert!(
            text_result.is_ok(),
            "P5: 32-level chain text compile failed: {:?}",
            text_result.err()
        );
        assert!(
            messages_result.is_ok(),
            "P5: 32-level chain messages compile failed: {:?}",
            messages_result.err()
        );
        assert!(
            elapsed.as_secs() < 2,
            "P5: 32-level chain took {:?}, must be < 2 s",
            elapsed
        );
    }

    // ── P6: PF-004 oversized base rejected in BOTH modes ─────────────────────

    #[test]
    fn p6_pf004_oversized_base_rejected_in_text_mode() {
        // PF-004 (applying avoids PF-004): a base larger than MAX_FILE_SIZE is rejected
        // via resolve_by_key_skeleton (FileSystem trait path, never std::fs).
        // Text mode must return mds::resource_limit.
        use crate::limits::MAX_FILE_SIZE;
        // One byte over the limit — large enough to trigger the guard.
        let oversized = "x".repeat((MAX_FILE_SIZE + 1) as usize);
        let child = "@extends \"./base.mds\"\n";
        let files = [("base.mds", oversized.as_str()), ("child.mds", child)];
        let err = compile_virtual(&files, "child.mds")
            .expect_err("P6 text: oversized base must be rejected");
        let code = err.serialize().code;
        assert_eq!(
            code, "mds::resource_limit",
            "P6 text: error code must be mds::resource_limit: {code}"
        );
        // PF-004 + debug-panics gotcha: no base filesystem path in error message.
        let msg = err.to_string();
        assert!(
            !msg.contains("/Users/") && !msg.contains("\\Users\\"),
            "P6 text: error must not leak absolute filesystem path: {msg}"
        );
    }

    #[test]
    fn p6_pf004_oversized_base_rejected_in_messages_mode() {
        // PF-004 (applying avoids PF-004): same oversized-base guard must also fire
        // in messages mode — both modes go through resolve_by_key_skeleton.
        use crate::limits::MAX_FILE_SIZE;
        let oversized = "x".repeat((MAX_FILE_SIZE + 1) as usize);
        let child = "@extends \"./base.mds\"\n";
        let files = [("base.mds", oversized.as_str()), ("child.mds", child)];
        let err = compile_messages_virtual_helper(&files, "child.mds")
            .expect_err("P6 messages: oversized base must be rejected");
        let code = err.serialize().code;
        assert_eq!(
            code, "mds::resource_limit",
            "P6 messages: error code must be mds::resource_limit: {code}"
        );
        // PF-004 + debug-panics gotcha: no base filesystem path in error message.
        let msg = err.to_string();
        assert!(
            !msg.contains("/Users/") && !msg.contains("\\Users\\"),
            "P6 messages: error must not leak absolute filesystem path: {msg}"
        );
    }

    // ── F9 multi-level messages (two-hop chain) ───────────────────────────────

    #[test]
    fn f9_messages_mode_multilevel_chain() {
        // A←B←C: C extends B extends A. A has @block (top-level) with @message inside.
        // B overrides the block. C overrides again. Most-derived (C) wins.
        let a = concat!(
            "@block msg:\n",
            "@message user:\n",
            "From A.\n",
            "@end\n",
            "@end\n",
        );
        let b = concat!(
            "@extends \"./a.mds\"\n",
            "@block msg:\n",
            "@message user:\n",
            "From B.\n",
            "@end\n",
            "@end\n",
        );
        let c = concat!(
            "@extends \"./b.mds\"\n",
            "@block msg:\n",
            "@message user:\n",
            "From C.\n",
            "@end\n",
            "@end\n",
        );
        let files = [("a.mds", a), ("b.mds", b), ("c.mds", c)];
        let messages = compile_messages_virtual_helper(&files, "c.mds")
            .expect("F9 multilevel: should compile");
        assert_eq!(messages.len(), 1, "F9 multilevel: got {messages:?}");
        assert!(
            messages[0].content.contains("From C."),
            "F9 multilevel: most-derived (C) wins: {:?}",
            messages[0].content
        );
    }

    // ── F11: whitespace contract — 4-combination byte-exact matrix ───────────────
    //
    // Decision #9 (from spec): block-body edge newlines (ONE leading + ONE trailing
    // `\n`) are stripped at parse time. Between-block Text nodes in the skeleton
    // are preserved verbatim. This test pins all four observable combinations:
    //
    //  1. Base default (no override):    skeleton text nodes pass through; block
    //                                    body edge newlines stripped.
    //  2. Child override, no blanks:     body = "Override." — only text, no leading/
    //                                    trailing blank lines.
    //  3. Child override WITH blanks:    extra blank lines inside the block body
    //                                    survive (only ONE edge \n is stripped each
    //                                    side), producing one extra \n in output.
    //  4. Child override, indented:      leading spaces inside block body are
    //                                    preserved verbatim; edge \n still stripped.

    #[test]
    fn f11_whitespace_contract_4_combination_matrix() {
        // Base skeleton: text before, one @block, text after.
        // Between the Text("Intro.\n\n") node and the @block body there is NO
        // extra whitespace beyond what the skeleton text nodes carry.
        //
        // Base source (repr): "Intro.\n\n@block body:\nDefault body.\n@end\n\nAfter.\n"
        //
        // Skeleton nodes after parse:
        //   Text("Intro.\n\n")
        //   Block("body")  body = [Text("Default body.")]   ← edge \n stripped
        //   Text("\nAfter.\n")                              ← the blank line + After.
        let base = "Intro.\n\n@block body:\nDefault body.\n@end\n\nAfter.\n";

        // ── Combination 1: base default, no child override ────────────────────
        // Child has no @block override — effective_blocks use the base default.
        // Between-block blank line (\n before "After.") preserved verbatim.
        {
            let child = "@extends \"./base.mds\"\n";
            let files = [("base.mds", base), ("child.mds", child)];
            let out = compile_virtual(&files, "child.mds").expect("F11 combo-1: should compile");
            assert_eq!(
                out,
                "Intro.\n\nDefault body.\nAfter.\n",
                "F11 combo-1: base default — between-block blank line preserved, body edge stripped"
            );
        }

        // ── Combination 2: override with no surrounding blank lines ───────────
        // Block body = "Override." (no leading/trailing blank lines).
        // After edge-strip: body = [Text("Override.")].
        {
            let child = "@extends \"./base.mds\"\n@block body:\nOverride.\n@end\n";
            let files = [("base.mds", base), ("child.mds", child)];
            let out = compile_virtual(&files, "child.mds").expect("F11 combo-2: should compile");
            assert_eq!(
                out, "Intro.\n\nOverride.\nAfter.\n",
                "F11 combo-2: override without blank lines — clean output"
            );
        }

        // ── Combination 3: override WITH leading+trailing blank lines ─────────
        // Block body raw = "\nOverride.\n\n" (blank line before + blank line after).
        // strip_leading_newline removes ONE leading \n  → "Override.\n\n"
        // strip_trailing_newline removes ONE trailing \n → "Override.\n"
        // Residual \n becomes part of the rendered block body, producing an extra
        // blank line BEFORE the "After." skeleton text node ("\nAfter.\n").
        // This pins decision #9: only one edge \n is stripped — extra interior
        // blank lines are preserved.
        {
            let child = "@extends \"./base.mds\"\n@block body:\n\nOverride.\n\n@end\n";
            let files = [("base.mds", base), ("child.mds", child)];
            let out = compile_virtual(&files, "child.mds").expect("F11 combo-3: should compile");
            assert_eq!(
                out,
                "Intro.\n\nOverride.\n\nAfter.\n",
                "F11 combo-3: override with surrounding blanks — extra blank line inside body preserved (only edge \n stripped)"
            );
        }

        // ── Combination 4: override with indented content ─────────────────────
        // Block body raw = "  Indented.\n".
        // strip_leading_newline: no leading \n, no change.
        // strip_trailing_newline: pop \n → "  Indented."
        // Leading spaces are preserved verbatim (base author's indentation style).
        {
            let child = "@extends \"./base.mds\"\n@block body:\n  Indented.\n@end\n";
            let files = [("base.mds", base), ("child.mds", child)];
            let out = compile_virtual(&files, "child.mds").expect("F11 combo-4: should compile");
            assert_eq!(
                out, "Intro.\n\n  Indented.\nAfter.\n",
                "F11 combo-4: indented override — leading spaces preserved verbatim"
            );
        }
    }

    // ── A3: Error-code mapping consolidation (resolver layer) ─────────────────
    //
    // Authoritative table for resolver-level errors:
    //
    // | ID | Trigger                               | Expected code          |
    // |----|---------------------------------------|------------------------|
    // | E3 | stray child content                   | mds::extends           |
    // | E4 | unknown override block                | mds::extends           |
    // | E5 | circular inheritance (A→B→A, self)    | mds::circular_import   |
    // | E7 | @block name collides with @define     | mds::name_collision    |
    // | E8 | duplicate @block in same module       | mds::name_collision    |
    //
    // E1/E2/E9 are covered in parser_tests.rs (a3_parser_error_code_table).

    #[test]
    fn a3_resolver_error_code_table() {
        // E3: stray child content → mds::extends
        {
            let base = "@block body:\nHello\n@end\n";
            let child = "@extends \"./base.mds\"\nStray text here.\n";
            let files = [("base.mds", base), ("child.mds", child)];
            let err = compile_virtual(&files, "child.mds")
                .expect_err("A3 E3: stray child content should error");
            assert_eq!(
                err.serialize().code,
                "mds::extends",
                "A3 E3: stray child content must be mds::extends, got: {:?}",
                err.serialize()
            );
        }

        // E4: unknown block override → mds::extends
        {
            let base = "@block body:\nHello\n@end\n";
            let child = "@extends \"./base.mds\"\n@block nonexistent:\nOverride\n@end\n";
            let files = [("base.mds", base), ("child.mds", child)];
            let err = compile_virtual(&files, "child.mds")
                .expect_err("A3 E4: unknown override should error");
            assert_eq!(
                err.serialize().code,
                "mds::extends",
                "A3 E4: unknown block override must be mds::extends, got: {:?}",
                err.serialize()
            );
        }

        // E5: circular inheritance (A→B→A) → mds::circular_import
        {
            let a = "@extends \"./b.mds\"\n@block body:\nFrom A\n@end\n";
            let b = "@extends \"./a.mds\"\n@block body:\nFrom B\n@end\n";
            let files = [("a.mds", a), ("b.mds", b)];
            let err = compile_virtual(&files, "a.mds")
                .expect_err("A3 E5: circular inheritance should error");
            assert_eq!(
                err.serialize().code,
                "mds::circular_import",
                "A3 E5: circular inheritance must be mds::circular_import, got: {:?}",
                err.serialize()
            );
        }

        // E7: @block name collides with @define name → mds::name_collision
        {
            let src = "@define body():\ncontent\n@end\n@block body:\nbody text\n@end\n";
            let err =
                crate::compile_str(src).expect_err("A3 E7: @block/@define collision should error");
            assert_eq!(
                err.serialize().code,
                "mds::name_collision",
                "A3 E7: @block vs @define must be mds::name_collision, got: {:?}",
                err.serialize()
            );
        }

        // E8: duplicate @block in same module → mds::name_collision
        {
            let src = "@block body:\nfirst\n@end\n@block body:\nsecond\n@end\n";
            let err = crate::compile_str(src).expect_err("A3 E8: duplicate @block should error");
            assert_eq!(
                err.serialize().code,
                "mds::name_collision",
                "A3 E8: duplicate @block must be mds::name_collision, got: {:?}",
                err.serialize()
            );
        }
    }
}
