//! Input-argument resolution shared by the directory-mode subcommands (#413).
//!
//! `build`, `check`, `fmt`, `lint` and `watch` all take a directory to walk. Each used
//! to check it on its own: four looked at the path as typed with `symlink_metadata`,
//! which a trailing `link/` walks straight past (the operating system follows the link
//! through the slash), and `watch` ran `NativeFs::check_symlink`, which cannot take a
//! path with no final name, so `mds watch .` failed with `file not found: .`.
//! [`resolve_directory_argument`] is the one check all five now make.

use std::path::{Path, PathBuf};

use mds::MdsError;

/// Check the directory argument `typed` of a directory-mode subcommand and return its
/// canonical form (#413).
///
/// Callers run it only once `typed.is_dir()` holds, and walk the TYPED path, so each
/// per-file message names a file as reached from what the user typed; `mds watch`
/// matches its event paths against the canonical form this returns. Every refusal is
/// `mds::io` (exit 2) and names the directory as typed, escaped by
/// [`mds::escape_path_for_message`].
///
/// The path is checked by [`mds::NativeFs::check_directory`], which takes it with or
/// without a final name (`src`, `src/`, `link/.`, `.`, `..`, `sub/..`): a forbidden
/// path character as typed or in the canonical form (#265) is refused in mds-core's
/// words, one message for both forms. This adds the CLI's own policy:
///
/// - A symlinked final component is refused as
///   `directory argument must not be a symlink: "<typed>"`. `link/..` names no link,
///   so it is accepted: it is the directory above the link's target on Unix.
/// - The filesystem root as the directory to walk is refused whatever spelling reaches
///   it: `directory argument must not be the filesystem root: "<typed>"`. A filesystem
///   root stays usable as a project root or as the working directory (#371): only
///   walking one is refused.
///
/// A symlink swapped onto the typed path between this check and the walk is the
/// check-then-open window every path-based read has. `mds watch`, which walks the
/// typed path for as long as it runs, re-checks that it still leads to the canonical
/// directory before every compile.
pub(crate) fn resolve_directory_argument(typed: &Path) -> Result<PathBuf, MdsError> {
    let canonical = mds::NativeFs::check_directory(typed).map_err(|e| match e {
        MdsError::ImportError { .. } => refusal(typed, "must not be a symlink"),
        other => other,
    })?;
    if is_filesystem_root(&canonical) {
        return Err(refusal(typed, "must not be the filesystem root"));
    }
    Ok(canonical)
}

/// Whether `path` is a filesystem root: `/` on Unix; on Windows a drive root (`C:\`),
/// its verbatim form (`\\?\C:\`) or a UNC share root (`\\server\share`,
/// `\\?\UNC\server\share`). A root has a root component and nothing above it.
fn is_filesystem_root(path: &Path) -> bool {
    path.has_root() && path.parent().is_none()
}

/// `directory argument <rule>: "<typed>"`, `mds::io`, the path escaped for a message.
fn refusal(typed: &Path, rule: &str) -> MdsError {
    MdsError::Io {
        message: format!(
            "directory argument {rule}: \"{}\"",
            mds::escape_path_for_message(&typed.to_string_lossy())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root predicate over every degenerate shape a path can take (PF-006: `.`,
    /// `..`, the empty path and a bare name have no parent or no name either, and none
    /// of them is a root).
    #[test]
    fn filesystem_root_predicate_covers_every_root_form() {
        let mut rows: Vec<(&str, bool)> = vec![
            ("", false),
            (".", false),
            ("..", false),
            ("src", false),
            ("./src", false),
            ("sub/..", false),
        ];
        #[cfg(unix)]
        rows.extend([("/", true), ("/src", false), ("/src/..", false)]);
        #[cfg(windows)]
        rows.extend([
            (r"C:\", true),
            (r"\\?\C:\", true),
            (r"\\server\share", true),
            (r"\\server\share\", true),
            (r"\\?\UNC\server\share", true),
            (r"C:", false),
            (r"C:\src", false),
            (r"\\?\C:\src", false),
            (r"\\server\share\src", false),
        ]);
        for (path, expected) in rows {
            assert_eq!(is_filesystem_root(Path::new(path)), expected, "{path:?}");
        }
    }

    /// Every degenerate shape through the resolver itself (PF-006). The unit tests run
    /// in the crate directory, so `.`, `..` and `src` exist and resolve.
    #[test]
    fn resolve_directory_argument_takes_every_path_shape() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .unwrap();
        for (typed, expected) in [
            (".", crate_dir.clone()),
            ("./", crate_dir.clone()),
            ("src/..", crate_dir.clone()),
            ("..", crate_dir.parent().unwrap().to_path_buf()),
            ("src", crate_dir.join("src")),
            ("src/", crate_dir.join("src")),
        ] {
            let canonical = resolve_directory_argument(Path::new(typed))
                .unwrap_or_else(|e| panic!("{typed:?}: {e}"));
            assert_eq!(canonical, expected, "{typed:?}");
        }

        let err = resolve_directory_argument(Path::new("")).unwrap_err();
        assert!(
            matches!(&err, MdsError::FileNotFound { path, .. } if path.is_empty()),
            "the empty path: {err:?}"
        );

        // The filesystem root, typed. Only canonicalized, never walked.
        let root = crate_dir.ancestors().last().unwrap();
        let err = resolve_directory_argument(root).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "directory argument must not be the filesystem root: \"{}\"",
                root.display()
            )
        );
    }

    /// A forbidden character in the path as typed is refused before the filesystem is
    /// touched: the path does not exist.
    #[test]
    fn resolve_directory_argument_refuses_a_forbidden_character_as_typed() {
        let typed = format!("no-such-dir{}x", '\u{1b}');
        let err = resolve_directory_argument(Path::new(&typed)).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "path contains forbidden character U+001B: \"no-such-dir{}u001Bx\"",
                '\\'
            )
        );
    }
}
