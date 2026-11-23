//! Check the destination of a compiled entry before a write.

use std::path::{Path, PathBuf};

use super::out_paths::{warn_output_extension_mismatch, OutputKind};
#[cfg(doc)]
use super::stdio::write_output;
use mds::{effective_parent, MdsError};
use miette::Result;

/// A single-file entry in the two forms [`admit_output`] takes, always in
/// `(typed, canonical)` order, so the two cannot be passed swapped.
///
/// `typed` is the path as the user typed it: a refusal names the entry that way, never
/// by its canonical absolute path. `canonical` is the entry's identity, which
/// [`admit_output`] compares with the output's through [`file_identity`], canonical with
/// canonical — `typed` is never compared with it as text (#408). `mds watch` lends the
/// entry it watches in its two forms. `mds build` compiles its entry once, by the path as
/// typed, and never holds a canonical form, so it passes the typed path as both:
/// [`file_identity`] canonicalizes it.
#[derive(Clone, Copy)]
pub(crate) struct EntryPaths<'a> {
    pub(crate) typed: &'a Path,
    pub(crate) canonical: &'a Path,
}

impl<'a> EntryPaths<'a> {
    /// An entry held only as typed, as `mds build` holds it: the typed path as both.
    pub(crate) fn as_typed(path: &'a Path) -> Self {
        Self {
            typed: path,
            canonical: path,
        }
    }
}

/// The canonical path of the file `path` names, for comparing two paths as files, or
/// `None` when not even its directory resolves.
///
/// Its directory is resolved as [`write_output`] will leave it, created if it does not
/// exist yet ([`resolve_dir_as_created`]), and its name is looked up there. An existing
/// file is canonicalized, which respells its name the way the volume stores it — so
/// `PAGE.md` and `page.md` are one file on a case-insensitive volume (#408), in a
/// directory reached back out of one the write creates (`newdir/../PAGE.md`) as much as
/// in one that exists. A name that does not exist yet, or is a symlink, is kept as it
/// stands: the symlink itself is the directory entry a write would replace, not the
/// file it points to.
fn file_identity(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let file = resolve_dir_as_created(effective_parent(path))?.join(name);
    let is_link = std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        if let Ok(canonical) = file.canonicalize() {
            return Some(canonical);
        }
    }
    Some(file)
}

/// The canonical path `dir` has once [`write_output`] has created it — the write creates
/// an output's directory, its anchor, with `create_dir_all` (#160) — found without
/// creating anything; `None` when not even the working directory
/// resolves, which a write of `dir` could not get past either.
///
/// An existing `dir` is canonicalized. Otherwise it is walked component by component, as
/// the system resolves it while creating it: an existing component is canonicalized —
/// a symlink is followed — and the first missing one is created as a plain directory,
/// so below it nothing exists yet: a name is appended as it stands, and a `..` leads back
/// to the parent, where the next name is looked up on disk again. So `newdir/..` is the
/// directory `newdir` would be created in, and `newdir/../lnk` follows `lnk`. The walk
/// visits each component of `dir` once, and a `..` never climbs above the root.
///
/// A relative `dir` is anchored at the working directory with [`std::path::absolute`].
/// On Windows that also collapses `..` lexically, which is how every Win32 file call
/// reads a path, so the walk and the write agree there too.
///
/// `mds watch` resolves its out-dir this way before each write, to tell the directory
/// the session started with from one the path now leads to (#160).
pub(crate) fn resolve_dir_as_created(dir: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if let Ok(canonical) = dir.canonicalize() {
        return Some(canonical);
    }
    let absolute = std::path::absolute(dir).ok()?;
    let mut resolved = PathBuf::new();
    // How many trailing components of `resolved` the write creates.
    let mut created: usize = 0;
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
                created = created.saturating_sub(1);
            }
            Component::Normal(name) => {
                let next = resolved.join(name);
                if created > 0 {
                    resolved = next;
                    created += 1;
                } else if let Ok(existing) = next.canonicalize() {
                    resolved = existing;
                } else {
                    resolved = next;
                    created = 1;
                }
            }
        }
    }
    Some(resolved)
}

/// Refuse to write the compiled entry over the entry file itself (#425): `mds::io`,
/// exit 2, naming the entry as `typed`, escaped.
///
/// A `.md` entry that declares `type: mds` compiles to Markdown, whose default output
/// name — the entry's stem plus `.md` — is the entry's own name; `--out-dir` or
/// `mds.json` `build.output_dir` naming the entry's directory, and `-o` naming the
/// entry itself (any extension), land on it too. Writing would replace the source with
/// its compiled form, which no longer declares `type: mds`, so the next build fails.
///
/// `output` and `entry.canonical` are compared as the files they name
/// ([`file_identity`]), canonical with canonical, never a path with a spelling of it
/// (#408) — an output whose directory does not exist yet as the write will create it, so
/// `newdir/../page.md` is `page.md`. `None` (stdout) is never the entry. It runs only
/// inside [`admit_output`], after the output path is resolved and before anything is
/// written.
fn refuse_output_over_entry(output: Option<&Path>, entry: EntryPaths<'_>) -> Result<(), MdsError> {
    let Some(output) = output else {
        return Ok(());
    };
    match (file_identity(output), file_identity(entry.canonical)) {
        (Some(output), Some(canonical)) if output == canonical => Err(MdsError::Io {
            message: format!(
                "output would overwrite the entry file: \"{}\"; \
                 write it elsewhere with -o <file> or --out-dir <dir>",
                mds::escape_path_for_message(&entry.typed.to_string_lossy())
            ),
        }),
        _ => Ok(()),
    }
}

/// Admit `output_path` as the destination of the compiled `entry`: refuse it when it is
/// the entry file itself ([`refuse_output_over_entry`], #425), and only then warn when
/// an explicit `-o` (`output_arg`) contradicts `kind` ([`warn_output_extension_mismatch`])
/// — the warning announces the write, so a refused output never gets one.
///
/// Every route to a compiled entry's output goes through it: `mds build` file mode,
/// `mds watch`'s startup compile, its startup fallback and every rebuild. A rebuild
/// passes no `output_arg`: the warning is a startup message, printed once for the path
/// every rebuild reuses.
pub(crate) fn admit_output(
    output_path: Option<&Path>,
    entry: EntryPaths<'_>,
    output_arg: &Option<String>,
    kind: OutputKind,
    quiet: bool,
) -> Result<(), MdsError> {
    refuse_output_over_entry(output_path, entry)?;
    warn_output_extension_mismatch(output_arg, kind, quiet);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #425: `admit_output` refuses an output that is the entry file and names the entry
    /// by `typed`, never by `canonical`; any other output is admitted, stdout included.
    /// Every write site — `mds build`, `mds watch`'s startup compile, its startup
    /// fallback and every rebuild — passes the entry as one `EntryPaths`, so none of them
    /// can hand the two forms over swapped.
    #[test]
    fn admit_output_refuses_naming_the_entry_as_typed() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("page.md"), "x").unwrap();
        let typed = dir.path().join("sub").join("..").join("page.md");
        let canonical = dir.path().join("page.md").canonicalize().unwrap();
        let entry = EntryPaths {
            typed: &typed,
            canonical: &canonical,
        };
        let admit = |output: Option<&Path>| {
            admit_output(output, entry, &None, OutputKind::Markdown, true)
                .map_err(|e| e.to_string())
        };

        let refused = admit(Some(&canonical)).unwrap_err();
        assert_eq!(
            refused,
            format!(
                "output would overwrite the entry file: \"{}\"; \
                 write it elsewhere with -o <file> or --out-dir <dir>",
                typed.display()
            )
        );
        assert!(
            !refused.contains(&*canonical.to_string_lossy()),
            "{refused}"
        );
        assert_eq!(
            admit(Some(&dir.path().join("sub").join("..").join("page.md"))),
            Err(refused),
            "another spelling of the entry is the entry"
        );

        assert_eq!(admit(Some(&dir.path().join("out.md"))), Ok(()), "control");
        assert_eq!(admit(None), Ok(()), "stdout is never the entry");
    }

    /// #425: an output whose directory does not exist yet is the file the write will
    /// reach once `write_output` has created that directory. A created directory is a
    /// plain one, so a `..` after it leads back to its parent — the entry's directory
    /// here — while an existing component is followed as the write follows it, a
    /// symlink included. The name is then looked up in the directory reached, so on a
    /// case-insensitive volume a case variant of the entry's name is the entry there
    /// too, however that directory was reached. Nothing is created by asking.
    #[test]
    fn file_identity_resolves_a_directory_the_write_creates() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("page.md"), "x").unwrap();
        let page = root.join("page.md");
        let case_insensitive = root.join("PAGE.md").exists();
        // A case variant is the entry on a case-insensitive volume, another file on a
        // case-sensitive one: each volume holds the variant to its own answer.
        let variant = if case_insensitive {
            page.clone()
        } else {
            root.join("PAGE.md")
        };

        let rows = [
            ("page.md", page.clone()),
            ("sub/../page.md", page.clone()),
            ("newdir/../page.md", page.clone()),
            ("a/b/../../page.md", page.clone()),
            ("sub/new/../../page.md", page.clone()),
            ("new/./x/../../page.md", page.clone()),
            ("newdir/page.md", root.join("newdir").join("page.md")),
            (
                "sub/new/page.md",
                root.join("sub").join("new").join("page.md"),
            ),
            ("PAGE.md", variant.clone()),
            ("newdir/../PAGE.md", variant.clone()),
            ("a/b/../../PAGE.md", variant),
        ];
        // `dir.path()` is not canonical on macOS (`/var` → `/private/var`), so the
        // existing part of each path is canonicalized, not merely joined.
        let mismatches: Vec<String> = rows
            .iter()
            .filter_map(|(rel, expected)| {
                let got = file_identity(&dir.path().join(rel));
                (got.as_ref() != Some(expected)).then(|| format!("{rel}: {got:?}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "{mismatches:#?}");

        #[cfg(unix)]
        {
            // Out of a directory the write creates, then through an existing link that
            // leads back to the entry's directory: the link is followed.
            std::os::unix::fs::symlink(&root, root.join("lnk")).unwrap();
            assert_eq!(
                file_identity(&dir.path().join("newdir/../lnk/page.md")),
                Some(page.clone())
            );
        }
        assert!(!root.join("newdir").exists() && !root.join("a").exists());
    }
}
