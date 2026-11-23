//! Per-module context and results from the definition and inheritance walks.

#[cfg(doc)]
use super::ResolvedModule;
use super::{
    line_len_at, Arc, EffectiveBlock, FunctionDef, HashMap, HashSet, IndexMap, MdsError,
    ModuleCache, Node, OnceCell, Origin, Scope, VarLayer,
};

/// Collected output of the AST definition/import walk in `collect_definitions_and_imports`.
pub(super) struct CollectedDefs {
    pub(super) functions: HashMap<String, Arc<FunctionDef>>,
    pub(super) has_explicit_exports: bool,
    pub(super) explicit_exports: HashSet<String>,
    /// Tracks declared `@block` names for duplicate-block and block-vs-function collision detection.
    ///
    /// Shared with `collect_define` so that a `@block foo:` and a `@define foo()` in the same
    /// module surface as `mds::name_collision` (same namespace — decision #10).
    pub(super) block_names: HashSet<String>,
}

/// Shared output of [`ModuleCache::resolve_extends_components`].
///
/// Steps 3a-3d (base resolution, child-only-blocks check, effective-blocks construction,
/// and scope merge) are identical for text and messages modes. This struct carries those
/// results so the two terminal steps differ only in how the spliced regions are evaluated:
/// - Cached text path: `validator::validate` → `evaluate_extends_regions`
/// - Intrinsic path:   `has_message_block` dispatch → `evaluate_message_regions`
///   (Messages) or `evaluate_extends_regions` + clean/frontmatter (Markdown)
pub(super) struct ExtendsComponents {
    /// Merged scope (base < child < runtime), with FM imports and functions loaded.
    pub(super) scope: Scope,
    /// Merged function map (base functions + child overrides).
    pub(super) functions: HashMap<String, Arc<FunctionDef>>,
    /// Root ancestor skeleton, Arc-shared (O(1), no deep-clone).
    pub(super) effective_skeleton: Arc<[Node]>,
    /// Fully-overridden block map for this subtree, each entry carrying its `Origin`.
    pub(super) effective_blocks: IndexMap<String, EffectiveBlock>,
    /// Origin of the root-ancestor skeleton (file + source for non-block skeleton nodes).
    pub(super) skeleton_origin: Origin,
    /// Whether the child declared any `@export` directives.
    pub(super) has_explicit_exports: bool,
    /// Named exports from the child.
    pub(super) explicit_exports: HashSet<String>,
    /// Deep-merged frontmatter YAML string (base < child, reserved keys excluded).
    ///
    /// Replaces the child's raw frontmatter in the compiled output so that `@extends`
    /// children emit the full merged key set rather than only their own keys (#154).
    /// `None` when the merged mapping is empty (no non-reserved FM keys on either side).
    pub(super) merged_frontmatter: Option<String>,
}

/// The immediate directive that requests a module's type check.
pub(super) struct ImportSite<'a> {
    file_str: &'a str,
    source: &'a str,
    offset: usize,
}

impl ImportSite<'_> {
    pub(super) fn attach_type_error(self, err: MdsError) -> MdsError {
        match err {
            MdsError::NotMdsFile {
                path, span: None, ..
            } => MdsError::not_mds_file_at(
                path,
                self.file_str,
                self.source,
                self.offset,
                line_len_at(self.source, self.offset),
            ),
            other => other,
        }
    }
}

impl ModuleCtx<'_> {
    #[inline(never)]
    pub(super) fn origin(&self, cache: &mut ModuleCache) -> Result<&Origin, MdsError> {
        if self.origin.get().is_none() {
            cache.charge_kept(self.origin_bytes())?;
        }
        Ok(self.origin.get_or_init(|| self.make_origin()))
    }

    pub(super) fn origin_bytes(&self) -> usize {
        self.source
            .len()
            .saturating_add(self.key.len())
            .saturating_add(self.file_str.len())
            .saturating_add(6 * size_of::<usize>())
    }

    pub(super) fn make_origin(&self) -> Origin {
        Origin {
            file: Arc::from(self.key),
            display: Arc::from(self.file_str),
            source: Arc::from(self.source),
        }
    }

    pub(super) fn import_site(&self, offset: usize) -> ImportSite<'_> {
        ImportSite {
            file_str: self.file_str,
            source: self.source,
            offset,
        }
    }
}

/// Bundle of borrowed per-module context threaded through the AST walk helpers.
pub(super) struct ModuleCtx<'a> {
    /// Canonical key of the source file.
    ///
    /// For `NativeFs` compiles this is the absolute canonical path returned by
    /// `fs.resolve_entry()` (the entry) or `fs.normalize_in_dir()` (an import).  For
    /// string-source compiles this is `SOURCE_LABEL`.  Used for source-map interning
    /// (`MapBuilder::new` / `Origin::file`) — must be the stable, dedup-safe identity
    /// key, never the display-friendly path.
    pub(super) key: &'a str,
    /// Display-safe (root-relative) path for user-visible strings — error messages,
    /// `NamedSource` names, validator diagnostics (R3 / CWE-209).
    ///
    /// Derived from `display_path_for(fs, key)` at construction time.  For `VirtualFs`
    /// and string-source sentinels this equals `key` (already relative / already a
    /// `<…>` sentinel).
    pub(super) file_str: &'a str,
    /// Raw file content used for source-span diagnostics (offset → line/column lookup).
    pub(super) source: &'a str,
    /// Directory from which relative `@import`/`@extends` paths are resolved.
    ///
    /// For file-backed modules this is the parent directory of the normalized file key
    /// (computed via `FileSystem::parent_dir`). For string-source modules this is the
    /// canonicalized `base_dir` supplied by the caller.
    pub(super) base_dir: &'a str,
    /// Variables injected at call-time (e.g. via `--set` or the public API `compile` call).
    pub(super) runtime_vars: &'a Arc<VarLayer>,
    /// Shared provenance, allocated only if this resolution needs an owned origin.
    pub(super) origin: OnceCell<Origin>,
}

/// Whether `name` is an available export of a module: every name when the module
/// declares no `@export` list, only the listed names when it does.
///
/// The one export rule — [`ResolvedModule::is_exported`] applies it, and so does the
/// `prompt` gate of both module paths (`process_module`, `process_module_extends`).
pub(super) fn is_visible_export(
    has_explicit_exports: bool,
    explicit_exports: &HashSet<String>,
    name: &str,
) -> bool {
    !has_explicit_exports || explicit_exports.contains(name)
}

/// The [`crate::sourcemap::FragmentMap`] an imported module's importers splice, from
/// the builder that recorded its `prompt` body — or `None`, when there is nothing
/// correct to splice:
///
/// - The builder dropped a segment at the cap (AC-PERF-03): a partial map would
///   misattribute, so none is kept and a warning says what that means for the
///   importer — the text included from the module is left unmapped, while the rest of
///   the importer's map is still produced.
/// - The body is empty (`None`): an `@include` of the module adds no text (PF-034; the
///   evaluator warns at the `@include`).
///
/// `display` names the module in the warning.
pub(super) fn fragment_map_from(
    builder: crate::sourcemap::MapBuilder,
    body: Option<&str>,
    display: &str,
    warnings: &mut Vec<String>,
) -> Option<Arc<crate::sourcemap::FragmentMap>> {
    if builder.segments_dropped {
        // The module filename is an untrusted identifier — WIRE, per the spec 7.5
        // per-field rule: a filename is never legitimately multi-line, and
        // `emit_warnings` prints this string to stderr in HUMAN mode, where a raw `\n`
        // would forge a standalone status line (CWE-117).
        warnings.push(format!(
            "source map segment cap ({} segments) exceeded in imported module '{}'; \
             text included from it is left unmapped in the source map",
            crate::limits::MAX_SOURCEMAP_SEGMENTS,
            crate::lint::sanitize_control_chars_wire(display),
        ));
        return None;
    }
    body.map(|_| Arc::new(builder.into_fragment()))
}
