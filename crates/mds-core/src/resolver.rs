mod frontmatter;
mod inheritance;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};

use crate::ast::{BlockNode, DefineBlock, ExportDirective, ImportDirective, Node};
use crate::error::MdsError;
use crate::evaluator::evaluate;
use crate::evaluator::evaluate_messages_intrinsic;
use crate::evaluator::evaluate_with_map;
use crate::fs::{FileSystem, NativeFs, VirtualFs};
use crate::lexer::tokenize;
use crate::limits::MAX_BLOCKS_PER_MODULE;
use crate::parser::parse_with_ctx;
use crate::scope::{FunctionDef, NamespaceScope, Scope};
use crate::validator;
use crate::value::Value;

use frontmatter::{build_scope_from_merged_mapping, deep_merge_yaml};
pub(crate) use frontmatter::{
    parse_frontmatter_imports, parse_frontmatter_imports_from_yaml, FrontmatterImport,
};
use inheritance::{
    apply_block_overrides, check_child_only_blocks, seed_effective_blocks, splice_skeleton,
    spliced_regions,
};

/// The display name and source bytes that a set of AST node offsets index into.
///
/// Each spliced region (skeleton non-block nodes, or a block's effective body) carries
/// the `Origin` of the file whose source bytes those AST offsets are relative to.
/// Validation runs per region against `origin.source` so span construction is always
/// in-bounds (fixing the cross-source-offset `OutOfBounds` diagnostic bug).
///
/// `Clone` = two refcount bumps (O(1)).
///
/// # Debug output
///
/// The manual `Debug` impl prints `file` + `source.len()` bytes — NEVER the raw source
/// text. This aligns with the `debug-panics` no-leak rule (source bytes must not appear
/// in panic messages or debug output).
#[derive(Clone)]
pub(crate) struct Origin {
    /// Display name of the file (shown in error messages / source labels).
    pub(crate) file: Arc<str>,
    /// Raw source bytes; AST node offsets in this region are relative to this string.
    pub(crate) source: Arc<str>,
}

impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Origin")
            .field("file", &self.file)
            .field("source_len", &self.source.len())
            .finish()
    }
}

/// An effective block together with the origin (file + source) its offsets index into.
///
/// The origin follows the **winning override**: if a child file overrides the block,
/// the origin is the child's file; if the base default is used, the origin is the
/// base's file. This is stamped at the last-wins insertion in `apply_block_overrides`.
///
/// `Clone` = three refcount bumps (one `Arc<BlockNode>` + two `Arc<str>` inside `Origin`).
pub(crate) struct EffectiveBlock {
    pub(crate) node: Arc<BlockNode>,
    pub(crate) origin: Origin,
}

impl std::fmt::Debug for EffectiveBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectiveBlock")
            .field("name", &self.node.name)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl Clone for EffectiveBlock {
    fn clone(&self) -> Self {
        Self {
            node: Arc::clone(&self.node),
            origin: self.origin.clone(),
        }
    }
}

/// A resolved module with its AST, exports, and prompt body.
///
/// Fields are `pub(crate)` — all external access must go through the methods
/// (`get_export`, `get_all_exports`, `get_prompt_value`, `to_namespace`) which
/// enforce export-visibility logic. Direct field access bypasses that logic.
///
/// # Template Inheritance Fields
///
/// - `effective_skeleton`: the root-ancestor body as a shared `Arc<[Node]>`. For a
///   non-extending module this is the module's own body (built once; Arc-shared across
///   all extending descendants). For an extending module it is `Arc::clone` of the
///   base's skeleton — never a deep-clone of the `Vec<Node>` (DoS guard, P1).
///
/// - `effective_blocks`: name → fully-overridden `EffectiveBlock` (block + origin).
///   For non-extending modules it is seeded from the module's own `@block` declarations.
///   For extending modules it is a clone of `base.effective_blocks` with the child's
///   overrides applied (most-derived wins, diamond-inheritance safe — NEVER mutate the
///   cached base map). Each block's `origin` follows the winning override file.
///
/// - `skeleton_origin`: the `Origin` (file + source) for skeleton non-block nodes —
///   i.e. the root-base file. Used by `spliced_regions` to validate non-block skeleton
///   content against the correct source.
///
/// - `frontmatter_values`: the module's parsed YAML mapping. For intermediate bases in a
///   chain this is the transitive accumulated deep-merge of all ancestors' FM, so a leaf
///   descending from it gets the full chain without re-traversing.
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
///   skeleton's `effective_skeleton` / `effective_blocks` / `skeleton_origin` Arcs so
///   descendants that already `Arc::clone`'d them keep pointer-identity.
pub struct ResolvedModule {
    pub(crate) functions: HashMap<String, Arc<FunctionDef>>,
    pub(crate) prompt_body: Option<String>,
    /// Pre-computed source-map fragment for this module's `prompt` output.
    ///
    /// Populated by `process_module` when `ModuleCache::source_map_mode` is
    /// true and the module exports a non-empty `prompt` (S6).  `None` for
    /// skeleton entries, `@extends` modules (S8 will add those), and all
    /// non-source-map compilations (zero-cost AC-PERF-01).
    pub(crate) prompt_map: Option<Arc<crate::sourcemap::FragmentMap>>,
    pub(crate) raw_frontmatter: Option<String>,
    pub(crate) has_explicit_exports: bool,
    pub(crate) explicit_exports: HashSet<String>,
    /// Root-ancestor body, Arc-shared across all descendants (never deep-cloned).
    /// For non-extending modules: own body. For extending: Arc::clone of base's skeleton.
    pub(crate) effective_skeleton: Arc<[Node]>,
    /// Fully-overridden block map for this subtree. Seeded from own @block declarations
    /// (non-extending) or clone(base.effective_blocks)+child overrides (extending).
    /// Each entry carries its `Origin` (file + source) so validation can attribute
    /// diagnostics to the correct source file (fixes the cross-source OutOfBounds bug).
    pub(crate) effective_blocks: IndexMap<String, EffectiveBlock>,
    /// Origin of the root-ancestor skeleton: used to validate non-block skeleton nodes
    /// (top-level text / interpolations / @if / @for between @block placeholders) against
    /// the correct file. Arc::clone'd down the chain so deep multi-level chains have
    /// zero extra allocations.
    pub(crate) skeleton_origin: Origin,
    /// Parsed YAML frontmatter mapping. Reserved-key splitting deferred to future refactor.
    pub(crate) frontmatter_values: Option<serde_yaml_ng::Mapping>,
    /// `true` when this entry was produced by `process_module_skeleton` (resolved as an
    /// `@extends` base: collect-only, NO standalone validate/evaluate, `prompt_body = None`).
    ///
    /// Cache-poisoning guard (A1): a skeleton entry must NOT be returned to a caller that
    /// needs a fully-rendered standalone module. `resolve_by_key` detects this flag on a
    /// cache hit and upgrades the entry to a full compile, so the SAME file resolved first
    /// as a base and later as a standalone target yields correct output. (The intrinsic
    /// path never caches its entry module — `resolve_intrinsic_by_key` always re-computes —
    /// so the poisoning window only exists on the `resolve_by_key` path.)
    pub(crate) is_skeleton: bool,
}

impl std::fmt::Debug for ResolvedModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModule")
            .field("functions_count", &self.functions.len())
            .field(
                "prompt_body_len",
                &self.prompt_body.as_ref().map(|s| s.len()),
            )
            .field("has_explicit_exports", &self.has_explicit_exports)
            .field("effective_skeleton_len", &self.effective_skeleton.len())
            .field("effective_blocks_count", &self.effective_blocks.len())
            .field("skeleton_origin", &self.skeleton_origin)
            .field("is_skeleton", &self.is_skeleton)
            .finish_non_exhaustive()
    }
}

impl Clone for ResolvedModule {
    fn clone(&self) -> Self {
        Self {
            functions: self.functions.clone(),
            prompt_body: self.prompt_body.clone(),
            prompt_map: self.prompt_map.clone(),
            raw_frontmatter: self.raw_frontmatter.clone(),
            has_explicit_exports: self.has_explicit_exports,
            explicit_exports: self.explicit_exports.clone(),
            effective_skeleton: Arc::clone(&self.effective_skeleton),
            effective_blocks: self.effective_blocks.clone(),
            skeleton_origin: self.skeleton_origin.clone(),
            frontmatter_values: self.frontmatter_values.clone(),
            is_skeleton: self.is_skeleton,
        }
    }
}

/// Maximum import depth to prevent stack overflow from deeply chained imports.
const MAX_IMPORT_DEPTH: usize = 64;

/// Display label and cycle-detection key for in-memory string-source compilation.
///
/// Used as `ctx.file_str` (shown in diagnostics) and as the cycle-detection key
/// pushed onto `self.resolving` in `resolve_source`/`resolve_source_intrinsic`.
/// The value `"<source>"` surfaces in miette diagnostic output (e.g. `<source>:3:1`).
const SOURCE_LABEL: &str = "<source>";

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
    /// Set to `true` by `process_module_intrinsic_opts` when `opts.source_map`
    /// is enabled.  When true, `process_module` builds a
    /// [`crate::sourcemap::FragmentMap`] alongside the prompt body for every
    /// standalone module that exports a non-empty `prompt`.
    ///
    /// Zero-cost path: when false (the default), no `MapBuilder` is allocated
    /// for sub-modules (AC-PERF-01).
    source_map_mode: bool,
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
            source_map_mode: false,
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
            source_map_mode: false,
        }
    }

    /// Create a `ModuleCache` with a custom [`FileSystem`] implementation.
    pub fn with_fs(fs: Box<dyn FileSystem>) -> Self {
        Self {
            fs,
            modules: IndexMap::new(),
            resolving: IndexSet::new(),
            source_map_mode: false,
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

    /// Resolve a module from a filesystem path string, dispatching on output shape.
    ///
    /// Output shape is intrinsic to the template: a template containing any `@message`
    /// block resolves to [`crate::CompiledOutput::Messages`], otherwise to
    /// [`crate::CompiledOutput::Markdown`]. Routes the entry through the filesystem
    /// normalizer (so `check_symlink` and `MAX_FILE_SIZE` are enforced on the entry),
    /// then resolves via `process_module_intrinsic`.
    pub fn resolve_path_intrinsic(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        let key = self.fs.normalize("", path)?;
        self.resolve_intrinsic_by_key(&key, runtime_vars, warnings)
    }

    /// Like [`resolve_path_intrinsic`] but accepts [`crate::CompileOptions`] and
    /// returns `(CompiledOutput, Option<SourceMap>)`.
    pub fn resolve_path_intrinsic_opts(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let key = self.fs.normalize("", path)?;
        self.resolve_intrinsic_by_key_opts(&key, runtime_vars, opts, warnings)
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

        let base_dir_owned = self.fs.parent_dir(key);
        let ctx = ModuleCtx {
            file_str: key,
            source: &source,
            base_dir: &base_dir_owned,
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
        // Arc-shared skeleton/blocks/origin so descendants that already Arc::clone'd them
        // keep pointer-identity. The freshly compiled `resolved` carries the correct
        // prompt_body (the skeleton-vs-standalone difference is solely validate/evaluate).
        if let Some(prev) = self.modules.get(key) {
            if prev.is_skeleton {
                resolved.effective_skeleton = Arc::clone(&prev.effective_skeleton);
                resolved.effective_blocks = prev.effective_blocks.clone();
                resolved.skeleton_origin = prev.skeleton_origin.clone();
            }
        }

        // Wrap in Arc, store in cache, and return a clone of the Arc (O(1)).
        let key_owned = key.to_string();
        let arc = Arc::new(resolved);
        self.modules.insert(key_owned, Arc::clone(&arc));

        Ok(arc)
    }

    /// Resolve an import relative to an explicit directory `base_dir`.
    ///
    /// Validates the import path, resolves it via `FileSystem::normalize_in_dir`
    /// (directory-anchored — no sentinel-path coupling), then delegates to
    /// [`ModuleCache::resolve_by_key`].
    fn resolve_import_from(
        &mut self,
        base_dir: &str,
        relative: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        validate_import_path(relative)?;
        let key = self.fs.normalize_in_dir(base_dir, relative)?;
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

        // Guard against re-entrant or cyclic calls that could form a cycle
        // back through this root module. Mirrors the resolving bookkeeping in
        // resolve_by_key so that cycle detection and depth checks apply to the
        // root module as well.
        self.check_import_depth()?;
        self.resolving.insert(SOURCE_LABEL.into());

        let ctx = ModuleCtx {
            file_str: SOURCE_LABEL,
            source,
            base_dir: &canonical_str,
            runtime_vars,
        };
        let resolved = self.process_module(&ctx, false, warnings);

        let popped = self.resolving.pop();
        Self::check_lifo_pop(resolved, popped, SOURCE_LABEL).map(Arc::new)
    }

    /// Resolve a virtual-filesystem entry by key, dispatching on output shape.
    ///
    /// Output shape is intrinsic to the template: a template containing any `@message`
    /// block resolves to [`crate::CompiledOutput::Messages`], otherwise to
    /// [`crate::CompiledOutput::Markdown`]. This is the entry point for virtual
    /// filesystems (use with [`ModuleCache::virtual_fs`]); the entry source is read
    /// from the cache's [`FileSystem`] backend.
    pub fn resolve_virtual_intrinsic(
        &mut self,
        entry: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        self.resolve_intrinsic_by_key(entry, runtime_vars, warnings)
    }

    /// Like [`resolve_virtual_intrinsic`] but accepts [`crate::CompileOptions`] and
    /// returns `(CompiledOutput, Option<SourceMap>)`.
    pub fn resolve_virtual_intrinsic_opts(
        &mut self,
        entry: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        self.resolve_intrinsic_by_key_opts(entry, runtime_vars, opts, warnings)
    }

    /// Resolve a module by its normalized key, dispatching on output shape.
    ///
    /// Shared core of [`resolve_path_intrinsic`] and [`resolve_virtual_intrinsic`].
    /// A single `process_module_intrinsic` pass over the entry module (no prior
    /// text-mode evaluation). Imported sub-modules are resolved through the normal
    /// cache (`resolve_by_key`) inside `collect_definitions_and_imports`, so they
    /// are evaluated only once.
    fn resolve_intrinsic_by_key(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        self.resolve_intrinsic_by_key_opts(key, runtime_vars, &Default::default(), warnings)
            .map(|(output, _)| output)
    }

    /// Like [`resolve_intrinsic_by_key`] but accepts opts and returns
    /// `(CompiledOutput, Option<SourceMap>)`.
    fn resolve_intrinsic_by_key_opts(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
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

        let base_dir_owned = self.fs.parent_dir(key);
        let ctx = ModuleCtx {
            file_str: key,
            source: &source,
            base_dir: &base_dir_owned,
            runtime_vars,
        };
        let result = self.process_module_intrinsic_opts(&ctx, is_md, opts, warnings);

        let popped = self.resolving.pop();
        Self::check_lifo_pop(result, popped, key)
    }

    /// Resolve a module from an in-memory source string, dispatching on output shape.
    ///
    /// Like [`resolve_source`] but dispatches on `has_message_block`, returning a
    /// [`crate::CompiledOutput`] (Markdown or Messages) instead of a rendered string.
    pub fn resolve_source_intrinsic(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        let canonical_str = self.fs.canonicalize(base_dir)?;
        self.fs.set_root(&canonical_str)?;
        self.check_import_depth()?;
        self.resolving.insert(SOURCE_LABEL.into());
        let ctx = ModuleCtx {
            file_str: SOURCE_LABEL,
            source,
            base_dir: &canonical_str,
            runtime_vars,
        };
        let result = self.process_module_intrinsic(&ctx, false, warnings);
        let popped = self.resolving.pop();
        Self::check_lifo_pop(result, popped, SOURCE_LABEL)
    }

    /// Like [`resolve_source_intrinsic`] but accepts [`crate::CompileOptions`] and
    /// returns `(CompiledOutput, Option<SourceMap>)`.
    pub fn resolve_source_intrinsic_opts(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let canonical_str = self.fs.canonicalize(base_dir)?;
        self.fs.set_root(&canonical_str)?;
        self.check_import_depth()?;
        self.resolving.insert(SOURCE_LABEL.into());
        let ctx = ModuleCtx {
            file_str: SOURCE_LABEL,
            source,
            base_dir: &canonical_str,
            runtime_vars,
        };
        let result = self.process_module_intrinsic_opts(&ctx, false, opts, warnings);
        let popped = self.resolving.pop();
        Self::check_lifo_pop(result, popped, SOURCE_LABEL)
    }

    /// Evaluate spliced `@extends` regions, accumulating output while recording
    /// source-map segments.
    ///
    /// Each region carries its own [`Origin`] so the builder's `current_src` is
    /// updated to the correct source index before evaluating that region.  Scope
    /// is shared across all regions (functions defined in earlier regions are
    /// visible to later ones).
    ///
    /// PF-004: cumulative output size is checked after each region rather than
    /// only at the end so runaway base templates are caught early.
    fn evaluate_regions_with_map(
        regions: &[(&[crate::ast::Node], &Origin)],
        scope: &mut crate::scope::Scope,
        warnings: &mut Vec<String>,
        map: Option<crate::sourcemap::MapBuilder>,
    ) -> Result<(String, Option<crate::sourcemap::MapBuilder>), MdsError> {
        let mut output = String::new();
        let mut current_map = map;

        for (nodes, origin) in regions {
            // Switch the builder's current_src to the source that owns this region.
            if let Some(ref mut builder) = current_map {
                let src_idx = builder.source_index(origin.file.as_ref(), origin.source.as_ref());
                builder.current_src = src_idx;
                // Cursor must equal accumulated output length before entering each region.
                // evaluate_with_map maintains this invariant internally, but after each
                // region we push the raw output and the builder's cursor is already correct.
            }

            let region_output = if let Some(builder) = current_map.take() {
                let (region_out, returned_builder) =
                    evaluate_with_map(nodes, scope, warnings, builder)?;
                current_map = Some(returned_builder);
                region_out
            } else {
                evaluate(nodes, scope, warnings)?
            };

            // PF-004: cumulative size guard — same limit as the per-node check.
            let new_len = output.len() + region_output.len();
            if new_len > crate::limits::MAX_OUTPUT_SIZE {
                return Err(MdsError::resource_limit(format!(
                    "output exceeds maximum size of {} bytes",
                    crate::limits::MAX_OUTPUT_SIZE
                )));
            }
            output.push_str(&region_output);
        }

        Ok((output, current_map))
    }

    /// Common intrinsic processing: tokenize, parse, build scope, then dispatch on
    /// output shape.
    ///
    /// Shares setup with `process_module` but dispatches on `has_message_block` at the
    /// end: a body containing any `@message` block evaluates to
    /// [`crate::CompiledOutput::Messages`] (via `evaluate_messages_intrinsic`), otherwise
    /// to [`crate::CompiledOutput::Markdown`] (via `evaluate`, then `clean_output` +
    /// `prepend_frontmatter`).
    ///
    /// When the parsed module has an `@extends` directive the shared extends pipeline
    /// (`resolve_extends_components`) builds `final_body` and `scope` identically to
    /// text mode — then the dispatch is performed on `final_body` (NOT `module.body`),
    /// so @message blocks inside base @block defaults are correctly detected (avoids
    /// PF-004 divergence, decision #8).
    fn process_module_intrinsic(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        self.process_module_intrinsic_opts(ctx, is_md, &Default::default(), warnings)
            .map(|(output, _)| output)
    }

    /// Opts-bearing variant of `process_module_intrinsic`.
    ///
    /// Returns `(CompiledOutput, Option<SourceMap>)`.  The source map is
    /// populated when `opts.source_map` is `true`; `None` otherwise.
    ///
    /// For Markdown output the source map is built by threading a
    /// [`crate::sourcemap::MapBuilder`] through the evaluator:
    ///
    /// - Standalone path: [`evaluate_with_map`] on `module.body`.
    /// - `@extends` path: [`evaluate_regions_with_map`] over
    ///   `spliced_regions(skeleton, effective_blocks, skeleton_origin)` so each
    ///   region's segments are attributed to the correct source file.
    ///
    /// Messages templates do not generate a source map (the source map covers
    /// compiled Markdown only; messages mode is not a single contiguous string).
    fn process_module_intrinsic_opts(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        // Gate sub-module FragmentMap construction on whether source maps are
        // requested for this compilation.  Must be set before any import
        // resolution (collect_definitions_and_imports / resolve_frontmatter_imports)
        // so that transitively-imported modules also build their FragmentMaps.
        // Zero-cost path: when opts.source_map=false this remains false and no
        // MapBuilder is allocated for sub-modules (AC-PERF-01).
        self.source_map_mode = opts.source_map;

        let tokens = tokenize(ctx.source, ctx.file_str)?;
        let module = parse_with_ctx(&tokens, ctx.file_str, ctx.source)?;

        let raw_frontmatter = module.frontmatter.as_ref().map(|fm| fm.raw.clone());

        // ── Extends branch ────────────────────────────────────────────────────
        if let Some(ext) = module.extends.clone() {
            let frontmatter_values = parse_frontmatter_mapping(module.frontmatter.as_ref())?;
            let components =
                self.resolve_extends_components(&module, &ext, ctx, &frontmatter_values, warnings)?;

            {
                let mut scope = components.scope.clone();
                Self::validate_extends_components(&components, &mut scope)?;
            }

            let ExtendsComponents {
                final_body,
                mut scope,
                merged_frontmatter,
                effective_skeleton,
                effective_blocks,
                skeleton_origin,
                ..
            } = components;

            if has_message_block(&final_body) {
                let messages = evaluate_messages_intrinsic(
                    &final_body,
                    &mut scope,
                    warnings,
                    ctx.file_str,
                    ctx.source,
                )?;
                return Ok((
                    crate::CompiledOutput::Messages(
                        messages.into_iter().map(crate::Message::from).collect(),
                    ),
                    None, // source maps cover Markdown only
                ));
            }

            let (body_raw, map_out) = if opts.source_map {
                // Per-region evaluation so each block's segments carry the
                // correct source origin (base template vs. child override).
                let regions =
                    spliced_regions(&effective_skeleton, &effective_blocks, &skeleton_origin);
                // Seed builder with the skeleton's root file.
                let builder = crate::sourcemap::MapBuilder::new(
                    skeleton_origin.file.to_string(),
                    skeleton_origin.source.to_string(),
                );
                let (raw, returned) =
                    Self::evaluate_regions_with_map(&regions, &mut scope, warnings, Some(builder))?;
                (raw, returned)
            } else {
                (evaluate(&final_body, &mut scope, warnings)?, None)
            };

            let body_clean = crate::clean_output(&body_raw);
            let body_clean_len = body_clean.len();
            let final_str = crate::prepend_frontmatter(merged_frontmatter.as_deref(), body_clean);
            let fm_prefix_len = final_str.len() - body_clean_len;
            let source_map =
                map_out.map(|b| b.finalize(&body_raw, &final_str, fm_prefix_len, None));
            return Ok((crate::CompiledOutput::Markdown(final_str), source_map));
        }

        // ── Standalone path ───────────────────────────────────────────────────
        let (mut scope, fm_imports) =
            build_scope_from_frontmatter(module.frontmatter.as_ref(), is_md, ctx.runtime_vars)?;

        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            explicit_exports,
            ..
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        validate_exports(&explicit_exports, &functions)?;

        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        if has_message_block(&module.body) {
            let messages = evaluate_messages_intrinsic(
                &module.body,
                &mut scope,
                warnings,
                ctx.file_str,
                ctx.source,
            )?;
            return Ok((
                crate::CompiledOutput::Messages(
                    messages.into_iter().map(crate::Message::from).collect(),
                ),
                None, // source maps cover Markdown only
            ));
        }

        let (body_raw, map_out) = if opts.source_map {
            let builder =
                crate::sourcemap::MapBuilder::new(ctx.file_str.to_string(), ctx.source.to_string());
            let (raw, returned) = evaluate_with_map(&module.body, &mut scope, warnings, builder)?;
            (raw, Some(returned))
        } else {
            (evaluate(&module.body, &mut scope, warnings)?, None)
        };

        let body_clean = crate::clean_output(&body_raw);
        let body_clean_len = body_clean.len();
        let final_str = crate::prepend_frontmatter(raw_frontmatter.as_deref(), body_clean);
        let fm_prefix_len = final_str.len() - body_clean_len;
        let source_map = map_out.map(|b| b.finalize(&body_raw, &final_str, fm_prefix_len, None));
        Ok((crate::CompiledOutput::Markdown(final_str), source_map))
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
    /// `ctx.file_str` is the display path for error messages (may be `SOURCE_LABEL`).
    /// `ctx.base_dir` is the directory used to resolve relative `@import`/`@extends`.
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
            return self.process_module_extends(module, ext, ctx, frontmatter_values, warnings);
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

        // Determine whether "prompt" is an available export for this module.
        // Mirrors the `is_exported("prompt")` logic on `ResolvedModule`.
        let prompt_exported = !has_explicit_exports || explicit_exports.contains("prompt");

        // Evaluate the body to get prompt text (and optionally a FragmentMap).
        //
        // When source-map mode is active and the module exports "prompt", run a
        // single evaluate_with_map pass to collect both the body string and the
        // segment records in one traversal (no double-evaluation).  The resulting
        // FragmentMap is cached in ResolvedModule and cloned into every NamespaceScope
        // that imports this module, enabling @include splice attribution (S6).
        let (prompt_body, prompt_map) = if self.source_map_mode && prompt_exported {
            let builder =
                crate::sourcemap::MapBuilder::new(ctx.file_str.to_string(), ctx.source.to_string());
            let (body_raw, returned) =
                evaluate_with_map(&module.body, &mut scope, warnings, builder)?;
            let body = (!body_raw.trim().is_empty()).then_some(body_raw);
            // Only keep the FragmentMap when the body is non-empty — an empty prompt
            // has no segments worth recording.
            let fmap = body.as_ref().map(|_| {
                Arc::new(crate::sourcemap::FragmentMap {
                    sources: returned
                        .sources
                        .iter()
                        .zip(returned.sources_content.iter())
                        .map(|(p, c)| (Arc::from(p.as_str()), Arc::from(c.as_str())))
                        .collect(),
                    segments: returned.segments,
                })
            });
            (body, fmap)
        } else {
            let body_raw = evaluate(&module.body, &mut scope, warnings)?;
            let body = (!body_raw.trim().is_empty()).then_some(body_raw);
            (body, None)
        };

        // Build Origin once for this module — Arc::clone'd into each EffectiveBlock (P3).
        let origin = Origin {
            file: Arc::from(ctx.file_str),
            source: Arc::from(ctx.source),
        };

        // Build effective_blocks first so module.body can be moved into the Arc below.
        let effective_blocks = seed_effective_blocks(&module.body, &block_names, &origin);

        // Move module.body into Arc<[Node]> (reuses the Vec allocation, no element clones, P1).
        let effective_skeleton: Arc<[Node]> = Arc::from(module.body);

        Ok(ResolvedModule {
            functions,
            prompt_body,
            prompt_map,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin: origin,
            frontmatter_values,
            is_skeleton: false,
        })
    }

    /// Build the merged scope for an `@extends` child (step 3d of `resolve_extends_components`).
    ///
    /// Covers steps 3d-i through 3d-vi:
    /// - Parse FM imports from base and child frontmatter (3d-i).
    /// - Deep-merge base and child FM mappings (3d-ii).
    /// - Build scope from the merged mapping with runtime vars (3d-iii).
    /// - Resolve base FM imports against the base file (3d-iv, ADR-014 ordering).
    /// - Resolve child FM imports against the child file (3d-v).
    /// - Merge base functions into scope (3d-vi).
    ///
    /// # Borrow note
    /// `base` is taken as an explicit `&ResolvedModule` (not via `self`) so that the
    /// borrow of `base.skeleton_origin.source` (needed for `base_ctx.source`) is
    /// independent of `&mut self`. The caller passes `&*arc` to deref from `Arc`.
    ///
    /// # Invariants preserved
    /// - Base FM imports resolved BEFORE child FM imports (ADR-014).
    /// - `deep_merge_yaml` applies `MAX_FRONTMATTER_MERGE_DEPTH` cap.
    /// - `resolve_frontmatter_imports` → `resolve_import_from` → `resolve_by_key_skeleton`
    ///   preserves PF-004 safety (cycle detection, `check_import_depth`, file-size cap).
    fn build_merged_extends_scope(
        &mut self,
        base: &ResolvedModule,
        child_fm: &Option<serde_yaml_ng::Mapping>,
        base_key: &str,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(Scope, serde_yaml_ng::Mapping), MdsError> {
        // 3d-i: Extract frontmatter imports from BOTH base and child BEFORE the deep merge.
        //
        // Base imports resolve relative to the BASE file's directory (base_base_dir, derived
        // from base_key via FileSystem::parent_dir). Child imports resolve relative to the
        // CHILD file's directory (ctx.base_dir). Both sets are resolved; a duplicate alias
        // across base+child → mds::name_collision (ADR-014).
        let base_fm_imports: Vec<FrontmatterImport> = base
            .frontmatter_values
            .as_ref()
            .and_then(|m| m.get("imports"))
            .map(parse_frontmatter_imports_from_yaml)
            .transpose()?
            .unwrap_or_default();

        let child_fm_imports: Vec<FrontmatterImport> = child_fm
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
        let child_mapping = child_fm.as_ref().unwrap_or(&empty_mapping);
        let merged_mapping = deep_merge_yaml(base_mapping, child_mapping, 0)?;

        // 3d-iii: Build scope from the merged mapping. Runtime vars applied LAST
        // (base < child < runtime, F7, decision #3).
        let mut scope = build_scope_from_merged_mapping(&merged_mapping, ctx.runtime_vars)?;

        // 3d-iv: Resolve base frontmatter imports against base_key (ADR-014 ordering,
        // PF-004 safe via resolve_frontmatter_imports → resolve_import_from).
        // Use a ctx pointing to the base file with its REAL source bytes so that any
        // span-carrying error here attributes correctly AND the at() debug_assert can't
        // false-fire. The skeleton_origin already carries these bytes; we use them here
        // for consistency.
        let base_source_ref: &str = &base.skeleton_origin.source;
        let base_base_dir = self.fs.parent_dir(base_key);
        let base_ctx = ModuleCtx {
            file_str: base_key,
            source: base_source_ref,
            base_dir: &base_base_dir,
            runtime_vars: ctx.runtime_vars,
        };
        self.resolve_frontmatter_imports(&base_fm_imports, &mut scope, &base_ctx, warnings)?;

        // 3d-v: Resolve child frontmatter imports against child directory (ctx.base_dir).
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

        Ok((scope, merged_mapping))
    }

    /// Shared extends-pipeline: steps 3a-3e are identical for the text-cached and
    /// intrinsic paths.
    ///
    /// Builds the `final_body` (splice of base skeleton with effective block overrides)
    /// and the `scope` (deep-merged frontmatter + FM imports + functions) needed by
    /// both `process_module_extends` (cached text path) and `process_module_intrinsic`.
    ///
    /// Callers differ only in the terminal step (step 3f):
    /// - Cached text path: `validate` → `evaluate(&final_body, …)`
    /// - Intrinsic path:   `has_message_block` dispatch → `evaluate_messages_intrinsic`
    ///   (Messages) or `evaluate` + clean/frontmatter (Markdown)
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
            .normalize_in_dir(ctx.base_dir, &ext.path)
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

        // skeleton_origin is the root base's Origin — Arc::clone'd (O(1)), never re-allocated.
        // This is how the base file's source bytes "ride along" to the leaf for span attribution.
        let skeleton_origin = base.skeleton_origin.clone();

        // ── Step 3d: build merged scope (Phase 3: deep merge + per-file FM imports) ──
        // Applies decision #3 (base < child < runtime) and decision #7 (reserved-key
        // exclusion, array wholesale replace, both sets of FM imports resolved per-file).
        // The merged_mapping is also returned so we can serialize it for the output
        // frontmatter (#154: @extends emits deep-merged frontmatter).
        let (mut scope, merged_mapping) =
            self.build_merged_extends_scope(&base, frontmatter_values, &base_key, ctx, warnings)?;
        let merged_frontmatter = serialize_merged_frontmatter(&merged_mapping)?;

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
        let final_body = splice_skeleton(&effective_skeleton, &effective_blocks, &skeleton_origin);

        Ok(ExtendsComponents {
            final_body,
            scope,
            functions,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            has_explicit_exports,
            explicit_exports,
            merged_frontmatter,
        })
    }

    /// Validate an extends pipeline's spliced regions, each against its own origin source.
    ///
    /// Walks `spliced_regions` and calls `validator::validate` per region so each region's
    /// AST node offsets are paired with the correct source string (fixing the cross-source
    /// OutOfBounds diagnostic bug). The scope is threaded through all regions so `@define`
    /// / `@for` scope push/pop behaves identically to a whole-slice validate.
    ///
    /// This single helper is called by BOTH `process_module_extends` (cached text path)
    /// and `process_module_intrinsic` (@extends branch) — enforcing PF-004 parity: the two
    /// parallel paths can never drift because they share one implementation.
    ///
    /// ADR-016: re-validate at the leaf (on `final_body` regions), not at intermediate bases.
    fn validate_extends_components(
        components: &ExtendsComponents,
        scope: &mut Scope,
    ) -> Result<(), MdsError> {
        for (nodes, origin) in spliced_regions(
            &components.effective_skeleton,
            &components.effective_blocks,
            &components.skeleton_origin,
        ) {
            validator::validate(nodes, scope, &origin.file, &origin.source)?;
        }
        Ok(())
    }

    /// Evaluate an extending child template in text mode.
    ///
    /// Delegates the shared pipeline (steps 3a-3e) to `resolve_extends_components`,
    /// then runs `validate_extends_components` + `evaluate` on `final_body` (step 3f).
    ///
    /// Decision #2: base is NEVER validated/evaluated standalone — deferred to leaf.
    /// PF-004: base is read via resolve_by_key_skeleton (FileSystem trait, never std::fs).
    fn process_module_extends(
        &mut self,
        module: crate::ast::Module,
        ext: crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        frontmatter_values: Option<serde_yaml_ng::Mapping>,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        let components =
            self.resolve_extends_components(&module, &ext, ctx, &frontmatter_values, warnings)?;

        // ── Step 3f: validate + evaluate on final_body ────────────────────────
        // Validate per-region so each region's offsets are checked against the correct
        // source (fixes the cross-source OutOfBounds diagnostic bug). This is what makes
        // E12 work: a base default block referencing an undefined var is caught HERE
        // against the merged leaf scope. (ADR-016: re-validate dynamically-assembled content.)
        {
            let mut scope = components.scope.clone();
            Self::validate_extends_components(&components, &mut scope)?;
        }

        let ExtendsComponents {
            final_body,
            mut scope,
            functions,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            has_explicit_exports,
            explicit_exports,
            merged_frontmatter,
        } = components;

        let prompt_body = evaluate(&final_body, &mut scope, warnings)?;
        let prompt_body = (!prompt_body.trim().is_empty()).then_some(prompt_body);

        Ok(ResolvedModule {
            functions,
            prompt_body,
            // @extends modules do not carry a FragmentMap in S6; multi-source
            // region attribution for extending modules is deferred to S8.
            prompt_map: None,
            // #154: emit deep-merged frontmatter (base < child, reserved keys excluded)
            // rather than the child's raw frontmatter.
            raw_frontmatter: merged_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
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

        let base_dir_owned = self.fs.parent_dir(key);
        let ctx = ModuleCtx {
            file_str: key,
            source: &source,
            base_dir: &base_dir_owned,
            runtime_vars,
        };
        let resolved = self.process_module_skeleton(&ctx, is_md, warnings);

        let popped = self.resolving.pop();
        let resolved = Self::check_lifo_pop(resolved, popped, key)?;

        let arc = Arc::new(resolved);
        self.modules.insert(key.to_string(), Arc::clone(&arc));
        Ok(arc)
    }

    /// Handle the intermediate-base arm of `process_module_skeleton`.
    ///
    /// Called when a module being resolved as a skeleton itself extends a grandparent.
    /// Resolves the grandparent in skeleton mode, applies block overrides from the
    /// intermediate base's body, Arc::clone's the skeleton_origin, and computes the
    /// transitive frontmatter merge (grandparent FM < own FM) so that a leaf descending
    /// from this intermediate base gets the full chain without re-traversal.
    ///
    /// # Invariants preserved
    /// - `resolve_by_key_skeleton` (never `resolve_by_key`) — skeleton-only evaluation.
    /// - `check_import_depth` + cycle detection (`self.resolving`) apply via
    ///   `resolve_by_key_skeleton`.
    /// - `deep_merge_yaml` depth cap (`MAX_FRONTMATTER_MERGE_DEPTH`) applies transitively.
    /// - `skeleton_origin` Arc is cloned from the grandparent (ADR-022 ride-along).
    #[allow(clippy::type_complexity)]
    fn resolve_intermediate_base(
        &mut self,
        ext: &crate::ast::ExtendsDirective,
        own_fm: &Option<serde_yaml_ng::Mapping>,
        module_body: &[Node],
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<
        (
            Arc<[Node]>,
            IndexMap<String, EffectiveBlock>,
            Origin,
            Option<serde_yaml_ng::Mapping>,
        ),
        MdsError,
    > {
        validate_import_path(&ext.path)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;
        let grandparent_key = self
            .fs
            .normalize_in_dir(ctx.base_dir, &ext.path)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;
        let grandparent = self
            .resolve_by_key_skeleton(&grandparent_key, ctx.runtime_vars, warnings)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;

        // Child-only-blocks check for this intermediate base (3b).
        check_child_only_blocks(module_body, ctx)?;

        let eff_blocks = apply_block_overrides(&grandparent.effective_blocks, module_body, ctx)?;

        // skeleton_origin Arc::clone'd from grandparent — the root base's source bytes
        // ride down the chain so the leaf's validate_extends_components can attribute
        // non-block skeleton node diagnostics to the root file (Risk #2, ADR-022).
        let skel_origin = grandparent.skeleton_origin.clone();

        // Phase 3: transitive FM merge: grandparent.frontmatter_values < own_fm_values.
        // This produces the accumulated FM for this intermediate base, so a leaf
        // descending from it gets the full transitive chain without re-traversing.
        let empty = serde_yaml_ng::Mapping::new();
        let gp_fm = grandparent.frontmatter_values.as_ref().unwrap_or(&empty);
        let own_fm_ref = own_fm.as_ref().unwrap_or(&empty);
        let merged_fm = deep_merge_yaml(gp_fm, own_fm_ref, 0)?;
        let accumulated_fm = if merged_fm.is_empty() {
            None
        } else {
            Some(merged_fm)
        };

        Ok((
            Arc::clone(&grandparent.effective_skeleton),
            eff_blocks,
            skel_origin,
            accumulated_fm,
        ))
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
        // B's skeleton_origin = A's skeleton_origin (Arc::clone — root source rides down the chain).
        //
        // Phase 3: B's frontmatter_values must be the transitive deep-merge of A's accumulated FM
        // with B's own FM (A < B), so that when C later merges against B, it gets A+B+C.
        let (effective_skeleton, effective_blocks, skeleton_origin, frontmatter_values) =
            if let Some(ext) = module.extends.as_ref() {
                self.resolve_intermediate_base(ext, &own_fm_values, &module.body, ctx, warnings)?
            } else {
                // Root base: own body is the skeleton; blocks seeded from own @block declarations.
                // Build Origin once — Arc::clone'd into each seeded EffectiveBlock (P3).
                let root_origin = Origin {
                    file: Arc::from(ctx.file_str),
                    source: Arc::from(ctx.source),
                };
                // Seed blocks first so module.body can be moved into the Arc (no element clones, P1).
                let eff_blocks = seed_effective_blocks(&module.body, &block_names, &root_origin);
                let eff_skeleton: Arc<[Node]> = Arc::from(module.body);
                // Root base: frontmatter_values is its own raw FM (no ancestors).
                (eff_skeleton, eff_blocks, root_origin, own_fm_values)
            };

        // A1 invariant: is_skeleton=true implies prompt_body=None (evaluate was never called).
        // Pinned with debug_assert so a future refactor that accidentally adds evaluate on this path
        // is caught immediately in debug builds. (Full enum split is tech debt.)
        let prompt_body: Option<String> = None;
        debug_assert!(
            prompt_body.is_none(),
            "A1 invariant: skeleton entry (is_skeleton=true) must have prompt_body=None"
        );

        Ok(ResolvedModule {
            functions,
            prompt_body,
            // Skeleton entries are never evaluated — no fragment map is built.
            // A1 invariant: prompt_body=None and prompt_map=None for skeleton entries.
            prompt_map: None,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
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
            ExportDirective::Named { name, .. } => {
                defs.explicit_exports.insert(name.clone());
            }
            ExportDirective::ReExport {
                name,
                path: import_path,
                ..
            } => {
                // Resolve the source module and bring in the function for
                // re-export only. Per spec: "@export from does not bring the
                // symbol into the current file's scope".
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self.resolve_import_from(
                    ctx.base_dir,
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
            ExportDirective::Wildcard {
                path: import_path, ..
            } => {
                // Re-export all exports from the target module. These are
                // available to importers but NOT in the current file's scope.
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self.resolve_import_from(
                    ctx.base_dir,
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
            .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, warnings)
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
                        .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    scope.set_namespace(alias, resolved.to_namespace());
                }
                FrontmatterImport::Merge { path } => {
                    let resolved = self
                        .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, warnings)
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
                        .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, warnings)
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
            .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, warnings)
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
            .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, warnings)
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
        // Respect export visibility: prompt_body and prompt_map are only included
        // in the namespace when "prompt" is an available export (same rule as
        // get_prompt_value).  Arc::clone is O(1) — no segment data is copied.
        let (prompt_body, prompt_map) = if self.is_exported("prompt") {
            (self.prompt_body.clone(), self.prompt_map.clone())
        } else {
            (None, None)
        };
        NamespaceScope {
            functions,
            prompt_body,
            prompt_map,
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
/// - Cached text path: `validator::validate` → `evaluate(&final_body, …)`
/// - Intrinsic path:   `has_message_block` dispatch → `evaluate_messages_intrinsic`
///   (Messages) or `evaluate` + clean/frontmatter (Markdown)
struct ExtendsComponents {
    /// Spliced final body: base skeleton with effective block bodies inlined.
    final_body: Vec<Node>,
    /// Merged scope (base < child < runtime), with FM imports and functions loaded.
    scope: Scope,
    /// Merged function map (base functions + child overrides).
    functions: HashMap<String, Arc<FunctionDef>>,
    /// Root ancestor skeleton, Arc-shared (O(1), no deep-clone).
    effective_skeleton: Arc<[Node]>,
    /// Fully-overridden block map for this subtree, each entry carrying its `Origin`.
    effective_blocks: IndexMap<String, EffectiveBlock>,
    /// Origin of the root-ancestor skeleton (file + source for non-block skeleton nodes).
    skeleton_origin: Origin,
    /// Whether the child declared any `@export` directives.
    has_explicit_exports: bool,
    /// Named exports from the child.
    explicit_exports: HashSet<String>,
    /// Deep-merged frontmatter YAML string (base < child, reserved keys excluded).
    ///
    /// Replaces the child's raw frontmatter in the compiled output so that `@extends`
    /// children emit the full merged key set rather than only their own keys (#154).
    /// `None` when the merged mapping is empty (no non-reserved FM keys on either side).
    merged_frontmatter: Option<String>,
}

/// Bundle of borrowed per-module context threaded through the AST walk helpers.
struct ModuleCtx<'a> {
    /// Canonical display path of the source file (e.g. the path shown in error messages).
    file_str: &'a str,
    /// Raw file content used for source-span diagnostics (offset → line/column lookup).
    source: &'a str,
    /// Directory from which relative `@import`/`@extends` paths are resolved.
    ///
    /// For file-backed modules this is the parent directory of the normalized file key
    /// (computed via `FileSystem::parent_dir`). For string-source modules this is the
    /// canonicalized `base_dir` supplied by the caller.
    base_dir: &'a str,
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
        // Leaf nodes that can never contain @message blocks. Enumerated
        // explicitly so a future new Node variant fails to compile here
        // and at the sibling match in evaluate_messages_intrinsic, rather
        // than silently being misclassified as markdown.
        Node::Text(_)
        | Node::Interpolation(_)
        | Node::EscapedBrace { .. }
        | Node::Define(_)
        | Node::Import(_)
        | Node::Export(_)
        | Node::Include(_) => false,
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

/// Build the `effective_blocks` map for a root (non-extending) module.
///
/// Walks `body` and collects all `Node::Block` entries whose name appears in
/// `block_names`.  Output order follows body declaration order (IndexMap preserves
/// insertion order, giving deterministic diamond-inheritance merges downstream).
///
/// Each entry carries `origin` (Arc::clone — O(1)), so validation can attribute
/// diagnostics to the correct file. For a root base the origin is the base's own
/// file and source.
///
/// **Perf rule:** `origin` is built ONCE per module resolution (outside any loop)
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
    debug_assert!(
        source.is_char_boundary(offset),
        "attach_import_span: offset {offset} is not a UTF-8 char boundary in source (len={})",
        source.len()
    );
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

/// Serialize a deep-merged frontmatter `Mapping` into a YAML string suitable for
/// `prepend_frontmatter`.
///
/// Returns `Ok(None)` when `mapping` is empty (no non-reserved keys on either side — no
/// frontmatter block should be emitted). Otherwise returns `Ok(Some(yaml_string))` where
/// `yaml_string` is the canonical YAML representation of `mapping` with a trailing
/// newline, ready to be wrapped in `---…---` fences by `prepend_frontmatter`.
///
/// Used by `resolve_extends_components` for the `@extends` deep-merged FM output (#154).
///
/// # Errors
///
/// Returns `Err(MdsError::YamlError)` if YAML serialization of the merged mapping fails.
/// This is not expected for well-formed mappings produced by `deep_merge_yaml`, but the
/// failure MUST propagate as a compile error rather than silently dropping the emitted
/// frontmatter fence from the output (no swallowed errors at boundaries).
fn serialize_merged_frontmatter(
    mapping: &serde_yaml_ng::Mapping,
) -> Result<Option<String>, MdsError> {
    if mapping.is_empty() {
        return Ok(None);
    }
    let yaml = serde_yaml_ng::to_string(&serde_yaml_ng::Value::Mapping(mapping.clone()))
        .map_err(|e| MdsError::yaml_error(e.to_string()))?;
    let trimmed = yaml.trim_end();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!("{trimmed}\n")))
    }
}

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

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
