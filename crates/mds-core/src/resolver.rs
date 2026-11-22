//! Module resolution and cache state.
//!
//! | Module or items | Responsibility |
//! |---|---|
//! | `charges` | Module budget charges and limits |
//! | `context` | Per-module context and results of module walks |
//! | `frontmatter` | Frontmatter parse and merge helpers |
//! | `import_paths` | Path checks, source-map anchors, and import error and frontmatter helpers |
//! | `inheritance` | Effective blocks and region layout |
//! | `module` | Resolved module data, exports, and module type checks |
//! | `module_scope` | Definition collection, scope construction, and export validation |
//! | `ModuleCache`, its fields, constructors, and `dependencies` | The root owns the cache state and its transition funnel |
//! | `MAX_IMPORT_DEPTH`, `SOURCE_LABEL`, and the tree helpers | Shared limits, source identity, and tree charges |
//! | `commit_retained`, `pop_resolving`, `check_lifo_pop`, and `finish_root` | The cache transition funnel |
//! | Resolution entry points | Entry to the cache transition funnel |
//! | `evaluate_regions_with_map` and `merge_frontmatter` | The output and merge funnels |
//! | `attach_import_span` and `line_len_at` | Shared diagnostic spans |
//!
//! Children use `use super::{...}`. This module is the import hub.

mod charges;
mod context;
mod frontmatter;
mod import_paths;
mod inheritance;
mod module;
mod module_scope;

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};

use crate::ast::{BlockNode, DefineBlock, ExportDirective, ImportDirective, Module, Node};
use crate::error::MdsError;
use crate::evaluator::budget::{bytes, MemBudget, Part};
use crate::evaluator::{
    evaluate_messages_seeded, evaluate_seeded, evaluate_with_map_seeded, EvalBudget,
};
use crate::fs::{FileSystem, NativeFs, VirtualFs};
use crate::lexer::{
    mask_file_key, probe_file_key, tokenize, Token, FILE_KEY_FORM, INDENTED_DIRECTIVES,
};
use crate::limits::{
    MAX_BLOCKS_PER_MODULE, MAX_COMPILE_MEMORY, MAX_DEFINES_PER_MODULE, MAX_FILE_SIZE,
    MAX_MODULE_COUNT, MAX_SOURCES_CONTENT_BYTES, TREE_PER_EXPR_BYTE, TREE_PER_TEXT_BYTE,
    TREE_PER_TOKEN,
};
use crate::parser::parse_with_ctx;
use crate::scope::{FunctionDef, NamespaceScope, PromptText, Scope, VarLayer};
// Import Origin from sourcemap.rs to avoid a scope→resolver import cycle.
pub(crate) use crate::sourcemap::Origin;
use crate::sourcemap::{CompileOptions, MapBuilder, SourceIndex};
use crate::validator;
use crate::value::Value;

use context::{
    fragment_map_from, is_visible_export, CollectedDefs, ExtendsComponents, ImportSite, ModuleCtx,
};
use frontmatter::{
    build_scope_from_merged_mapping, deep_merge_yaml, parse_frontmatter_yaml_with, Allowance,
};
pub(crate) use frontmatter::{
    parse_frontmatter_imports, parse_frontmatter_imports_from_yaml, parse_frontmatter_yaml,
    FrontmatterImport,
};
#[cfg(test)]
use import_paths::ImportPathViolation;
use import_paths::{
    attach_frontmatter_index, build_cycle_string, import_path_violation, parse_frontmatter_mapping,
    validate_import_path,
};
use inheritance::{
    apply_block_overrides, check_child_only_blocks, seed_effective_blocks, spliced_regions,
};
#[cfg(test)]
use module::has_type_mds_frontmatter;
use module::has_type_mds_frontmatter_raw;
pub use module::{check_module_type, ModuleKey, ModuleRef};
pub(crate) use module::{EffectiveBlock, ResolvedModule};
use module_scope::{
    build_scope_from_frontmatter, collect_block, collect_define, count_module_defines,
    has_message_block, runtime_layer, validate_exports,
};

// `Origin` is defined in `sourcemap.rs` and re-exported above via `pub(crate) use`.
// It carries (file: Arc<str>, source: Arc<str>) — the defining module's display
// path and raw source content for source-map provenance (S7/S8).

/// Maximum import depth to prevent stack overflow from deeply chained imports.
const MAX_IMPORT_DEPTH: usize = 64;

/// Display label and cycle-detection key for in-memory string-source compilation.
///
/// Used as `ctx.file_str` (shown in diagnostics) and as the cycle-detection key
/// pushed onto `self.resolving` in `resolve_source`/`resolve_source_intrinsic`.
/// The value `"<source>"` surfaces in miette diagnostic output (e.g. `<source>:3:1`).
///
/// `pub(crate)` so sibling modules can reference the single definition and stay in
/// sync automatically — no per-module copy needed.  Two consumers inside `mds-core`:
///
/// - `sourcemap::map_source_label` — compares against this constant directly.
/// - `error::MdsError::is_string_source` — wraps the comparison in a public
///   predicate; downstream crates (`mds-cli`) call that method rather than
///   hardcoding `"<source>"`, which would have no compile-time link to this
///   definition.  CLI callers MUST use `is_string_source()`, not this constant.
pub(crate) const SOURCE_LABEL: &str = "<source>";

/// Warning emitted when `source_map: true` is used with a messages-mode template.
///
/// Source maps operate on a flat text stream; messages-mode boundaries
/// (`@message` blocks) are not representable in the SMv3 segment model.
/// The warning is surface-neutral (no mention of specific option names or APIs).
const MSG_MODE_SOURCE_MAP_WARNING: &str =
    "source maps are not supported for messages-mode templates (@message blocks); \
     no source map will be generated";

/// The tree estimate of a module: its tokens, its directive and interpolation text
/// and its source bytes, each times its factor. `None` is a count past the address
/// space, so the charge fails as a charge over the limit.
fn tree_bytes(tokens: usize, expr: usize, len: usize) -> Option<usize> {
    bytes(tokens, TREE_PER_TOKEN)?
        .checked_add(bytes(expr, TREE_PER_EXPR_BYTE)?)?
        .checked_add(bytes(len, TREE_PER_TEXT_BYTE)?)
}

pub(crate) fn tree_estimate(tokens: &[Token], len: usize) -> Option<usize> {
    let mut expr: usize = 0;
    for token in tokens {
        if let Token::Interpolation(text, _) | Token::Directive(text, _) = token {
            expr = expr.checked_add(text.len())?;
        }
    }
    tree_bytes(tokens.len(), expr, len)
}

/// Charge the tree estimate of an import scan against a new compile memory budget.
/// A scan holds one module, so its budget starts empty. It runs between the lexer
/// and the parser, so a refused tree is never built. A count past the address space
/// fails the charge.
pub(crate) fn charge_scan_tree(tokens: &[Token], len: usize) -> Result<(), MdsError> {
    MemBudget::default()
        .charge(Part::Temp, tree_estimate(tokens, len))
        .map(|_| ())
}

/// Module cache to avoid re-resolving the same file or virtual key.
///
/// Supports multiple filesystem backends via the [`FileSystem`] trait.
#[non_exhaustive]
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
    /// is enabled.  When true, `process_module` and `process_module_extends`
    /// build a [`crate::sourcemap::FragmentMap`] alongside the prompt body for
    /// every imported module — standalone or extending — that exports a
    /// non-empty `prompt`.
    ///
    /// Zero-cost path: when false (the default), no `MapBuilder` is allocated
    /// for sub-modules (AC-PERF-01).
    source_map_mode: bool,
    budget: EvalBudget,
    retained: usize,
    kept: usize,
    #[cfg(test)]
    module_probes: Vec<ModuleProbe>,
    #[cfg(test)]
    function_call_seed: usize,
    #[cfg(test)]
    temporaries_seed: usize,
    #[cfg(test)]
    retained_transfer_seed: usize,
}

struct ModuleMark {
    temporaries: usize,
    outer_kept: usize,
    #[cfg(test)]
    probe: usize,
}

#[cfg(test)]
#[derive(Default)]
struct ModuleProbe {
    buffers_before: usize,
    buffers_after_insert: Option<usize>,
    temporaries_before: usize,
    temporaries_open: usize,
    temporaries_closed: usize,
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
        Self::with_fs(Box::new(NativeFs::new()))
    }

    /// Create a `ModuleCache` backed by an in-memory virtual filesystem.
    ///
    /// Useful for testing and WASM environments where OS access is unavailable.
    pub fn virtual_fs(modules: HashMap<String, String>) -> Self {
        Self::with_fs(Box::new(VirtualFs::new(modules)))
    }

    /// Create a `ModuleCache` with a custom [`FileSystem`] implementation.
    pub fn with_fs(fs: Box<dyn FileSystem>) -> Self {
        Self {
            fs,
            modules: IndexMap::new(),
            resolving: IndexSet::new(),
            source_map_mode: false,
            budget: EvalBudget::default(),
            retained: 0,
            kept: 0,
            #[cfg(test)]
            module_probes: Vec::new(),
            #[cfg(test)]
            function_call_seed: 0,
            #[cfg(test)]
            temporaries_seed: 0,
            #[cfg(test)]
            retained_transfer_seed: 0,
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

    fn commit_retained(&mut self, kept: usize) {
        let (Some(buffers), Some(retained @ ..=MAX_COMPILE_MEMORY)) = (
            self.budget.mem.buffers.checked_sub(kept),
            self.retained.checked_add(kept),
        ) else {
            self.budget.mem.underflow = true;
            return;
        };
        self.budget.mem.buffers = buffers;
        self.retained = retained;
        self.budget.mem.headroom = MAX_COMPILE_MEMORY - retained;
    }

    /// Resolve a module from a filesystem path string.
    ///
    /// `path` is a UTF-8 string representation of the OS path (callers convert
    /// `&Path` to `&str` at the public API boundary via `path_to_str`).
    /// Validates `path` and resolves it to a key via [`FileSystem::resolve_entry`],
    /// then resolves through the module cache with cycle detection and depth guarding.
    pub fn resolve_path(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        let key = self.resolve_entry_key(path)?;
        let entry = ModuleRef {
            key: &key,
            typed: path,
        };
        self.resolve_by_key(entry, &runtime_layer(runtime_vars), None, warnings)
    }

    /// Resolve a module from a filesystem path string, dispatching on output shape.
    ///
    /// Output shape is intrinsic to the template: a template containing any `@message`
    /// block resolves to [`crate::CompiledOutput::Messages`], otherwise to
    /// [`crate::CompiledOutput::Markdown`]. Routes the entry through entry validation
    /// and [`FileSystem::resolve_entry`] (so `check_symlink` and `MAX_FILE_SIZE` are
    /// enforced on the entry), then resolves via `process_module_intrinsic`.
    pub fn resolve_path_intrinsic(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        self.resolve_path_intrinsic_source(path, runtime_vars, warnings)
            .map(|(output, _)| output)
    }

    /// Resolve the entry and return the exact source text read by its check gate.
    pub(crate) fn resolve_path_intrinsic_source(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, String), MdsError> {
        let key = self.resolve_entry_key(path)?;
        let entry = ModuleRef {
            key: &key,
            typed: path,
        };
        let ((output, _), source) = self.resolve_intrinsic_by_key_opts_source(
            entry,
            &runtime_layer(runtime_vars),
            &Default::default(),
            warnings,
        )?;
        Ok((output, source))
    }

    /// Like [`Self::resolve_path_intrinsic`] but accepts [`crate::CompileOptions`] and
    /// returns `(CompiledOutput, Option<SourceMap>)`.
    pub fn resolve_path_intrinsic_opts(
        &mut self,
        path: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let key = self.resolve_entry_key(path)?;
        let entry = ModuleRef {
            key: &key,
            typed: path,
        };
        self.resolve_intrinsic_by_key_opts(entry, &runtime_layer(runtime_vars), opts, warnings)
    }

    /// Resolve a module by its normalized key.
    ///
    /// This is the core resolution loop: cache check → depth check →
    /// cycle detection → read → validate type → process → cache insert.
    /// `module.typed` only names the module when it is not an MDS file (#417).
    fn resolve_by_key(
        &mut self,
        module: ModuleRef<'_>,
        runtime_vars: &Arc<VarLayer>,
        import_site: Option<ImportSite<'_>>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        let key = module.key;
        // Step 1: cache hit — return immediately without reading, UNLESS the cached
        // entry is a skeleton (resolved as an @extends base: collect-only, never
        // validated/evaluated standalone, prompt_body = None). A skeleton entry must
        // not be served to a standalone-compile caller — that would yield empty output
        // for the base. Fall through to a full compile and upgrade the entry in place,
        // reusing the skeleton's Arcs so existing descendants keep Arc-sharing (A1).
        let upgrade = match self.modules.get(key) {
            Some(cached) if !cached.is_skeleton => return Ok(Arc::clone(cached)),
            cached => cached.is_some(),
        };

        // Step 2: cycle detection — must happen before we push to `resolving`.
        if self.resolving.contains(key) {
            let cycle = build_cycle_string(&self.resolving, key);
            return Err(MdsError::circular_import(cycle));
        }

        // Step 3: depth guard.
        self.check_import_depth()?;

        // Step 3.5: module count guard (REL-2, applies PF-004).
        self.check_module_count(key)?;

        // Step 4: read the file only on a cache miss.
        let source = self.fs.read(key)?;

        // Step 5: determine if markdown (for frontmatter type-key handling).
        let is_md = self.fs.is_markdown(key);

        // Attach a type refusal here, before recursion. A spanless frontmatter
        // refusal from a child must not acquire this module's importing directive.
        check_module_type(module, &source).map_err(|err| match import_site {
            Some(site) => site.attach_type_error(err),
            None => err,
        })?;

        // Mark as resolving before recursing into process_module.
        // IndexSet preserves insertion order, so it serves as both the set (O(1) lookup)
        // and the ordered stack (for cycle path reconstruction).
        let (mark, read) = self.begin_module(key, runtime_vars, source.capacity());
        let resolved = read.and_then(|()| {
            let base_dir_owned = self.fs.parent_dir(key);
            let display_owned = crate::source_path::display_path_for(&*self.fs, key);
            let ctx = ModuleCtx {
                key,
                file_str: &display_owned,
                source: &source,
                base_dir: &base_dir_owned,
                runtime_vars,
                origin: OnceCell::new(),
            };
            self.process_module(&ctx, is_md, upgrade, warnings)
        });
        #[cfg(test)]
        let probe = mark.probe;
        let kept = self.close_module(mark);

        let popped = self.pop_resolving(key);
        let mut resolved = resolved?;
        popped?;

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

        let arc = Arc::new(resolved);
        #[cfg(test)]
        let kept = kept + self.retained_transfer_seed;
        self.retain_module(key, &arc, kept);
        #[cfg(test)]
        {
            self.module_probes[probe].buffers_after_insert = Some(self.budget.mem.buffers);
        }

        self.finish_root()?;
        Ok(arc)
    }

    /// Resolve an import relative to an explicit directory `base_dir`.
    ///
    /// Validates the import path, resolves it via `FileSystem::normalize_in_dir`
    /// (directory-anchored — no sentinel-path coupling), then delegates to
    /// [`ModuleCache::resolve_by_key`], which names the module by `relative` as written.
    fn resolve_import_from(
        &mut self,
        base_dir: &str,
        relative: &str,
        runtime_vars: &Arc<VarLayer>,
        import_site: Option<ImportSite<'_>>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        validate_import_path(relative)?;
        let key = self.fs.normalize_in_dir(base_dir, relative)?;
        let module = ModuleRef {
            key: &key,
            typed: relative,
        };
        self.resolve_by_key(module, runtime_vars, import_site, warnings)
    }

    /// Resolve an entry module by its key.
    ///
    /// This is the entry point for virtual filesystems where there is no OS path.
    /// Use this with [`ModuleCache::virtual_fs`] or a custom [`FileSystem`] backend.
    /// The key is an entry path like any other: it is validated and resolved
    /// through [`FileSystem::resolve_entry`] first, so on [`crate::NativeFs`] a
    /// symlinked key is refused, the first key anchors the project root, and a
    /// later key outside that root is refused.
    pub fn resolve_key(
        &mut self,
        key: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        let resolved = self.resolve_entry_key(key)?;
        let entry = ModuleRef {
            key: &resolved,
            typed: key,
        };
        self.resolve_by_key(entry, &runtime_layer(runtime_vars), None, warnings)
    }

    /// Resolve a module from an in-memory source string.
    ///
    /// Imports within the source are resolved relative to `base_dir`, which goes
    /// through [`FileSystem::anchor_base_dir`] first: [`crate::NativeFs`]
    /// canonicalizes it, refuses a symlinked directory and anchors the project
    /// root there; the default (in-memory backends) uses it unchanged as a
    /// key-space directory (`""` is the root). A `base_dir` carrying a forbidden
    /// path character is refused with `mds::io` before the backend sees it (#265).
    pub fn resolve_source(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        // Entry-size backstop for the string funnel (PF-004) — before any IO or parse.
        Self::check_source_size(source)?;
        // Anchor base_dir through the FileSystem abstraction so that custom or
        // virtual backends can override this behaviour (fixes issue #21).
        let canonical_str = self.anchor_base(base_dir)?;

        // Guard against re-entrant or cyclic calls that could form a cycle
        // back through this root module. Mirrors the resolving bookkeeping in
        // resolve_by_key so that cycle detection and depth checks apply to the
        // root module as well.
        self.check_import_depth()?;
        self.check_module_count(SOURCE_LABEL)?;
        let runtime = runtime_layer(runtime_vars);
        let (mark, read) = self.begin_module(SOURCE_LABEL, &runtime, 0);

        let ctx = ModuleCtx {
            key: SOURCE_LABEL,
            file_str: SOURCE_LABEL,
            source,
            base_dir: &canonical_str,
            runtime_vars: &runtime,
            origin: OnceCell::new(),
        };
        let resolved = read.and_then(|()| self.process_module(&ctx, false, false, warnings));
        self.close_module(mark);

        let popped = self.pop_resolving(SOURCE_LABEL);
        let resolved = resolved?;
        popped?;
        self.finish_root()?;
        Ok(Arc::new(resolved))
    }

    /// Resolve a virtual-filesystem entry by key, dispatching on output shape.
    ///
    /// Output shape is intrinsic to the template: a template containing any `@message`
    /// block resolves to [`crate::CompiledOutput::Messages`], otherwise to
    /// [`crate::CompiledOutput::Markdown`]. This is the entry point for virtual
    /// filesystems (use with [`ModuleCache::virtual_fs`]); the entry source is read
    /// from the cache's [`FileSystem`] backend. The entry key is validated and
    /// resolved through [`FileSystem::resolve_entry`] first, like every entry path.
    pub fn resolve_virtual_intrinsic(
        &mut self,
        entry: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        let key = self.resolve_entry_key(entry)?;
        let entry = ModuleRef {
            key: &key,
            typed: entry,
        };
        self.resolve_intrinsic_by_key(entry, &runtime_layer(runtime_vars), warnings)
    }

    /// Like [`Self::resolve_virtual_intrinsic`] but accepts [`crate::CompileOptions`] and
    /// returns `(CompiledOutput, Option<SourceMap>)`.
    pub fn resolve_virtual_intrinsic_opts(
        &mut self,
        entry: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let key = self.resolve_entry_key(entry)?;
        let entry = ModuleRef {
            key: &key,
            typed: entry,
        };
        self.resolve_intrinsic_by_key_opts(entry, &runtime_layer(runtime_vars), opts, warnings)
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
        entry: ModuleRef<'_>,
        runtime_vars: &Arc<VarLayer>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        self.resolve_intrinsic_by_key_opts(entry, runtime_vars, &Default::default(), warnings)
            .map(|(output, _)| output)
    }

    /// Like [`resolve_intrinsic_by_key`] but accepts opts and returns
    /// `(CompiledOutput, Option<SourceMap>)`.
    fn resolve_intrinsic_by_key_opts(
        &mut self,
        entry: ModuleRef<'_>,
        runtime_vars: &Arc<VarLayer>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        self.resolve_intrinsic_by_key_opts_source(entry, runtime_vars, opts, warnings)
            .map(|(result, _)| result)
    }

    fn resolve_intrinsic_by_key_opts_source(
        &mut self,
        entry: ModuleRef<'_>,
        runtime_vars: &Arc<VarLayer>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<((crate::CompiledOutput, Option<crate::SourceMap>), String), MdsError> {
        let key = entry.key;
        // Cycle detection: if this key is already on the resolving stack it forms
        // a circular import that must be rejected.
        if self.resolving.contains(key) {
            let cycle = build_cycle_string(&self.resolving, key);
            return Err(MdsError::circular_import(cycle));
        }

        self.check_import_depth()?;
        self.check_module_count(key)?;

        let source = self.fs.read(key)?;
        let is_md = self.fs.is_markdown(key);
        check_module_type(entry, &source)?;

        let (mark, read) = self.begin_module(key, runtime_vars, source.capacity());
        let result = read.and_then(|()| {
            let base_dir_owned = self.fs.parent_dir(key);
            let display_owned = crate::source_path::display_path_for(&*self.fs, key);
            let ctx = ModuleCtx {
                key,
                file_str: &display_owned,
                source: &source,
                base_dir: &base_dir_owned,
                runtime_vars,
                origin: OnceCell::new(),
            };
            self.process_module_intrinsic_opts(&ctx, is_md, opts, warnings)
        });
        self.close_module(mark);

        let popped = self.pop_resolving(key);
        let result = result?;
        popped?;
        self.finish_root()?;
        Ok((result, source))
    }

    /// Resolve a module from an in-memory source string, dispatching on output shape.
    ///
    /// Like [`Self::resolve_source`] but dispatches on `has_message_block`, returning a
    /// [`crate::CompiledOutput`] (Markdown or Messages) instead of a rendered string.
    pub fn resolve_source_intrinsic(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        warnings: &mut Vec<String>,
    ) -> Result<crate::CompiledOutput, MdsError> {
        Self::check_source_size(source)?;
        let canonical_str = self.anchor_base(base_dir)?;
        self.check_import_depth()?;
        self.check_module_count(SOURCE_LABEL)?;
        let runtime = runtime_layer(runtime_vars);
        let (mark, read) = self.begin_module(SOURCE_LABEL, &runtime, 0);
        let ctx = ModuleCtx {
            key: SOURCE_LABEL,
            file_str: SOURCE_LABEL,
            source,
            base_dir: &canonical_str,
            runtime_vars: &runtime,
            origin: OnceCell::new(),
        };
        let result = read.and_then(|()| self.process_module_intrinsic(&ctx, false, warnings));
        self.close_module(mark);
        let popped = self.pop_resolving(SOURCE_LABEL);
        let result = result?;
        popped?;
        self.finish_root()?;
        Ok(result)
    }

    /// Like [`Self::resolve_source_intrinsic`] but accepts [`crate::CompileOptions`] and
    /// returns `(CompiledOutput, Option<SourceMap>)`.
    pub fn resolve_source_intrinsic_opts(
        &mut self,
        source: &str,
        base_dir: &str,
        runtime_vars: &HashMap<String, Value>,
        opts: &crate::sourcemap::CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        Self::check_source_size(source)?;
        let canonical_str = self.anchor_base(base_dir)?;
        self.check_import_depth()?;
        self.check_module_count(SOURCE_LABEL)?;
        let runtime = runtime_layer(runtime_vars);
        let (mark, read) = self.begin_module(SOURCE_LABEL, &runtime, 0);
        let ctx = ModuleCtx {
            key: SOURCE_LABEL,
            file_str: SOURCE_LABEL,
            source,
            base_dir: &canonical_str,
            runtime_vars: &runtime,
            origin: OnceCell::new(),
        };
        let result =
            read.and_then(|()| self.process_module_intrinsic_opts(&ctx, false, opts, warnings));
        self.close_module(mark);
        let popped = self.pop_resolving(SOURCE_LABEL);
        let result = result?;
        popped?;
        self.finish_root()?;
        Ok(result)
    }

    /// Evaluate spliced `@extends` regions in order, accumulating their output and,
    /// when a `MapBuilder` is supplied, recording source-map segments.
    ///
    /// Each region is evaluated against its own [`Origin`] — the file its node
    /// offsets index into — so a span-bearing error names that file (#114). With a
    /// builder, its current source is switched to the region's source; without one,
    /// the region's display path and source are passed directly. Never
    /// `origin.file`: that is the canonical key (absolute for `NativeFs`) and must
    /// not reach a diagnostic (R3 / CWE-209). Scope is shared across all regions
    /// (functions defined in earlier regions are visible to later ones).
    ///
    /// One [`EvalBudget`] and one output buffer cover all regions, so the caps
    /// apply to the whole module evaluation.
    fn evaluate_regions_with_map(
        regions: &[(&[crate::ast::Node], &Origin)],
        scope: &mut crate::scope::Scope,
        warnings: &mut Vec<String>,
        map: Option<crate::sourcemap::MapBuilder>,
        budget: &mut EvalBudget,
    ) -> Result<(String, Option<crate::sourcemap::MapBuilder>), MdsError> {
        let mut output = String::new();
        let mut current_map = map;
        // The previous region's origin and source index: consecutive regions from one
        // file (every between-block skeleton node) reuse the index without a lookup.
        let mut previous: Option<(&Origin, SourceIndex)> = None;

        for (nodes, origin) in regions {
            // Switch the builder's current source to the one that owns this region —
            // switched for every region, a reused index included, so no region relies
            // on the one before it having restored it.
            if let Some(ref mut builder) = current_map {
                let src_idx = match previous {
                    Some((prev, idx)) if Arc::ptr_eq(&prev.file, &origin.file) => idx,
                    _ => builder.register(origin),
                };
                builder.switch_to_index(src_idx);
                previous = Some((origin, src_idx));
            }

            if let Some(builder) = current_map.take() {
                current_map = Some(evaluate_with_map_seeded(
                    nodes,
                    scope,
                    warnings,
                    builder,
                    budget,
                    &mut output,
                )?);
            } else {
                evaluate_seeded(
                    nodes,
                    scope,
                    warnings,
                    &origin.display,
                    &origin.source,
                    budget,
                    &mut output,
                )?;
            }
        }

        Ok((output, current_map))
    }

    /// Evaluate an `@extends` chain's spliced regions: the terminal step the entry's
    /// extends path (`process_module_intrinsic_opts`) and an imported extending module
    /// (`process_module_extends`) share.
    ///
    /// With a `seed` — the chain's `skeleton_origin`, so a map lists the root base
    /// first however the chain is reached — every region is recorded into a
    /// [`crate::sourcemap::MapBuilder`] seeded with it; without one nothing is
    /// recorded. The body is evaluated either way, all regions under one
    /// [`EvalBudget`] ([`Self::evaluate_regions_with_map`]).
    fn evaluate_extends_regions(
        regions: &[(&[crate::ast::Node], &Origin)],
        seed: Option<&Origin>,
        scope: &mut crate::scope::Scope,
        warnings: &mut Vec<String>,
        budget: &mut EvalBudget,
    ) -> Result<(String, Option<crate::sourcemap::MapBuilder>), MdsError> {
        let builder = seed.map(|origin| crate::sourcemap::MapBuilder::new(origin.clone()));
        Self::evaluate_regions_with_map(regions, scope, warnings, builder, budget)
    }

    /// Messages-mode twin of [`Self::evaluate_regions_with_map`]: collect the
    /// `@message` blocks of spliced `@extends` regions in order.
    ///
    /// Each region is evaluated against its own [`Origin`]'s display path and source,
    /// so a span-bearing error — orphan text outside a `@message`
    /// (`mds::mixed_content`) or a cross-type comparison — names the file the region
    /// came from (#115). PF-004: one message vector and one [`EvalBudget`] cover every
    /// region, so the message-count, message-byte and iteration caps apply to the
    /// whole module evaluation rather than to each region.
    fn evaluate_message_regions(
        regions: &[(&[crate::ast::Node], &Origin)],
        scope: &mut crate::scope::Scope,
        warnings: &mut Vec<String>,
        budget: &mut EvalBudget,
    ) -> Result<Vec<crate::evaluator::EvalMessage>, MdsError> {
        let mut messages = Vec::new();
        for (nodes, origin) in regions {
            evaluate_messages_seeded(
                nodes,
                scope,
                warnings,
                &origin.display,
                &origin.source,
                budget,
                &mut messages,
            )?;
        }
        Ok(messages)
    }

    /// Common intrinsic processing: tokenize, parse, build scope, then dispatch on
    /// output shape.
    ///
    /// Shares setup with `process_module` but dispatches on `has_message_block` at the
    /// end: a body containing any `@message` block evaluates to
    /// [`crate::CompiledOutput::Messages`] (via `evaluate_messages_seeded`), otherwise
    /// to [`crate::CompiledOutput::Markdown`] (via `evaluate_seeded` or mapped evaluation,
    /// then `finalize_markdown`).
    ///
    /// When the parsed module has an `@extends` directive the shared extends pipeline
    /// (`resolve_extends_components`) builds the effective blocks and `scope`
    /// identically to text mode — then the dispatch is performed on the spliced
    /// regions (NOT `module.body`), so @message blocks inside base @block defaults are
    /// correctly detected (avoids PF-004 divergence, decision #8).
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
    /// - Standalone path: [`evaluate_with_map_seeded`] on `module.body`.
    /// - `@extends` path: [`Self::evaluate_extends_regions`] over
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

        // The entry holds its tree charge as a temporary until the module closes.
        let module = self.parse_charged(ctx, false)?;

        let raw_frontmatter = module.frontmatter.as_ref().map(|fm| fm.raw.as_str());

        // ── Extends branch ────────────────────────────────────────────────────
        if let Some(ext) = module.extends.clone() {
            let (frontmatter_values, frontmatter_bytes) =
                parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;
            let components = self.resolve_extends_components(
                &module,
                &ext,
                ctx,
                &frontmatter_values,
                frontmatter_bytes,
                warnings,
            )?;

            {
                let mut scope = components.scope.clone();
                Self::validate_extends_components(&components, &mut scope)?;
            }

            let ExtendsComponents {
                mut scope,
                merged_frontmatter,
                effective_skeleton,
                effective_blocks,
                skeleton_origin,
                ..
            } = components;

            // Per-region evaluation in both modes, with or without a source map:
            // base-skeleton nodes, base defaults and child overrides index into
            // different files, so each region is evaluated against its own origin
            // (#114, #115).
            let regions = spliced_regions(&effective_skeleton, &effective_blocks, &skeleton_origin);

            if regions.iter().any(|(nodes, _)| has_message_block(nodes)) {
                // AC-FUNC-07: source_map=true is incompatible with messages-mode templates.
                // The evaluator only has text-stream semantics; messages boundaries don't
                // have stable byte offsets relative to the source.  Degrade gracefully.
                if opts.source_map {
                    warnings.push(MSG_MODE_SOURCE_MAP_WARNING.to_string());
                }
                let messages = Self::evaluate_message_regions(
                    &regions,
                    &mut scope,
                    warnings,
                    self.budget.start_module(),
                )?;
                return Ok((
                    crate::CompiledOutput::Messages(
                        messages.into_iter().map(crate::Message::from).collect(),
                    ),
                    None, // source maps cover Markdown only (AC-FUNC-07)
                ));
            }

            // Seed the builder with the skeleton's root file (source maps only).
            let seed = opts.source_map.then_some(&skeleton_origin);
            let (body_raw, builder) = Self::evaluate_extends_regions(
                &regions,
                seed,
                &mut scope,
                warnings,
                self.budget.start_module(),
            )?;
            return self.finalize_markdown(
                body_raw,
                merged_frontmatter.as_deref(),
                builder.map(|b| (b, 0)),
                ctx.base_dir,
                opts,
                warnings,
            );
        }

        // ── Standalone path ───────────────────────────────────────────────────
        // The parsed tree is dropped once the scope layer is built from it.
        let (mut scope, fm_imports) = {
            let (mapping, spent) =
                parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;
            build_scope_from_frontmatter(
                module.frontmatter.as_ref(),
                mapping.as_ref(),
                spent,
                is_md,
                ctx.runtime_vars,
                &mut self.budget.mem,
            )?
        };

        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            explicit_exports,
            ..
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;

        validate_exports(&explicit_exports, &functions)?;

        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        if has_message_block(&module.body) {
            // AC-FUNC-07: source_map=true is incompatible with messages-mode templates.
            if opts.source_map {
                warnings.push(MSG_MODE_SOURCE_MAP_WARNING.to_string());
            }
            let mut messages = Vec::new();
            evaluate_messages_seeded(
                &module.body,
                &mut scope,
                warnings,
                ctx.file_str,
                ctx.source,
                self.budget.start_module(),
                &mut messages,
            )?;
            return Ok((
                crate::CompiledOutput::Messages(
                    messages.into_iter().map(crate::Message::from).collect(),
                ),
                None, // source maps cover Markdown only (AC-FUNC-07)
            ));
        }

        let mut body_raw = String::new();
        let builder = if opts.source_map {
            // The builder's seed (current source 0) is ctx.key / ctx.file_str / ctx.source;
            // evaluate_with_map derives display/source from builder (issue #58 / R3).
            let (origin, map_origin_bytes) = match ctx.origin.get() {
                Some(origin) => (origin.clone(), 0),
                // A map-only origin is part of optional map work. It is at most one
                // capped source copy; charge it with the live segments at finalize.
                None => (ctx.make_origin(), ctx.origin_bytes()),
            };
            let builder = MapBuilder::new(origin);
            let returned = evaluate_with_map_seeded(
                &module.body,
                &mut scope,
                warnings,
                builder,
                self.budget.start_module(),
                &mut body_raw,
            )?;
            Some((returned, map_origin_bytes))
        } else {
            evaluate_seeded(
                &module.body,
                &mut scope,
                warnings,
                ctx.file_str,
                ctx.source,
                self.budget.start_module(),
                &mut body_raw,
            )?;
            None
        };

        self.finalize_markdown(
            body_raw,
            raw_frontmatter,
            builder,
            ctx.base_dir,
            opts,
            warnings,
        )
    }

    fn finalize_markdown(
        &mut self,
        body_raw: String,
        frontmatter: Option<&str>,
        builder: Option<(MapBuilder, usize)>,
        base_dir: &str,
        opts: &CompileOptions,
        warnings: &mut Vec<String>,
    ) -> Result<(crate::CompiledOutput, Option<crate::SourceMap>), MdsError> {
        let mem = &mut self.budget.mem;
        let mark = mem.mark();
        if let Some(raw) = frontmatter {
            mem.charge(
                Part::Temp,
                bytes(raw.len(), 2).and_then(|n| n.checked_add(9)),
            )?;
        }
        let prefix = crate::frontmatter_prefix(frontmatter);
        let fm_prefix_len = prefix.as_ref().map_or(0, String::len);
        // Reserve the output's charge before optional map work can use headroom.
        // The empty body takes the prefix as-is, without a final newline reserve.
        let growth = fm_prefix_len + usize::from(!body_raw.trim_end().is_empty());
        mem.charge(Part::Buffers, Some(growth))?;
        let charged_capacity = body_raw.capacity() + growth;

        let pending_map = builder.and_then(|(mut builder, map_origin_bytes)| {
            let total_src_bytes = builder.sources_content_bytes();
            let label_base_bytes = opts
                .source_map_base
                .as_ref()
                .map_or(0, |base| base.as_os_str().len());
            let prefix_lines = prefix.as_deref().map_or(0, count_newlines);
            if !builder.segments_dropped
                && !charge_finalize(
                    mem,
                    &builder,
                    &body_raw,
                    FinalizeCosts {
                        content: if opts.include_sources_content
                            && total_src_bytes <= MAX_SOURCES_CONTENT_BYTES
                        {
                            total_src_bytes
                        } else {
                            0
                        },
                        map_origin_bytes,
                        prefix_lines,
                        label_base_bytes,
                    },
                )
            {
                builder.segments_dropped = true;
            }
            apply_map_degradation(builder, total_src_bytes, opts, warnings)
                .map(|b| b.finalize_points(&body_raw))
        });
        let mut body = crate::clean_output_owned(body_raw, fm_prefix_len);
        if let Some(prefix) = prefix {
            if body.is_empty() {
                body = prefix;
            } else {
                body.insert_str(0, &prefix);
            }
        }
        if body.capacity() > charged_capacity {
            mem.charge(Part::Buffers, Some(body.capacity() - charged_capacity))?;
        } else {
            mem.release_buffers(charged_capacity - body.capacity());
        }

        let source_map = pending_map.map(|map| map.finish(&body, fm_prefix_len, None));
        mem.release_to(mark);
        self.anchor_root_for_source_map(base_dir);
        // Relativize every source label before the caller receives the map.
        let source_map = source_map.map(|mut sm| {
            let root_str = self.fs.source_root();
            let root = root_str.as_deref().map(std::path::Path::new);
            let base = opts.source_map_base.as_deref();
            for src in &mut sm.sources {
                *src = crate::source_path::relativize_source(src, base, root);
            }
            sm
        });
        Ok((crate::CompiledOutput::Markdown(body), source_map))
    }

    // Call even on error. Only propagate the pop error if the module result is Ok.
    fn pop_resolving(&mut self, key: &str) -> Result<(), MdsError> {
        Self::check_lifo_pop(self.resolving.pop(), key)
    }

    fn check_lifo_pop(popped: Option<String>, expected: &str) -> Result<(), MdsError> {
        if popped.as_deref() == Some(expected) {
            Ok(())
        } else {
            Err(MdsError::syntax(format!(
                "internal error: resolving stack LIFO invariant violated \
                 (expected {expected}, got {got}) — this is a compiler bug, please report it",
                got = popped.as_deref().unwrap_or("<empty>"),
            )))
        }
    }

    // Call after a successful resolution. The thread check is defense in depth.
    fn finish_root(&self) -> Result<(), MdsError> {
        if self.resolving.is_empty()
            && (self.budget.mem.mark() != 0 || self.budget.mem.underflow)
            && !std::thread::panicking()
        {
            return Err(MdsError::internal());
        }
        Ok(())
    }

    /// Common module processing: tokenize, parse, build scope, evaluate.
    ///
    /// `ctx.file_str` is the display path for error messages (may be `SOURCE_LABEL`).
    /// `ctx.base_dir` is the directory used to resolve relative `@import`/`@extends`.
    /// `is_md` controls whether the `type` frontmatter key is treated as a file-type marker.
    /// `upgrade` is true when the module replaces a cached skeleton entry.
    fn process_module(
        &mut self,
        ctx: &ModuleCtx<'_>,
        is_md: bool,
        upgrade: bool,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        // The module entry keeps its tree. An upgrade of a cached skeleton keeps the
        // tree that the skeleton entry counts, so the new tree stays a temporary.
        let module = self.parse_charged(ctx, !upgrade)?;

        // Parse frontmatter YAML once for both scope building and storage. The
        // module entry keeps the tree.
        let (frontmatter_values, frontmatter_bytes) =
            parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;
        self.keep(frontmatter_bytes);

        // Branch: child template (@extends) vs. standalone module.
        if let Some(ext) = module.extends.clone() {
            return self.process_module_extends(
                module,
                ext,
                ctx,
                frontmatter_values,
                frontmatter_bytes,
                warnings,
            );
        }

        // ── Standalone (non-extending) path ──────────────────────────────────
        let raw_frontmatter = self.copy_raw_frontmatter(&module)?;

        // Build scope from frontmatter + runtime vars; extract any frontmatter imports.
        let (mut scope, fm_imports) = build_scope_from_frontmatter(
            module.frontmatter.as_ref(),
            frontmatter_values.as_ref(),
            frontmatter_bytes,
            is_md,
            ctx.runtime_vars,
            &mut self.budget.mem,
        )?;

        // Resolve frontmatter imports BEFORE body imports (per spec: frontmatter imports
        // resolve before body imports; a duplicate alias is a compile error — legacy
        // decision 014 in 88ddbcc~1:.devflow/decisions/decisions.md).
        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        // Walk the AST: collect @define functions (with closure capture), process imports/exports
        let CollectedDefs {
            functions,
            has_explicit_exports,
            explicit_exports,
            block_names,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;
        // A function can capture the scope layer, so the entry keeps the layer too.
        if !functions.is_empty() {
            self.keep(frontmatter_bytes);
        }

        // Validate that all named exports refer to defined functions or "prompt"
        validate_exports(&explicit_exports, &functions)?;

        // Validate semantic correctness before evaluation
        validator::validate(&module.body, &mut scope, ctx.file_str, ctx.source)?;

        // Whether "prompt" is an available export of this module (the rule
        // `ResolvedModule::is_exported` applies).
        let prompt_exported = is_visible_export(has_explicit_exports, &explicit_exports, "prompt");

        let origin = ctx.origin(self)?;

        // Evaluate the body to get prompt text (and optionally a FragmentMap).
        //
        // When source-map mode is active and the module exports "prompt", run a
        // single evaluate_with_map pass to collect both the text and its segment records.
        // The module and its namespace share the fragment map for @include attribution.
        // Evaluate the body even if it is not exported, to distinguish the two include warnings.
        let mut body_raw = String::new();
        let builder = if self.source_map_mode && prompt_exported {
            let builder = crate::sourcemap::MapBuilder::new(origin.clone());
            // evaluate_with_map derives file/source from the builder's current source
            // (issue #58).
            Some(evaluate_with_map_seeded(
                &module.body,
                &mut scope,
                warnings,
                builder,
                self.budget.start_module(),
                &mut body_raw,
            )?)
        } else {
            evaluate_seeded(
                &module.body,
                &mut scope,
                warnings,
                ctx.file_str,
                ctx.source,
                self.budget.start_module(),
                &mut body_raw,
            )?;
            None
        };
        let (prompt_body, prompt_map) =
            self.prompt_body_and_map(body_raw, builder, ctx.file_str, warnings);

        // Build effective_blocks first so module.body can be moved into the Arc below.
        let effective_blocks = seed_effective_blocks(&module.body, &block_names, origin);

        // Move module.body into Arc<[Node]> (reuses the Vec allocation, no element clones, P1).
        let effective_skeleton: Arc<[Node]> = Arc::from(module.body);

        let namespace = ResolvedModule::build_namespace(
            &functions,
            has_explicit_exports,
            &explicit_exports,
            prompt_body.as_ref(),
            prompt_map.as_ref(),
        );
        Ok(ResolvedModule {
            functions,
            prompt_body,
            namespace,
            prompt_map,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin: origin.clone(),
            frontmatter_values,
            frontmatter_bytes,
            is_skeleton: false,
        })
    }

    /// Build the merged scope for an `@extends` child (step 3d of `resolve_extends_components`).
    ///
    /// Covers steps 3d-i through 3d-vi:
    /// - Parse FM imports from base and child frontmatter (3d-i).
    /// - Deep-merge base and child FM mappings (3d-ii).
    /// - Build scope from the merged mapping with runtime vars (3d-iii).
    /// - Resolve base FM imports against the base file (3d-iv, frontmatter-first ordering).
    /// - Resolve child FM imports against the child file (3d-v).
    /// - Merge base functions into scope (3d-vi).
    ///
    /// # Borrow note
    /// `base` is taken as an explicit `&ResolvedModule` (not via `self`) so that the
    /// borrow of `base.skeleton_origin.source` (needed for `base_ctx.source`) is
    /// independent of `&mut self`. The caller passes `&*arc` to deref from `Arc`.
    ///
    /// # Invariants preserved
    /// - Base FM imports resolved BEFORE child FM imports (frontmatter-first ordering).
    /// - `deep_merge_yaml` applies `MAX_FRONTMATTER_MERGE_DEPTH` cap.
    /// - `resolve_frontmatter_imports` → `resolve_import_from` → `resolve_by_key_skeleton`
    ///   preserves PF-004 safety (cycle detection, `check_import_depth`, file-size cap).
    fn build_merged_extends_scope(
        &mut self,
        base: &ResolvedModule,
        child_fm: &Option<serde_yaml_ng::Mapping>,
        child_fm_bytes: usize,
        base_key: &str,
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<(Scope, serde_yaml_ng::Mapping, usize), MdsError> {
        // 3d-i: Extract frontmatter imports from BOTH base and child BEFORE the deep merge.
        //
        // Base imports resolve relative to the BASE file's directory (base_base_dir, derived
        // from base_key via FileSystem::parent_dir). Child imports resolve relative to the
        // CHILD file's directory (ctx.base_dir). Both sets are resolved; a duplicate alias
        // across base+child → mds::name_collision (a duplicate alias across frontmatter
        // and body is a compile error).
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
        // Reserved keys (imports, type, extends) are excluded by deep_merge_yaml.
        // Precedence: base < child (child overrides base on collision).
        // The merged mapping is a temporary.
        let (merged_mapping, charge) = self.merge_frontmatter(base, child_fm, child_fm_bytes)?;

        // 3d-iii: Build scope from the merged mapping. Runtime vars applied LAST
        // (base < child < runtime, F7, decision #3). The layer is charged with the
        // merge charge as a temporary first. It stays a temporary: an extending child
        // has only @block overrides, so no function of its own can capture the layer.
        self.budget.mem.charge(Part::Temp, Some(charge))?;
        let mut scope = build_scope_from_merged_mapping(&merged_mapping, ctx.runtime_vars)?;

        // 3d-iv: Resolve base frontmatter imports against base_key (frontmatter-first ordering,
        // PF-004 safe via resolve_frontmatter_imports → resolve_import_from).
        // Use a ctx pointing to the base file with its REAL source bytes so that any
        // span-carrying error here attributes correctly AND the at() debug_assert can't
        // false-fire. The skeleton_origin already carries these bytes; we use them here
        // for consistency.
        let base_source_ref: &str = &base.skeleton_origin.source;
        let base_base_dir = self.fs.parent_dir(base_key);
        let base_display_owned = crate::source_path::display_path_for(&*self.fs, base_key);
        let base_ctx = ModuleCtx {
            key: base_key,
            file_str: &base_display_owned,
            source: base_source_ref,
            base_dir: &base_base_dir,
            runtime_vars: ctx.runtime_vars,
            origin: OnceCell::new(),
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

        Ok((scope, merged_mapping, charge))
    }

    /// Deep-merge the frontmatter tree of `base` with the child's tree. Every merge
    /// in the resolver goes through here. The bytes of both trees are charged as a
    /// temporary before the merge: they bound the merged tree, whose sub-mappings
    /// are part of the result. Returns the merged tree and that charge.
    fn merge_frontmatter(
        &mut self,
        base: &ResolvedModule,
        child: &Option<serde_yaml_ng::Mapping>,
        child_bytes: usize,
    ) -> Result<(serde_yaml_ng::Mapping, usize), MdsError> {
        let charge = self
            .budget
            .mem
            .charge(Part::Temp, base.frontmatter_bytes.checked_add(child_bytes))?;
        // An empty mapping stands in for a side with no frontmatter.
        let empty = serde_yaml_ng::Mapping::new();
        let merged = deep_merge_yaml(
            base.frontmatter_values.as_ref().unwrap_or(&empty),
            child.as_ref().unwrap_or(&empty),
            0,
        )?;
        Ok((merged, charge))
    }

    /// Serialize a deep-merged frontmatter `Mapping` for `frontmatter_prefix`, by
    /// reference.
    ///
    /// Return `Ok(None)` for an empty mapping. Otherwise return the canonical YAML
    /// representation with a final newline and no fences.
    ///
    /// `charge` is the merge charge of the mapping. Four text bytes per charged byte,
    /// plus one for the newline, bound the text: an escaped control byte takes four.
    /// The bound is charged as a temporary before the text is allocated. A text past
    /// the bound grows once, and that growth is charged after it. The entry keeps the
    /// text, so its charge moves to the kept bytes at the text capacity.
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
        &mut self,
        mapping: &serde_yaml_ng::Mapping,
        charge: usize,
    ) -> Result<Option<String>, MdsError> {
        if mapping.is_empty() {
            return Ok(None);
        }
        let bound = self
            .budget
            .mem
            .charge(Part::Temp, bytes(charge, 4).and_then(|n| n.checked_add(1)))?;
        let mut yaml = Vec::with_capacity(bound);
        serde_yaml_ng::to_writer(&mut yaml, mapping)
            .map_err(|e| MdsError::yaml_error(e.to_string()))?;
        // The serializer writes UTF-8 only.
        let mut text = String::from_utf8(yaml).map_err(|_| MdsError::internal())?;
        text.truncate(text.trim_end().len());
        if text.is_empty() {
            return Ok(None);
        }
        text.push('\n');
        self.budget
            .mem
            .charge(Part::Temp, text.capacity().checked_sub(bound))?;
        self.keep(text.capacity());
        Ok(Some(text))
    }

    /// Shared extends-pipeline: steps 3a-3d are identical for the text-cached and
    /// intrinsic paths.
    ///
    /// Builds the effective skeleton and blocks (base skeleton plus the winning block
    /// overrides, each with its `Origin`) and the `scope` (deep-merged frontmatter + FM
    /// imports + functions) needed by both `process_module_extends` (cached text path)
    /// and `process_module_intrinsic`.
    ///
    /// Callers differ only in the terminal step (step 3e), which walks the skeleton's
    /// `spliced_regions` and evaluates each against its own origin:
    /// - Cached text path: `validate` → `evaluate_extends_regions` (+ a FragmentMap)
    /// - Intrinsic path:   `has_message_block` dispatch → `evaluate_message_regions`
    ///   (Messages) or `evaluate_extends_regions` + clean/frontmatter (Markdown)
    ///
    /// Factoring here enforces that BOTH modes go through the same PF-004-safe
    /// `resolve_by_key_skeleton` path for the base, and share one copy of the
    /// scope-construction pipeline (re-validated at the leaf even though every part
    /// passed its parse-time check; decision #3/7).
    fn resolve_extends_components(
        &mut self,
        module: &crate::ast::Module,
        ext: &crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        frontmatter_values: &Option<serde_yaml_ng::Mapping>,
        frontmatter_bytes: usize,
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
        let base_ref = ModuleRef {
            key: &base_key,
            typed: &ext.path,
        };
        let base = self
            .resolve_by_key_skeleton(
                base_ref,
                ctx.runtime_vars,
                ctx.import_site(ext.offset),
                warnings,
            )
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;

        // ── Step 3b: child-only-blocks check ─────────────────────────────────
        // Every top-level node in module.body must be Node::Block or whitespace-only Text.
        // (Frontmatter and @extends are already split out of module.body by the parser.)
        check_child_only_blocks(&module.body, ctx)?;

        // ── Step 3c: build effective_blocks from base, applying child overrides ──
        // Clone base's map first (diamond-inheritance safe — never mutate cached base).
        let effective_blocks =
            apply_block_overrides(&base.effective_blocks, &module.body, ctx, self)?;

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
        let (mut scope, merged_mapping, charge) = self.build_merged_extends_scope(
            &base,
            frontmatter_values,
            frontmatter_bytes,
            &base_key,
            ctx,
            warnings,
        )?;
        let merged_frontmatter = self.serialize_merged_frontmatter(&merged_mapping, charge)?;

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

        Ok(ExtendsComponents {
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
    /// OutOfBounds diagnostic bug), under the region's display path — never the canonical
    /// `origin.file`, which is absolute for `NativeFs` (R3 / CWE-209, #114). The scope is
    /// threaded through all regions so `@define` / `@for` scope push/pop behaves
    /// identically to a whole-slice validate.
    ///
    /// This single helper is called by BOTH `process_module_extends` (cached text path)
    /// and `process_module_intrinsic` (@extends branch) — enforcing PF-004 parity: the two
    /// parallel paths can never drift because they share one implementation.
    ///
    /// Re-validate at the leaf (on the spliced regions), not at intermediate bases.
    fn validate_extends_components(
        components: &ExtendsComponents,
        scope: &mut Scope,
    ) -> Result<(), MdsError> {
        for (nodes, origin) in spliced_regions(
            &components.effective_skeleton,
            &components.effective_blocks,
            &components.skeleton_origin,
        ) {
            validator::validate(nodes, scope, &origin.display, &origin.source)?;
        }
        Ok(())
    }

    /// Evaluate an extending child template in text mode.
    ///
    /// Delegates the shared pipeline (steps 3a-3d) to `resolve_extends_components`,
    /// then runs `validate_extends_components` + a region-by-region evaluation (step 3e),
    /// which in source-map mode also yields the module's FragmentMap (#412).
    ///
    /// Decision #2: base is NEVER validated/evaluated standalone — deferred to leaf.
    /// PF-004: base is read via resolve_by_key_skeleton (FileSystem trait, never std::fs).
    fn process_module_extends(
        &mut self,
        module: crate::ast::Module,
        ext: crate::ast::ExtendsDirective,
        ctx: &ModuleCtx<'_>,
        frontmatter_values: Option<serde_yaml_ng::Mapping>,
        frontmatter_bytes: usize,
        warnings: &mut Vec<String>,
    ) -> Result<ResolvedModule, MdsError> {
        let components = self.resolve_extends_components(
            &module,
            &ext,
            ctx,
            &frontmatter_values,
            frontmatter_bytes,
            warnings,
        )?;

        // ── Step 3e: validate + evaluate the spliced regions ──────────────────
        // Validate per-region so each region's offsets are checked against the correct
        // source (fixes the cross-source OutOfBounds diagnostic bug). This is what makes
        // E12 work: a base default block referencing an undefined var is caught HERE
        // against the merged leaf scope. (dynamically-assembled content is re-validated
        // even though each part was checked at parse time.)
        {
            let mut scope = components.scope.clone();
            Self::validate_extends_components(&components, &mut scope)?;
        }

        let ExtendsComponents {
            mut scope,
            functions,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            has_explicit_exports,
            explicit_exports,
            merged_frontmatter,
        } = components;

        // Base-skeleton nodes, base defaults and child overrides index into different
        // files, so each region is evaluated against its own origin (#114). In
        // source-map mode the regions are recorded into a builder seeded with the
        // chain's root, as for a direct compile of this file, and kept as the
        // FragmentMap an importer's `@include` splices (#412) — under the same gate as
        // a standalone module. The body is evaluated either way.
        let prompt_exported = is_visible_export(has_explicit_exports, &explicit_exports, "prompt");
        let regions = spliced_regions(&effective_skeleton, &effective_blocks, &skeleton_origin);
        let seed = (self.source_map_mode && prompt_exported).then_some(&skeleton_origin);
        let (body_raw, builder) = Self::evaluate_extends_regions(
            &regions,
            seed,
            &mut scope,
            warnings,
            self.budget.start_module(),
        )?;
        let (prompt_body, prompt_map) =
            self.prompt_body_and_map(body_raw, builder, ctx.file_str, warnings);

        let namespace = ResolvedModule::build_namespace(
            &functions,
            has_explicit_exports,
            &explicit_exports,
            prompt_body.as_ref(),
            prompt_map.as_ref(),
        );
        Ok(ResolvedModule {
            functions,
            prompt_body,
            namespace,
            prompt_map,
            // #154: emit deep-merged frontmatter (base < child, reserved keys excluded)
            // rather than the child's raw frontmatter.
            raw_frontmatter: merged_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
            frontmatter_bytes,
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
        module: ModuleRef<'_>,
        runtime_vars: &Arc<VarLayer>,
        import_site: ImportSite<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Arc<ResolvedModule>, MdsError> {
        let key = module.key;
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

        // Module count guard (REL-2, applies PF-004).
        self.check_module_count(key)?;

        // PF-004: read via FileSystem trait — NEVER std::fs.
        let source = self.fs.read(key)?;
        let is_md = self.fs.is_markdown(key);
        check_module_type(module, &source).map_err(|err| import_site.attach_type_error(err))?;

        let (mark, read) = self.begin_module(key, runtime_vars, source.capacity());
        let resolved = read.and_then(|()| {
            let base_dir_owned = self.fs.parent_dir(key);
            let display_owned = crate::source_path::display_path_for(&*self.fs, key);
            let ctx = ModuleCtx {
                key,
                file_str: &display_owned,
                source: &source,
                base_dir: &base_dir_owned,
                runtime_vars,
                origin: OnceCell::new(),
            };
            self.process_module_skeleton(&ctx, is_md, warnings)
        });
        #[cfg(test)]
        let probe = mark.probe;
        let kept = self.close_module(mark);

        let popped = self.pop_resolving(key);
        let resolved = resolved?;
        popped?;

        let arc = Arc::new(resolved);
        self.retain_module(key, &arc, kept);
        #[cfg(test)]
        {
            self.module_probes[probe].buffers_after_insert = Some(self.budget.mem.buffers);
        }
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
    /// - `skeleton_origin` Arc is cloned from the grandparent (origin rides along the
    ///   data — never a path→source lookup; legacy decision 022 in the same
    ///   git-history file).
    #[allow(clippy::type_complexity)]
    fn resolve_intermediate_base(
        &mut self,
        ext: &crate::ast::ExtendsDirective,
        own_fm: &Option<serde_yaml_ng::Mapping>,
        own_fm_bytes: usize,
        module_body: &[Node],
        ctx: &ModuleCtx<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<
        (
            Arc<[Node]>,
            IndexMap<String, EffectiveBlock>,
            Origin,
            Option<serde_yaml_ng::Mapping>,
            usize,
        ),
        MdsError,
    > {
        validate_import_path(&ext.path)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;
        let grandparent_key = self
            .fs
            .normalize_in_dir(ctx.base_dir, &ext.path)
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;
        let grandparent_ref = ModuleRef {
            key: &grandparent_key,
            typed: &ext.path,
        };
        let grandparent = self
            .resolve_by_key_skeleton(
                grandparent_ref,
                ctx.runtime_vars,
                ctx.import_site(ext.offset),
                warnings,
            )
            .map_err(|e| attach_import_span(e, &ext.path, ctx.file_str, ctx.source, ext.offset))?;

        // Child-only-blocks check for this intermediate base (3b).
        check_child_only_blocks(module_body, ctx)?;

        let eff_blocks =
            apply_block_overrides(&grandparent.effective_blocks, module_body, ctx, self)?;

        // skeleton_origin Arc::clone'd from grandparent — the root base's source bytes
        // ride down the chain so the leaf's validate_extends_components can attribute
        // non-block skeleton node diagnostics to the root file (Risk #2; origin rides
        // along via Arc, never reconstructed from a path cache).
        let skel_origin = grandparent.skeleton_origin.clone();

        // Phase 3: transitive FM merge: grandparent.frontmatter_values < own_fm_values.
        // This produces the accumulated FM for this intermediate base, so a leaf
        // descending from it gets the full transitive chain without re-traversing.
        // The entry keeps the merged tree with its merge charge; the own tree stays
        // a temporary.
        let (merged_fm, charge) = self.merge_frontmatter(&grandparent, own_fm, own_fm_bytes)?;
        let (accumulated_fm, accumulated_bytes) = if merged_fm.is_empty() {
            (None, 0)
        } else {
            (Some(merged_fm), charge)
        };

        Ok((
            Arc::clone(&grandparent.effective_skeleton),
            eff_blocks,
            skel_origin,
            accumulated_fm,
            accumulated_bytes,
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
        // The skeleton entry keeps its tree.
        let module = self.parse_charged(ctx, true)?;

        let raw_frontmatter = self.copy_raw_frontmatter(&module)?;
        // own_fm_values: this module's raw frontmatter (not yet merged with ancestors).
        let (own_fm_values, own_fm_bytes) =
            parse_frontmatter_mapping(module.frontmatter.as_ref(), &mut self.budget.mem)?;

        // Build scope for @define closure capture (base functions must be available).
        let (mut scope, fm_imports) = build_scope_from_frontmatter(
            module.frontmatter.as_ref(),
            own_fm_values.as_ref(),
            own_fm_bytes,
            is_md,
            ctx.runtime_vars,
            &mut self.budget.mem,
        )?;
        self.resolve_frontmatter_imports(&fm_imports, &mut scope, ctx, warnings)?;

        let CollectedDefs {
            functions,
            has_explicit_exports,
            explicit_exports,
            block_names,
        } = self.collect_definitions_and_imports(&module.body, &mut scope, ctx, warnings)?;
        // A function can capture the scope layer, so the entry keeps the layer too.
        if !functions.is_empty() {
            self.keep(own_fm_bytes);
        }

        // Multi-level chain (A←B←C): B may itself extend A.
        // B's effective_skeleton = A's effective_skeleton (Arc::clone, O(1) fold).
        // B's effective_blocks = clone(A.effective_blocks) + B's overrides (most-derived wins, F3).
        // B's skeleton_origin = A's skeleton_origin (Arc::clone — root source rides down the chain).
        //
        // Phase 3: B's frontmatter_values must be the transitive deep-merge of A's accumulated FM
        // with B's own FM (A < B), so that when C later merges against B, it gets A+B+C.
        let (
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
            frontmatter_bytes,
        ) = if let Some(ext) = module.extends.as_ref() {
            self.resolve_intermediate_base(
                ext,
                &own_fm_values,
                own_fm_bytes,
                &module.body,
                ctx,
                warnings,
            )?
        } else {
            // Root base: own body is the skeleton; blocks seeded from own @block declarations.
            let root_origin = ctx.origin(self)?.clone();
            // Seed blocks first so module.body can be moved into the Arc (no element clones, P1).
            let eff_blocks = seed_effective_blocks(&module.body, &block_names, &root_origin);
            let eff_skeleton: Arc<[Node]> = Arc::from(module.body);
            // Root base: frontmatter_values is its own raw FM (no ancestors).
            (
                eff_skeleton,
                eff_blocks,
                root_origin,
                own_fm_values,
                own_fm_bytes,
            )
        };
        // The entry keeps its frontmatter tree.
        self.keep(frontmatter_bytes);

        // Skeleton entries have no evaluated text or fragment map.
        let prompt_body: Option<PromptText> = None;
        debug_assert!(
            prompt_body.is_none(),
            "A1 invariant: skeleton entry (is_skeleton=true) must have prompt_body=None"
        );
        let namespace = ResolvedModule::build_namespace(
            &functions,
            has_explicit_exports,
            &explicit_exports,
            prompt_body.as_ref(),
            None,
        );
        Ok(ResolvedModule {
            functions,
            namespace,
            prompt_body,
            prompt_map: None,
            raw_frontmatter,
            has_explicit_exports,
            explicit_exports,
            effective_skeleton,
            effective_blocks,
            skeleton_origin,
            frontmatter_values,
            frontmatter_bytes,
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
        let mut define_count: usize = 0;
        for node in body {
            count_module_defines(node, &mut define_count)?;
            match node {
                Node::Define(def) => collect_define(def, &mut defs, scope, ctx, self)?,
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
                offset,
            } => {
                // Resolve the source module and bring in the function for
                // re-export only. Per spec: "@export from does not bring the
                // symbol into the current file's scope".
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self
                    .resolve_import_from(
                        ctx.base_dir,
                        import_path,
                        ctx.runtime_vars,
                        Some(ctx.import_site(*offset)),
                        warnings,
                    )
                    .map_err(|e| {
                        attach_import_span(e, import_path, ctx.file_str, ctx.source, *offset)
                    })?;
                let func = source_module.get_export(name).ok_or_else(|| {
                    MdsError::export_error(format!(
                        "cannot re-export '{name}': not exported from \"{import_path}\""
                    ))
                })?;
                defs.functions.insert(name.clone(), func);
                defs.explicit_exports.insert(name.clone());
            }
            ExportDirective::Wildcard {
                path: import_path,
                offset,
            } => {
                // Re-export all exports from the target module. These are
                // available to importers but NOT in the current file's scope.
                // Note: resolve_import_from calls validate_import_path internally,
                // so path validation errors surface with correct messages automatically.
                let source_module = self
                    .resolve_import_from(
                        ctx.base_dir,
                        import_path,
                        ctx.runtime_vars,
                        Some(ctx.import_site(*offset)),
                        warnings,
                    )
                    .map_err(|e| {
                        attach_import_span(e, import_path, ctx.file_str, ctx.source, *offset)
                    })?;
                let line_len = line_len_at(ctx.source, *offset);
                for (name, func) in source_module.get_all_exports() {
                    if defs.functions.contains_key(&name) {
                        return Err(MdsError::name_collision_at(
                            &name,
                            ctx.file_str,
                            ctx.source,
                            *offset,
                            line_len,
                        ));
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
            return Err(MdsError::name_collision_at(
                alias,
                ctx.file_str,
                ctx.source,
                offset,
                alias.len(),
            ));
        }
        let resolved = self
            .resolve_import_from(
                ctx.base_dir,
                path,
                ctx.runtime_vars,
                Some(ctx.import_site(offset)),
                warnings,
            )
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        scope.set_namespace(alias, resolved.shared_namespace());
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
                        .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, None, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    scope.set_namespace(alias, resolved.shared_namespace());
                }
                FrontmatterImport::Merge { path } => {
                    let resolved = self
                        .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, None, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    for (name, func) in resolved.get_all_exports() {
                        if scope.get_function(&name).is_some() {
                            return Err(MdsError::name_collision(name));
                        }
                        scope.set_function(&name, func);
                    }
                    if let Some(val) = resolved.shared_prompt_value() {
                        scope.set_var_shared("prompt", val);
                    }
                }
                FrontmatterImport::Selective { path, names } => {
                    let resolved = self
                        .resolve_import_from(ctx.base_dir, path, ctx.runtime_vars, None, warnings)
                        .map_err(|e| attach_frontmatter_index(e, i))?;
                    let not_exported = |name: &str| {
                        MdsError::import_error(format!(
                            "'{name}' is not exported from '{path}' (in frontmatter imports[{i}])"
                        ))
                    };
                    for name in names {
                        if name == "prompt" {
                            scope.set_var_shared(
                                "prompt",
                                resolved
                                    .shared_prompt_value()
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
            .resolve_import_from(
                ctx.base_dir,
                path,
                ctx.runtime_vars,
                Some(ctx.import_site(offset)),
                warnings,
            )
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        // Per spec: only functions and the prompt body are imported via merge.
        // Frontmatter variables from the imported module are NOT brought into scope.
        let line_len = line_len_at(ctx.source, offset);
        for (name, func) in resolved.get_all_exports() {
            if scope.get_function(&name).is_some() {
                return Err(MdsError::name_collision_at(
                    &name,
                    ctx.file_str,
                    ctx.source,
                    offset,
                    line_len,
                ));
            }
            scope.set_function(&name, func);
        }
        if let Some(val) = resolved.shared_prompt_value() {
            scope.set_var_shared("prompt", val);
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
            .resolve_import_from(
                ctx.base_dir,
                path,
                ctx.runtime_vars,
                Some(ctx.import_site(offset)),
                warnings,
            )
            .map_err(|e| attach_import_span(e, path, ctx.file_str, ctx.source, offset))?;
        let line_len = line_len_at(ctx.source, offset);
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
                scope.set_var_shared(
                    "prompt",
                    resolved
                        .shared_prompt_value()
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
                ..
            } => self.resolve_selective_import(names, path, *offset, scope, ctx, warnings),
        }
    }
}

impl Default for ModuleCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleCache {
    fn prompt_body_and_map(
        &mut self,
        body_raw: String,
        builder: Option<MapBuilder>,
        display: &str,
        warnings: &mut Vec<String>,
    ) -> (
        Option<PromptText>,
        Option<Arc<crate::sourcemap::FragmentMap>>,
    ) {
        let capacity = body_raw.capacity();
        let prompt_body = if body_raw.trim().is_empty() {
            self.budget
                .mem
                .transfer(Part::Buffers, Part::Temp, capacity);
            None
        } else {
            // Evaluation charged this capacity before its allocation.
            self.kept += capacity;
            Some(PromptText::from(body_raw))
        };
        let prompt_map = builder.and_then(|mut builder| {
            if prompt_body.is_some()
                && !builder.segments_dropped
                && self.charge_kept(builder.segments_bytes()).is_err()
            {
                builder.segments_dropped = true;
            }
            fragment_map_from(builder, prompt_body.as_deref(), display, warnings)
        });
        (prompt_body, prompt_map)
    }
}

struct FinalizeCosts {
    content: usize,
    map_origin_bytes: usize,
    prefix_lines: usize,
    label_base_bytes: usize,
}

fn count_newlines(text: &str) -> usize {
    text.bytes().filter(|&byte| byte == b'\n').count()
}

fn charge_finalize(
    mem: &mut MemBudget,
    builder: &MapBuilder,
    body: &str,
    costs: FinalizeCosts,
) -> bool {
    let n = builder.segment_count() + count_newlines(body);
    let points = bytes(n, 36);
    // Points (16 B), resolved points (20 B), and the sampled CR table.
    let work = points
        .and_then(|n| n.checked_add(4 * (body.len() / 64 + 1)))
        .and_then(|n| n.checked_add(builder.segments_bytes()))
        .and_then(|n| n.checked_add(costs.map_origin_bytes));
    // A frontmatter line can also add a semicolon before the first point.
    let output = points
        .and_then(|n| n.checked_add(bytes(costs.prefix_lines, 2)?))
        .and_then(|n| n.checked_add(costs.content))
        .and_then(|n| n.checked_add(builder.source_labels_bytes(costs.label_base_bytes)?));
    let mark = mem.mark();
    if mem.charge(Part::Temp, work).is_err() {
        return false;
    }
    if mem.charge(Part::Buffers, output).is_err() {
        mem.release_to(mark);
        return false;
    }
    true
}

/// Apply AC-PERF-03 and AC-SEC-04 degradation checks to a completed builder.
///
/// - AC-PERF-03: if the segment cap was hit, degrade to `None` (a partial map
///   is misleading; warn and discard the builder).
/// - AC-SEC-04: if `sourcesContent` total bytes exceed the ceiling, set
///   `no_sources_content` so [`crate::sourcemap::PendingMap::finish`] omits the array.
/// - Caller opt-out: `!opts.include_sources_content` also sets the flag.
fn apply_map_degradation(
    mut builder: MapBuilder,
    total_src_bytes: usize,
    opts: &CompileOptions,
    warnings: &mut Vec<String>,
) -> Option<crate::sourcemap::MapBuilder> {
    if builder.segments_dropped {
        warnings.push(format!(
            "source map segment cap ({} segments) exceeded; \
             no source map will be generated",
            crate::limits::MAX_SOURCEMAP_SEGMENTS,
        ));
        return None;
    }
    if total_src_bytes > MAX_SOURCES_CONTENT_BYTES {
        warnings.push(format!(
            "sourcesContent total size ({} bytes) exceeds the {} byte ceiling; \
             sourcesContent will be omitted from the source map",
            total_src_bytes,
            crate::limits::MAX_SOURCES_CONTENT_BYTES,
        ));
        builder.no_sources_content = true;
    }
    if !opts.include_sources_content {
        builder.no_sources_content = true;
    }
    Some(builder)
}

/// Attach the importing directive's span to a spanless file, module, or cycle error.
/// Keep an existing span so errors inside another module retain their own source.
fn attach_import_span(
    err: MdsError,
    path: &str,
    file_str: &str,
    source: &str,
    offset: usize,
) -> MdsError {
    // Compute the span length as the number of bytes from `offset` to the
    // end of the `@import` line (not including the newline character itself),
    // so the whole directive is underlined. A non-boundary or out-of-range
    // offset can only come from a defect in offset attribution, never from a
    // template; `line_len_at` degrades it to a zero-length span instead of
    // slicing (#220) — `MdsError::at` then keeps the numeric offset and drops
    // the snippet rather than mis-attributing.
    let line_len = line_len_at(source, offset);
    match err {
        MdsError::FileNotFound { span: None, .. } => {
            MdsError::file_not_found_at(path, file_str, source, offset, line_len)
        }
        MdsError::ModuleNotFound {
            key, span: None, ..
        } => MdsError::module_not_found_at(key, file_str, source, offset, line_len),
        MdsError::CircularImport {
            cycle, span: None, ..
        } => MdsError::circular_import_at(cycle, file_str, source, offset, line_len),
        other => other,
    }
}

/// Returns the byte count from `offset` to just before the next `\n` (or
/// to the end of the string when there is no newline). Returns `0` when
/// `offset` is out of bounds or falls on a non-UTF-8 char boundary so callers
/// degrade gracefully rather than panic (ADR-005 — degrade rather than
/// mis-attribute; consistent with the `is_char_boundary` guard in
/// `build_type_mismatch` in evaluator.rs).
pub(crate) fn line_len_at(source: &str, offset: usize) -> usize {
    if source.is_char_boundary(offset) {
        source[offset..]
            .find('\n')
            .unwrap_or(source[offset..].len())
    } else {
        0
    }
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
mod tests;
