//! Path checks, source-map anchors, and import error and frontmatter helpers.

use super::{
    mask_file_key, parse_frontmatter_yaml_with, probe_file_key, Allowance, IndexSet, MdsError,
    MemBudget, ModuleCache, Part, FILE_KEY_FORM, INDENTED_DIRECTIVES,
};
#[cfg(doc)]
use super::{FileSystem, NativeFs, VirtualFs};

impl ModuleCache {
    /// Validate an entry path and resolve it to its key through the backend.
    ///
    /// Entry validation runs here, before the backend is called, so a custom
    /// [`FileSystem`] passed to [`ModuleCache::with_fs`] is covered as well as the
    /// built-in backends (PF-004): an empty entry path, or one containing a null
    /// byte or another forbidden path character (#265), is refused with `mds::io`
    /// and never reaches [`FileSystem::resolve_entry`].
    pub(super) fn resolve_entry_key(&self, path: &str) -> Result<String, MdsError> {
        crate::fs::validate_entry_path(path)?;
        self.fs.resolve_entry(path)
    }

    /// Validate the base directory of a string compile and anchor it through the
    /// backend.
    ///
    /// The forbidden-character check (#265) runs here, before the backend is
    /// called, for the same reason entry validation does: a custom [`FileSystem`]
    /// keeps the default identity `anchor_base_dir` and checks nothing (PF-004).
    pub(super) fn anchor_base(&self, base_dir: &str) -> Result<String, MdsError> {
        crate::fs::reject_forbidden_path_chars("base directory", base_dir)?;
        self.fs.anchor_base_dir(base_dir)
    }

    /// Anchor the project root at a module's `base_dir` when no entry call has
    /// established one, before `sources[]` is relativized — the defense-in-depth
    /// half of the source-map choke point, against a future entry path that skips
    /// root establishment (PF-004 shape).
    ///
    /// Best-effort by design, never propagated. Every built-in entry API anchors
    /// first, so [`NativeFs`] never reaches the call without a root, and the
    /// default `anchor_base_dir` ([`VirtualFs`]) cannot fail — nor does it give
    /// `source_root` a value. Only a custom backend without a `source_root` of its
    /// own reaches it, for a directory it may never have been asked to anchor, and
    /// that backend has already accepted the entry: a refusal must not fail the
    /// compile. It leaves `source_root()` at `None`, and `relativize_source` then
    /// degrades each absolute source to its basename — never an absolute path.
    pub(super) fn anchor_root_for_source_map(&self, base_dir: &str) {
        if self.fs.source_root().is_none() && !base_dir.is_empty() {
            let _ = self.fs.anchor_base_dir(base_dir);
        }
    }
}

/// The first rule an `@import` / `@extends` / frontmatter `imports:` path breaks.
///
/// One classification shared by [`validate_import_path`] (body directives) and the
/// frontmatter `imports:` parser, so both report the same, real reason (#265).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImportPathViolation {
    /// The path does not start with `./` or `../`.
    NotRelative,
    /// The path contains a null byte, which could truncate it in some OS APIs.
    NullByte,
    /// The path carries a forbidden path character other than NUL (#265).
    ForbiddenChar(char),
}

impl ImportPathViolation {
    /// The violation as a message fragment: `must start with './' or '../'`,
    /// `contains null byte`, `contains forbidden character U+XXXX`.
    pub(super) fn reason(self) -> String {
        match self {
            Self::NotRelative => "must start with './' or '../'".to_string(),
            Self::NullByte => "contains null byte".to_string(),
            Self::ForbiddenChar(ch) => {
                format!("contains forbidden character U+{:04X}", u32::from(ch))
            }
        }
    }
}

/// Classify `path` as an import path: `None` when it is acceptable.
///
/// Checked in order — relative form, null byte, then the rest of the forbidden
/// class — so a NUL keeps its own reason even though U+0000 is forbidden too.
pub(super) fn import_path_violation(path: &str) -> Option<ImportPathViolation> {
    if !path.starts_with("./") && !path.starts_with("../") {
        return Some(ImportPathViolation::NotRelative);
    }
    if path.contains('\0') {
        return Some(ImportPathViolation::NullByte);
    }
    crate::fs::first_forbidden_char(path).map(ImportPathViolation::ForbiddenChar)
}

/// Validate that an import path is safe and relative.
///
/// Rejects paths that are not `./`/`../`-relative, contain a null byte, or carry
/// any other forbidden path character (#265). It runs before the backend is
/// called, so a custom `with_fs` backend is covered too. The path is escaped in
/// every message that shows it.
pub(super) fn validate_import_path(path: &str) -> Result<(), MdsError> {
    match import_path_violation(path) {
        None => Ok(()),
        Some(ImportPathViolation::NotRelative) => Err(MdsError::import_error(format!(
            "import path must be relative (start with './' or '../'): \"{}\"",
            crate::lint::escape_path_for_message(path)
        ))),
        Some(ImportPathViolation::NullByte) => {
            Err(MdsError::import_error("import path contains null byte"))
        }
        Some(ImportPathViolation::ForbiddenChar(ch)) => Err(MdsError::import_error(
            crate::fs::forbidden_char_message("import path", ch, path),
        )),
    }
}

/// Format a cycle chain like "a.mds → b.mds → a.mds" from the resolving set.
///
/// `IndexSet` preserves insertion order, so we can use it as both the set
/// and the ordered stack for cycle path reconstruction.
pub(super) fn build_cycle_string(resolving: &IndexSet<String>, repeated: &str) -> String {
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

/// Attach "(in frontmatter imports[i])" context to errors that have no source span.
///
/// Errors that already carry a span (e.g. cascading errors inside the imported file)
/// are returned unchanged so they continue to report their own locations.
pub(super) fn attach_frontmatter_index(err: MdsError, i: usize) -> MdsError {
    match err {
        MdsError::FileNotFound {
            path, span: None, ..
        } => MdsError::import_error(format!(
            "file not found: \"{path}\" (in frontmatter imports[{i}])"
        )),
        MdsError::ModuleNotFound {
            key, span: None, ..
        } => MdsError::import_error(format!(
            "module not found: \"{key}\" (in frontmatter imports[{i}])"
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

/// Parse the frontmatter YAML of a module into a `serde_yaml_ng::Mapping`.
/// Return it with the bytes that the parse spent.
///
/// The byte allowance is the smaller of the rest of the budget and `MAX_OUTPUT_SIZE`.
/// The spent bytes stay charged as a temporary; a caller that keeps the tree moves
/// them to the module's kept bytes. The mapping is `None` when there is no
/// frontmatter or when the YAML is not a mapping.
pub(super) fn parse_frontmatter_mapping(
    frontmatter: Option<&crate::ast::Frontmatter>,
    mem: &mut MemBudget,
) -> Result<(Option<serde_yaml_ng::Mapping>, usize), MdsError> {
    let Some(fm) = frontmatter else {
        return Ok((None, 0));
    };
    let (yaml, spent) = parse_frontmatter_yaml_with(&fm.raw, Allowance::in_compile(mem))?;
    mem.charge(Part::Temp, Some(spent))?;
    let raw = &fm.raw;
    let expected = probe_file_key(raw).map_err(|_| MdsError::yaml_error(FILE_KEY_FORM))?;
    let map = match yaml {
        serde_yaml_ng::Value::Mapping(map) => Some(map),
        _ => None,
    };
    let actual = map.as_ref().and_then(|map| map.get(INDENTED_DIRECTIVES));
    if actual.map(serde_yaml_ng::Value::as_bool) != expected.map(Some) {
        return Err(MdsError::yaml_error(FILE_KEY_FORM));
    }

    // A first-line hit cannot be inside another YAML value. For later hits,
    // verify that an alternate YAML key did not hide a match inside a scalar.
    if expected.is_some()
        && !raw
            .strip_prefix(INDENTED_DIRECTIVES)
            .is_some_and(|tail| tail.starts_with(':'))
    {
        let mark = mem.mark();
        mem.charge(Part::Temp, Some(raw.len()))?;
        let verified = (|| -> Result<bool, MdsError> {
            let masked = mask_file_key(raw).ok_or_else(MdsError::internal)?;
            let (proof, _) = parse_frontmatter_yaml_with(&masked, Allowance::in_compile(mem))
                .map_err(|err| match err {
                    MdsError::YamlError { .. } => MdsError::yaml_error(FILE_KEY_FORM),
                    other => other,
                })?;
            Ok(proof
                .as_mapping()
                .and_then(|map| map.get(INDENTED_DIRECTIVES))
                == Some(&serde_yaml_ng::Value::Null))
        })();
        mem.release_to(mark);
        if !verified? {
            return Err(MdsError::yaml_error(FILE_KEY_FORM));
        }
    }
    Ok((map, spent))
}
