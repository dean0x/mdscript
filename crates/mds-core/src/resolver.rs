//! Module resolution and cache state.
//!
//! | Module or items | Responsibility |
//! |---|---|
//! | `charges` | Module budget charges and limits |
//! | `context` | Per-module context and results of module walks |
//! | `extends` | Inheritance resolution, merged frontmatter, and skeleton modules |
//! | `frontmatter` | Frontmatter parse and merge helpers |
//! | `import_paths` | Path checks, source-map anchors, and import error and frontmatter helpers |
//! | `imports` | Definition and import walks, with export collection |
//! | `inheritance` | Effective blocks and region layout |
//! | `module` | Resolved module data, exports, and module type checks |
//! | `module_scope` | Definition collection, scope construction, and export validation |
//! | `process` | Module processing, output evaluation, and source-map finalization |
//! | `ModuleCache`, its fields, `fmt`, `default`, `new`, `native`, `virtual_fs`, `with_fs`, and `dependencies` | The root owns the cache state and its transition funnel |
//! | `ModuleMark` and `ModuleProbe` | Budget state and test data for cache transitions |
//! | `MAX_IMPORT_DEPTH`, `SOURCE_LABEL`, `tree_bytes`, `tree_estimate`, and `charge_scan_tree` | Shared limits, source identity, and tree charges |
//! | `commit_retained`, `pop_resolving`, `check_lifo_pop`, and `finish_root` | The cache transition funnel |
//! | `resolve_path`, `resolve_path_intrinsic`, `resolve_path_intrinsic_source`, `resolve_path_intrinsic_opts`, and `resolve_by_key` | Path entry to the cache transition funnel |
//! | `resolve_key`, `resolve_virtual_intrinsic`, `resolve_virtual_intrinsic_opts`, `resolve_intrinsic_by_key`, `resolve_intrinsic_by_key_opts`, and `resolve_intrinsic_by_key_opts_source` | Key and virtual file entry to the cache transition funnel |
//! | `resolve_source`, `resolve_source_intrinsic`, and `resolve_source_intrinsic_opts` | String source entry to the cache transition funnel |
//! | `evaluate_regions_with_map` and `merge_frontmatter` | The output and merge funnels |
//! | `attach_import_span` and `line_len_at` | Shared diagnostic spans |
//! | `tests` | Tests for the entire resolver family |
//!
//! Children use `use super::{...}`. This module is the import hub.

mod charges;
mod context;
mod extends;
mod frontmatter;
mod import_paths;
mod imports;
mod inheritance;
mod module;
mod module_scope;
mod process;

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
// The defining module supplies its canonical key, display path, and source text for source maps (S7/S8).
// Keep Origin in sourcemap.rs so scope does not depend on the resolver.
pub(crate) use crate::sourcemap::Origin;
use crate::sourcemap::{CompileOptions, MapBuilder, SourceIndex};
use crate::validator;
use crate::value::Value;

use context::{
    fragment_map_from, is_visible_export, CollectedDefs, ExtendsComponents, ImportSite, ModuleCtx,
};
use frontmatter::{deep_merge_yaml, merged_frontmatter_layer};
pub(crate) use frontmatter::{
    parse_data_json_with, parse_data_yaml_with, NODE_YAML, RESERVED_MERGE_KEYS,
};
pub(crate) use frontmatter::{
    parse_frontmatter_imports_from_yaml, parse_frontmatter_yaml, parse_frontmatter_yaml_with,
    Allowance, FrontmatterImport,
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
#[cfg(test)]
use process::{apply_map_degradation, charge_finalize, FinalizeCosts};

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
    data: crate::data::resolve::State,
    analyze_data: bool,
    source_snapshot: Option<process::SourceSnapshot>,
    /// The refusal of the whole-graph check, kept until the root closes. The compile
    /// then runs without a snapshot, so the fault that it meets first is reported.
    /// A data bind returns this refusal and reads nothing, and a compile that meets
    /// no fault of its own fails with it at the root.
    graph_refusal: Option<MdsError>,
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
            data: crate::data::resolve::State::default(),
            analyze_data: false,
            source_snapshot: None,
            graph_refusal: None,
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

    /// Only compile entry points may opt in to local data reads.
    pub(crate) fn compiling(fs: Box<dyn FileSystem>) -> Self {
        let mut cache = Self::tooling(fs);
        cache.data.compile = true;
        cache
    }

    /// A compile that derives the requests of its URL entries and binds the responses of
    /// `ctx`. It is the only role with a step context: the tooling roles take none.
    pub(crate) fn stepping(fs: Box<dyn FileSystem>, ctx: crate::data::step::StepCtx) -> Self {
        let mut cache = Self::compiling(fs);
        cache.data.net = Some(ctx);
        cache
    }

    /// Take the step context back after a step compile.
    pub(crate) fn take_step(&mut self) -> Option<crate::data::step::StepCtx> {
        self.data.net.take()
    }

    /// The check gate can validate without returning a rendered artifact.
    pub(crate) fn tooling(fs: Box<dyn FileSystem>) -> Self {
        let mut cache = Self::with_fs(fs);
        cache.analyze_data = true;
        cache
    }

    pub(crate) fn has_pending_data(&self) -> bool {
        self.data.pending
    }

    fn read_module(&self, key: &str) -> Result<(Arc<String>, usize), MdsError> {
        if let Some(snapshot) = &self.source_snapshot {
            let source = snapshot.source(key).ok_or_else(MdsError::internal)?;
            return Ok((Arc::clone(source), 0));
        }
        let source = self.fs.read(key)?;
        let capacity = source.capacity();
        Ok((Arc::new(source), capacity))
    }

    fn normalize_module_edge(&self, ctx: &ModuleCtx<'_>, path: &str) -> Result<String, MdsError> {
        if let Some(snapshot) = &self.source_snapshot {
            return snapshot
                .edge(ctx.key, path)
                .map(str::to_owned)
                .ok_or_else(MdsError::internal);
        }
        self.fs.normalize_in_dir(ctx.base_dir, path)
    }

    /// Returns normalized keys of all modules resolved during compilation,
    /// in first-resolution order (depth-first traversal), then the keys of the
    /// local data files the compile read, in first-read order.
    ///
    /// This includes the entry module itself. Use this after a successful
    /// `resolve_path` / `resolve_key` / `resolve_source` call to obtain the
    /// dependency graph. Callers that want to exclude the entry point should
    /// filter it out themselves (see `compile_virtual_with_deps`).
    ///
    /// A key occurs once: a data file that a module import also names is listed
    /// with the modules. Only a compile entry point reads data files, so a cache
    /// from [`ModuleCache::new`], [`ModuleCache::native`], [`ModuleCache::virtual_fs`]
    /// or [`ModuleCache::with_fs`] lists modules only. A data entry that a runtime
    /// variable replaces reads no file, and no URL is ever listed.
    pub fn dependencies(&self) -> Vec<String> {
        let data = self
            .data
            .files
            .keys()
            .filter(|key| !self.modules.contains_key(*key));
        self.modules.keys().chain(data).cloned().collect()
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
        let (source, read_bytes) = self.read_module(key)?;

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
        let (mark, read) = self.begin_module(key, runtime_vars, read_bytes);
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
            self.prepare_data(&ctx, warnings)?;
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
        let resolved = read.and_then(|()| {
            self.prepare_data(&ctx, warnings)?;
            self.process_module(&ctx, false, false, warnings)
        });
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
            self.prepare_data(&ctx, warnings)?;
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
        let result = read.and_then(|()| {
            self.prepare_data(&ctx, warnings)?;
            self.process_module_intrinsic(&ctx, false, warnings)
        });
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
        let result = read.and_then(|()| {
            self.prepare_data(&ctx, warnings)?;
            self.process_module_intrinsic_opts(&ctx, false, opts, warnings)
        });
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

    // Call even on error. Only propagate the pop error if the module result is Ok.
    // At the root, a kept graph refusal becomes the pop error, so a module error
    // that the compile met first still wins.
    fn pop_resolving(&mut self, key: &str) -> Result<(), MdsError> {
        let result = Self::check_lifo_pop(self.resolving.pop(), key);
        if self.resolving.is_empty() {
            if let Some(snapshot) = self.source_snapshot.take() {
                self.budget.mem.release_buffers(snapshot.charge);
            }
            if let Some(refusal) = self.graph_refusal.take() {
                return result.and(Err(refusal));
            }
        }
        result
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
}

impl Default for ModuleCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Attach the importing directive's span to a spanless file, module, or cycle error.
/// Keep an existing span so errors inside another module retain their own source.
/// Keep the path a spanless file error names: the import as written when the import
/// itself is missing, or a missing data file that the imported module declares.
fn attach_import_span(err: MdsError, file_str: &str, source: &str, offset: usize) -> MdsError {
    // Compute the span length as the number of bytes from `offset` to the
    // end of the `@import` line (not including the newline character itself),
    // so the whole directive is underlined. A non-boundary or out-of-range
    // offset can only come from a defect in offset attribution, never from a
    // template; `line_len_at` degrades it to a zero-length span instead of
    // slicing (#220) — `MdsError::at` then keeps the numeric offset and drops
    // the snippet rather than mis-attributing.
    let line_len = line_len_at(source, offset);
    match err {
        MdsError::FileNotFound {
            path, span: None, ..
        } => MdsError::file_not_found_at(path, file_str, source, offset, line_len),
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
