//! Path checks for local data files.

use std::path::Path;

use crate::fs::{reject_forbidden_in_path, validate_relative_import};
use crate::{escape_path_for_message, FileSystem, MdsError};

pub(crate) fn normalize(
    fs: &(impl FileSystem + ?Sized),
    dir: &str,
    relative: &str,
) -> Result<String, MdsError> {
    validate_relative_import(relative)?;
    if !relative.starts_with("./") && !relative.starts_with("../") {
        return Err(refused(relative, "must start with './' or '../'"));
    }
    if relative.contains("{{") {
        return Err(refused(relative, "references are not allowed"));
    }
    if ![".json", ".yaml", ".yml"]
        .iter()
        .any(|ext| relative.ends_with(ext))
    {
        return Err(refused(
            relative,
            "expected the .json, .yaml or .yml extension",
        ));
    }
    check_segments(relative, relative, true)?;

    let key = fs.normalize_in_dir(dir, relative)?;
    check_key(&key, fs.source_root().as_deref().map(Path::new), relative)?;
    Ok(key)
}

/// Inspect the exact key that the backend uses for the read, not a display path.
pub(crate) fn check_key(key: &str, root: Option<&Path>, shown: &str) -> Result<(), MdsError> {
    let path = Path::new(key);
    reject_forbidden_in_path(path, shown)?;
    let relative = match root {
        Some(root) => path
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| refused(shown, "escapes project directory"))?,
        None if key.starts_with(['/', '\\']) || key.as_bytes().get(1) == Some(&b':') => {
            return Err(refused(shown, "escapes project directory"));
        }
        None => key,
    };
    check_segments(relative, shown, false)
}

fn check_segments(path: &str, shown: &str, allow_relative: bool) -> Result<(), MdsError> {
    for segment in path.split(['/', '\\']) {
        if allow_relative && matches!(segment, "." | "..") {
            continue;
        }
        if segment.starts_with('.') {
            return Err(refused(shown, "dot-prefixed segments are not allowed"));
        }
        // A Windows path with a trailing dot or space can have two spellings.
        #[cfg(windows)]
        let segment = segment.trim_end_matches(['.', ' ']);
        if segment.eq_ignore_ascii_case("node_modules") {
            return Err(refused(shown, "node_modules segments are not allowed"));
        }
    }
    Ok(())
}

fn refused(shown: &str, reason: &str) -> MdsError {
    MdsError::import_error(format!(
        "data path \"{}\": {reason}",
        escape_path_for_message(shown)
    ))
}
