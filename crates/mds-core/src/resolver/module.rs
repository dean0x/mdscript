//! Resolved module data, exports, and module type checks.

use super::{
    is_visible_export, Arc, BlockNode, FunctionDef, HashMap, HashSet, IndexMap, MdsError,
    NamespaceScope, Node, Origin, PromptText, Value,
};

/// An effective block together with the origin (file + source) its offsets index into.
///
/// The origin follows the **winning override**: if a child file overrides the block,
/// the origin is the child's file; if the base default is used, the origin is the
/// base's file. This is stamped at the last-wins insertion in `apply_block_overrides`.
///
/// A clone shares the block node and the origin's key, display path, and source text.
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
/// Use the accessors for imports. They respect the module's export list.
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
    pub(crate) prompt_body: Option<PromptText>,
    pub(super) namespace: Arc<NamespaceScope>,
    /// Pre-computed source-map fragment for this module's `prompt` output.
    ///
    /// Populated by `process_module` — and, for an extending module, by
    /// `process_module_extends` (#412) — when `ModuleCache::source_map_mode` is
    /// true and the module exports a non-empty `prompt` (S6).  `None` for
    /// skeleton entries, for a module whose map passed the segment cap, and for
    /// all non-source-map compilations (zero-cost AC-PERF-01).
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
    /// The bytes charged for `frontmatter_values`, part of this entry's kept bytes.
    pub(crate) frontmatter_bytes: usize,
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
            namespace: Arc::clone(&self.namespace),
            prompt_map: self.prompt_map.clone(),
            raw_frontmatter: self.raw_frontmatter.clone(),
            has_explicit_exports: self.has_explicit_exports,
            explicit_exports: self.explicit_exports.clone(),
            effective_skeleton: Arc::clone(&self.effective_skeleton),
            effective_blocks: self.effective_blocks.clone(),
            skeleton_origin: self.skeleton_origin.clone(),
            frontmatter_values: self.frontmatter_values.clone(),
            frontmatter_bytes: self.frontmatter_bytes,
            is_skeleton: self.is_skeleton,
        }
    }
}

impl ResolvedModule {
    /// Return `true` if `name` is an available export of this module.
    ///
    /// When no explicit `@export` list is present every name is visible.
    /// When an explicit list exists only the listed names are visible.
    fn is_exported(&self, name: &str) -> bool {
        is_visible_export(self.has_explicit_exports, &self.explicit_exports, name)
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

    /// Get the prompt body as an owned Value, if it is an available export.
    pub fn get_prompt_value(&self) -> Option<Value> {
        self.shared_prompt_value().as_deref().cloned()
    }

    pub(crate) fn shared_prompt_value(&self) -> Option<Arc<Value>> {
        if self.is_exported("prompt") {
            self.prompt_body.as_ref().map(PromptText::shared_value)
        } else {
            None
        }
    }

    pub(crate) fn shared_namespace(&self) -> Arc<NamespaceScope> {
        Arc::clone(&self.namespace)
    }

    pub(super) fn build_namespace(
        functions: &HashMap<String, Arc<FunctionDef>>,
        has_explicit_exports: bool,
        explicit_exports: &HashSet<String>,
        prompt_body: Option<&PromptText>,
        prompt_map: Option<&Arc<crate::sourcemap::FragmentMap>>,
    ) -> Arc<NamespaceScope> {
        let is_exported = |name| is_visible_export(has_explicit_exports, explicit_exports, name);
        let functions = functions
            .iter()
            .filter(|(name, _)| is_exported(name.as_str()))
            .map(|(name, func)| (name.clone(), Arc::clone(func)))
            .collect();
        let prompt_exported = is_exported("prompt");
        let prompt_suppressed_by_exports = !prompt_exported && prompt_body.is_some();
        let (prompt_body, prompt_map) = if prompt_exported {
            (prompt_body.cloned(), prompt_map.cloned())
        } else {
            (None, None)
        };
        Arc::new(NamespaceScope {
            functions,
            prompt_body,
            prompt_map,
            prompt_suppressed_by_exports,
        })
    }
}

/// A module named two ways: by the key the backend resolved it to, and by the path
/// the caller typed to reach it (#417).
///
/// `key` is the identity every check, cache lookup and read uses — on
/// [`crate::NativeFs`] the canonical absolute path, in its on-disk spelling.
/// `typed` is the text that named the module: the entry path passed to the API, or
/// the `@import`, `@export … from`, frontmatter `imports:` or `@extends` string as
/// written in the template. `typed` only ever names the module in an error message,
/// escaped as it enters it; nothing compares it with `key` (canonicalization
/// re-spells a path, #408).
///
/// Built in two steps that each name their value, [`ModuleRef::keyed`] then
/// [`ModuleKey::typed`], so the two strings are never paired by position: a key
/// passed where the typed path goes is not a swap the compiler lets through.
///
/// # Examples
///
/// ```
/// let module = mds::ModuleRef::keyed("/proj/doc.txt").typed("./doc.txt");
/// assert_eq!((module.key, module.typed), ("/proj/doc.txt", "./doc.txt"));
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleRef<'a> {
    /// The key the backend resolved the module to.
    pub key: &'a str,
    /// The path the caller typed to reach the module.
    pub typed: &'a str,
}

impl<'a> ModuleRef<'a> {
    /// The first step of a [`ModuleRef`]: the module's resolved key. The path the
    /// caller typed is the second, [`ModuleKey::typed`].
    pub fn keyed(key: &'a str) -> ModuleKey<'a> {
        ModuleKey { key }
    }
}

/// A module's resolved key, waiting for the path the caller typed to reach it: the
/// first step of a [`ModuleRef`], from [`ModuleRef::keyed`].
#[must_use = "a ModuleKey is only the first step: add the typed path with `typed`"]
#[derive(Debug, Clone, Copy)]
pub struct ModuleKey<'a> {
    key: &'a str,
}

impl<'a> ModuleKey<'a> {
    /// The [`ModuleRef`] of this key, reached by the path `typed`.
    pub fn typed(self, typed: &'a str) -> ModuleRef<'a> {
        ModuleRef {
            key: self.key,
            typed,
        }
    }
}

/// Refuse a module that is not an MDS file, as the resolver refuses every module it
/// reads: neither a `.mds` file nor a `.md` file whose frontmatter declares `type: mds`
/// is [`MdsError::NotMdsFile`] (`mds::not_mds`).
///
/// The extension is judged on `module.key` — on [`crate::NativeFs`] the on-disk
/// spelling, so `Doc.MDS` typed for `doc.mds` is an MDS file (#408); `source` is the
/// module's text. The error names `module.typed`, the path as the caller typed it,
/// escaped with [`crate::escape_path_for_message`] — never the resolved key, which on
/// [`crate::NativeFs`] is an absolute path (#417).
///
/// This is the resolver's own check, run after a module is read and before it is
/// parsed. `@mdscript/mds`'s WASM backend, whose JS pre-scanner reads each file itself,
/// runs it through the `preflightModule` export in the same place, so a file that is
/// not an MDS file is refused identically on both backends — and its import-like
/// lines are never followed (#417).
///
/// # Errors
///
/// [`MdsError::NotMdsFile`] naming `module.typed`.
///
/// # Examples
///
/// ```
/// use mds::ModuleRef;
///
/// mds::check_module_type(ModuleRef::keyed("/proj/doc.mds").typed("./doc.mds"), "Hello!\n")?;
/// let page = ModuleRef::keyed("/proj/page.md").typed("page.md");
/// mds::check_module_type(page, "---\ntype: mds\n---\nHi\n")?;
///
/// let doc = ModuleRef::keyed("/proj/doc.txt").typed("./doc.txt");
/// let err = mds::check_module_type(doc, "Hello!\n").unwrap_err();
/// assert_eq!(err.to_string(), "not an MDS file: ./doc.txt");
/// # Ok::<(), mds::MdsError>(())
/// ```
pub fn check_module_type(module: ModuleRef<'_>, source: &str) -> Result<(), MdsError> {
    let key = module.key;
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

    Err(MdsError::not_mds_file(
        crate::lint::escape_path_for_message(module.typed).into_owned(),
    ))
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
pub(super) fn has_type_mds_frontmatter(source: &str) -> bool {
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
pub(super) fn has_type_mds_frontmatter_raw(raw: &str) -> bool {
    raw.lines().any(is_type_mds_line)
}
