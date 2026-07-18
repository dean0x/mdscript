//! Shared output-path machinery for build, check, and watch subcommands.
//!
//! # What lives here
//!
//! - [`OutputBase`] / [`resolve_output_base`] / [`output_path_for`]: directory-mode
//!   path resolution used by watch and build-directory.
//! - [`collect_mds_files`] / [`is_partial`]: directory traversal helpers.
//! - [`probe_and_remove_stale`]: stale-output cleanup for format-flip (AC-FUNC-23).
//!
//! Single-file path helpers (`OutputKind`, `compile_to_content`, `compile_and_write`,
//! `resolve_output_path_for_kind`) remain in `build.rs`; they are imported here when
//! callers need both single-file and directory logic.

use std::path::{Path, PathBuf};

use miette::Result;

use crate::build::{MdsConfig, OutputKind};

// ── Output base for directory mode ────────────────────────────────────────────

/// Describes where directory-mode output files are written.
///
/// `Dir(base)` mirrors the source subtree under `base`:
///   `source.strip_prefix(root)` → `base/rel/stem.<ext>`
/// `NextToSource` places the output next to the source file.
#[derive(Debug, Clone)]
pub(crate) enum OutputBase {
    Dir(PathBuf),
    NextToSource,
}

/// Resolve `out_dir` to an absolute, canonicalized path for reliable `starts_with` checks.
///
/// Used by both `run_build_directory` and `dir_watch_startup` before calling
/// [`resolve_output_base`]. Relative paths are resolved against `current_dir`; the result
/// is then canonicalized (falls back to the absolute form when the directory does not yet exist).
pub(crate) fn canonicalize_out_dir(out_dir: Option<&PathBuf>) -> Option<PathBuf> {
    out_dir.map(|d| {
        let abs = if d.is_absolute() {
            d.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(d)
        };
        abs.canonicalize().unwrap_or(abs)
    })
}

/// Compute the `OutputBase` for directory mode.
///
/// Precedence (mirrors `resolve_output_path` for file mode):
/// 1. `--out-dir` → `Dir(abs_out_dir)`
/// 2. `mds.json build.output_dir` → `Dir(config_dir.join(output_dir))`
///    — rejects `..` components at startup with a hard error.
/// 3. Default → `NextToSource`
pub(crate) fn resolve_output_base(
    abs_out_dir: Option<&Path>,
    config: &Option<(MdsConfig, PathBuf)>,
) -> Result<OutputBase> {
    if let Some(d) = abs_out_dir {
        return Ok(OutputBase::Dir(d.to_path_buf()));
    }
    if let Some((cfg, config_dir)) = config {
        if let Some(ref output_dir) = cfg.build.output_dir {
            let traversal = Path::new(output_dir)
                .components()
                .any(|c| c == std::path::Component::ParentDir);
            if traversal {
                return Err(miette::miette!(
                    "mds.json output_dir '{}' must not contain '..' components",
                    output_dir
                ));
            }
            return Ok(OutputBase::Dir(config_dir.join(output_dir)));
        }
    }
    Ok(OutputBase::NextToSource)
}

/// Compute the mirrored output path for a source file in directory mode.
///
/// Infallible — no directory creation.
///
/// Defined in terms of [`output_base_no_ext`] so the strip_prefix / AC-M7
/// path-escape logic is kept in one place (issue 5 — single source of truth).
///
/// - `Dir(base)`: mirrors `source` relative to `root` under `base`.
///   If `strip_prefix` fails (source not under root after canonicalization),
///   falls back to `base/stem.<ext>` — **never** joins an absolute path that
///   could escape the output directory (AC-M7 path-escape guard).
/// - `NextToSource`: `source.with_extension(ext)`.
///
/// The `ext` parameter is the output extension without leading `.` (`"md"` or `"json"`).
pub(crate) fn output_path_for(source: &Path, root: &Path, base: &OutputBase, ext: &str) -> PathBuf {
    let no_ext = output_base_no_ext(source, root, base);
    match base {
        OutputBase::Dir(d) => {
            let mut name = no_ext
                .file_name()
                .unwrap_or(source.as_os_str())
                .to_os_string();
            name.push(".");
            name.push(ext);
            let out = no_ext.parent().unwrap_or(d.as_path()).join(&name);
            // AC-M7 containment invariant: the output path must remain inside the out-dir.
            // `output_base_no_ext` already guards the strip_prefix escape case by returning
            // `d/<stem>` for out-of-root sources; the with-extension step cannot escape.
            // The check here is a defence-in-depth belt-and-suspenders assertion.
            if out.starts_with(d) {
                out
            } else {
                debug_assert!(
                    false,
                    "output_path_for: AC-M7 violated — output {out:?} escaped out-dir {d:?}"
                );
                let flat_name = {
                    let mut n = source
                        .file_stem()
                        .unwrap_or(source.as_os_str())
                        .to_os_string();
                    n.push(".");
                    n.push(ext);
                    n
                };
                d.join(flat_name)
            }
        }
        OutputBase::NextToSource => no_ext.with_extension(ext),
    }
}

// ── Directory traversal ───────────────────────────────────────────────────────

/// Result of a directory walk, carrying both the collected files and a count
/// of `.mds` files that were skipped because they reside inside
/// default-excluded directories (hidden dirs, `node_modules`).
///
/// A non-zero `excluded_by_default` with an empty `files` list means every
/// candidate was filtered out by the default exclusions — distinguishable from
/// a genuinely empty tree (where both are zero).
pub(crate) struct WalkResult {
    /// Files that were collected and are eligible for processing.
    pub files: Vec<PathBuf>,
    /// Count of `.mds` files found inside default-excluded directories.
    pub excluded_by_default: usize,
}

/// Recursively collect all `.mds` files under `root`, bounded by `max_depth`,
/// returning a [`WalkResult`] that also carries the count of files skipped due
/// to default exclusions (hidden dirs, `node_modules`).
///
/// Use this at call sites that need to distinguish "genuinely empty tree" from
/// "all candidates excluded". Use [`collect_mds_files`] at call sites (e.g.
/// watch) that only need the file list.
pub(crate) fn collect_mds_files_detailed(
    root: &Path,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
) -> WalkResult {
    let mut files = Vec::new();
    let mut excluded_by_default = 0;
    collect_mds_files_inner(
        root,
        0,
        max_depth,
        exclude_prefix,
        &mut files,
        &mut excluded_by_default,
    );
    WalkResult {
        files,
        excluded_by_default,
    }
}

/// Recursively collect all `.mds` files under `root`, bounded by `max_depth`.
///
/// Symlinked directories AND symlinked files are skipped to avoid cycles and
/// to maintain build parity with the single-file symlink guard (PF-004 / commit aa0c538).
/// When `exclude_prefix` is `Some(p)`, any path that starts with `p` is skipped
/// (used to exclude the out-dir when it is inside the watched root).
///
/// For callers that need to distinguish "genuinely empty tree" from "all candidates
/// excluded", use [`collect_mds_files_detailed`] instead.
pub(crate) fn collect_mds_files(
    root: &Path,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
) -> Vec<PathBuf> {
    collect_mds_files_detailed(root, max_depth, exclude_prefix).files
}

/// Return `true` when a directory name should be excluded from recursive
/// traversal by default (PF-004: enforced on the shared walker so ALL
/// subcommands — build / check / lint / fmt / watch — inherit the behaviour).
///
/// Excluded directory names:
/// - Any name that starts with `.` (hidden directories, e.g. `.git`, `.cache`)
/// - `node_modules`
///
/// Note: this gate applies to the RECURSION step only — the root directory
/// that was explicitly passed to `collect_mds_files` is always processed,
/// even if its own name happens to start with `.`.  Hidden *files* (e.g.
/// `.dotfile.mds`) at the traversed directory level are still collected.
pub(crate) fn is_default_excluded_dir(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

/// Return `true` when `path` lives inside a default-excluded sub-directory
/// of `root` (i.e. traversal would have been skipped there by
/// `is_default_excluded_dir`).
///
/// Used by the watch guards to detect events that should be treated as external
/// dependencies rather than normal output-producing sources (PF-004 class:
/// the same limit must be enforced on the parallel event-processing path as on
/// the initial walker path — avoids the "limit on one path but not another"
/// bug class).
pub(crate) fn is_within_default_excluded_dir(root: &Path, path: &Path) -> bool {
    // Strip the root prefix to get a relative path, then walk the ancestor
    // chain using Path::parent() — avoids allocating a Vec<Component> just to
    // drop the final component (issue #69: this runs on the watch per-event
    // hot path).
    //
    // Edge case: when `rel` is a single component (e.g. "foo.mds"),
    // rel.parent() returns Some("") whose file_name() is None, and
    // "".parent() returns None, ending the loop correctly.
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false, // path is not under root at all
    };
    let mut ancestor = rel.parent();
    while let Some(dir) = ancestor {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            if is_default_excluded_dir(name) {
                return true;
            }
        }
        ancestor = dir.parent();
    }
    false
}

fn collect_mds_files_inner(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
    results: &mut Vec<PathBuf>,
    excluded_count: &mut usize,
) {
    if depth > max_depth {
        eprintln!(
            "warning: directory depth limit ({max_depth}) reached at {}; \
             deeper files will not be processed",
            dir.display()
        );
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();

        // Skip the output directory when it is nested inside the root.
        if let Some(excl) = exclude_prefix {
            if path.starts_with(excl) {
                continue;
            }
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            // Symlinked dirs AND symlinked files are skipped (PF-004 / build parity).
            // This preserves the same guard as single-file mode where symlinked entries
            // are rejected at startup (commit aa0c538).
            continue;
        }
        if file_type.is_dir() {
            // Skip hidden directories (e.g. .git, .cache) and node_modules on the
            // RECURSION step so all subcommands inherit the default exclusions via
            // the shared walker (PF-004).  The root dir that was explicitly passed
            // to collect_mds_files() is NEVER checked here — this guard applies
            // only to directory ENTRIES discovered during traversal.  Since entries
            // are always children of the current dir, they are never the explicit root.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if is_default_excluded_dir(name) {
                    // Count the .mds files we're skipping so callers can emit a
                    // meaningful diagnostic when all candidates are excluded.
                    count_mds_in_excluded_dir(&path, depth + 1, max_depth, excluded_count);
                    continue;
                }
            }
            collect_mds_files_inner(
                &path,
                depth + 1,
                max_depth,
                exclude_prefix,
                results,
                excluded_count,
            );
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds") {
            results.push(path);
        }
    }
}

/// Count `.mds` files inside a directory that is being skipped due to a
/// default exclusion. Symlinks are still skipped. Does not apply further
/// exclusion filtering — we are already inside an excluded root, so every
/// `.mds` descendant is a skipped candidate regardless of name.
fn count_mds_in_excluded_dir(dir: &Path, depth: usize, max_depth: usize, count: &mut usize) {
    if depth > max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            count_mds_in_excluded_dir(&path, depth + 1, max_depth, count);
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds") {
            *count += 1;
        }
    }
}

/// Return `true` if `path`'s file name starts with `_` (partial convention, DD2).
pub(crate) fn is_partial(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.starts_with('_'))
        .unwrap_or(false)
}

// ── Stale-output cleanup ──────────────────────────────────────────────────────

/// Probe for BOTH possible output siblings and unlink the one that does NOT match `kind`.
///
/// Called after writing a compiled output to clean up a stale sibling from a previous
/// format flip (e.g. a file that used to emit `x.md` but now emits `x.json`).
///
/// If neither sibling exists the function is a no-op. If the wrong-extension file
/// exists it is deleted; errors are soft-warned (non-fatal: the stale file stays,
/// which is an annoyance, not a correctness issue).
///
/// `base_path` must be the path WITHOUT extension (e.g. `/out/foo` for a source
/// `foo.mds`). The function constructs `base_path.with_extension("md")` and
/// `base_path.with_extension("json")` and removes the one that contradicts `kind`.
///
/// AC-FUNC-23 (watch format-flip) and the equivalent dir-build stale-cleanup both
/// call this function so the probe-and-unlink logic is shared.
pub(crate) fn probe_and_remove_stale(base_no_ext: &Path, kind: OutputKind) {
    let stale_ext = kind.stale_extension();
    let stale_path = base_no_ext.with_extension(stale_ext);
    if stale_path.exists() {
        match std::fs::remove_file(&stale_path) {
            Ok(()) => {
                // non-loud: stale cleanup is a housekeeping detail, not an action the
                // user normally needs to know about (mirrors watch "Removed …" style).
            }
            Err(e) => {
                eprintln!(
                    "warning: could not remove stale output {}: {e}",
                    stale_path.display()
                );
            }
        }
    }
}

/// Return the path stem (path without extension) for a compiled source.
///
/// Used to construct the `base_no_ext` argument to [`probe_and_remove_stale`].
///
/// For `Dir(base)` mode this mirrors the same strip_prefix logic as [`output_path_for`]
/// so the stem is always computed consistently.
pub(crate) fn output_base_no_ext(source: &Path, root: &Path, base: &OutputBase) -> PathBuf {
    match base {
        OutputBase::Dir(d) => {
            let rel = match source.strip_prefix(root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => {
                    // Path-escape guard: use filename only (mirrors output_path_for).
                    let stem = source.file_stem().unwrap_or(source.as_os_str());
                    return d.join(stem);
                }
            };
            // Build the path with no extension.
            let stem = rel.file_stem().unwrap_or(rel.as_os_str()).to_os_string();
            d.join(rel.parent().unwrap_or(Path::new(""))).join(stem)
        }
        OutputBase::NextToSource => {
            // source.with_extension("") removes the existing extension.
            source.with_extension("")
        }
    }
}

// ── Sanitized stderr render ───────────────────────────────────────────────────

/// Render a miette Report to stderr with control-character sanitization applied.
///
/// Per-file error handlers in directory-mode loops (e.g. `lint_one_file_human`,
/// `format_one_file`) MUST use this helper instead of bare
/// `eprintln!("{:?}", miette::Report::from(e))`.  Centralising the render here
/// means the sanitizer cannot be forgotten on any future parallel path
/// (avoids PF-004: a check enforced on the primary path silently absent on a
/// sibling path).
///
/// The `mds::sanitize_control_chars` function strips C0, C1, and DEL codepoints
/// while preserving `\n`, `\t`, and printable Unicode — miette box-drawing and
/// carets therefore survive intact; only raw ESC bytes and other non-printing
/// controls are escaped to `\uXXXX` literals.
pub(crate) fn eprint_error(report: miette::Report) {
    eprintln!("{}", mds::sanitize_control_chars(&format!("{report:?}")));
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // T-CLI-21 (unit): output_path_for with "json" / "md" extensions.
    #[test]
    fn output_path_for_json_extension_dir_mode() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "json");
        assert_eq!(result, PathBuf::from("/out/src/chat.json"));
    }

    #[test]
    fn output_path_for_md_extension_dir_mode() {
        let source = PathBuf::from("/root/src/page.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, PathBuf::from("/out/src/page.md"));
    }

    #[test]
    fn output_path_for_next_to_source() {
        let source = PathBuf::from("/root/src/page.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::NextToSource;
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, PathBuf::from("/root/src/page.md"));
    }

    #[test]
    fn output_path_for_next_to_source_json() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::NextToSource;
        let result = output_path_for(&source, &root, &base, "json");
        assert_eq!(result, PathBuf::from("/root/src/chat.json"));
    }

    // T-CLI-21 (unit): ..‑containment guard (AC-M7) still holds.
    // When source is outside root, output must be `base/stem.ext`, not escaped.
    #[test]
    fn output_path_for_outside_root_falls_back_to_flat() {
        let source = PathBuf::from("/other/page.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        // Must be inside /out, not escape to /other.
        assert!(
            result.starts_with("/out"),
            "output must be inside /out; got {result:?}"
        );
        assert_eq!(result, PathBuf::from("/out/page.md"));
    }

    #[test]
    fn is_partial_detects_underscore_prefix() {
        assert!(is_partial(Path::new("/dir/_partial.mds")));
        assert!(!is_partial(Path::new("/dir/main.mds")));
        assert!(!is_partial(Path::new("/dir/not_partial.mds")));
    }

    // ── is_default_excluded_dir ───────────────────────────────────────────────

    #[test]
    fn hidden_dir_is_excluded() {
        assert!(is_default_excluded_dir(".git"));
        assert!(is_default_excluded_dir(".cache"));
        assert!(is_default_excluded_dir(".hidden"));
    }

    #[test]
    fn node_modules_is_excluded() {
        assert!(is_default_excluded_dir("node_modules"));
    }

    #[test]
    fn ordinary_dirs_are_not_excluded() {
        assert!(!is_default_excluded_dir("src"));
        assert!(!is_default_excluded_dir("prompts"));
        assert!(!is_default_excluded_dir("templates"));
    }

    // ── is_within_default_excluded_dir ───────────────────────────────────────

    #[test]
    fn path_inside_node_modules_is_excluded() {
        assert!(is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/node_modules/foo.mds")
        ));
    }

    #[test]
    fn path_inside_git_dir_is_excluded() {
        assert!(is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/.git/config")
        ));
    }

    #[test]
    fn path_inside_hidden_subdir_is_excluded() {
        assert!(is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/.cache/something.mds")
        ));
    }

    #[test]
    fn normal_path_under_root_is_not_excluded() {
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/src/main.mds")
        ));
    }

    #[test]
    fn hidden_file_at_root_level_is_not_excluded() {
        // Hidden files at the top level are not inside an excluded DIR.
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/.dotfile.mds")
        ));
    }

    #[test]
    fn path_outside_root_is_not_excluded() {
        // Paths not under root at all are not affected by the root-relative check.
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/other/node_modules/foo.mds")
        ));
    }

    // ── collect_mds_files walker exclusions ───────────────────────────────────

    #[test]
    fn walker_skips_node_modules_subdir() {
        let dir = tempfile::tempdir().unwrap();
        // Create a normal .mds file and one inside node_modules.
        std::fs::write(dir.path().join("main.mds"), "hello").unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("lib.mds"), "lib").unwrap();

        let files = collect_mds_files(dir.path(), 64, None);
        assert_eq!(
            files.len(),
            1,
            "node_modules/lib.mds should be excluded; found: {files:?}"
        );
        assert!(files[0].ends_with("main.mds"));
    }

    #[test]
    fn walker_skips_hidden_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.mds"), "hello").unwrap();
        let hidden = dir.path().join(".git");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("config.mds"), "not a real file").unwrap();

        let files = collect_mds_files(dir.path(), 64, None);
        assert_eq!(
            files.len(),
            1,
            ".git/*.mds should be excluded; found: {files:?}"
        );
    }

    #[test]
    fn walker_collects_hidden_file_at_root_level() {
        // Hidden FILES (not directories) at the traversed level are still collected.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.mds"), "hello").unwrap();
        std::fs::write(dir.path().join(".dotfile.mds"), "dot").unwrap();

        let mut files = collect_mds_files(dir.path(), 64, None);
        files.sort();
        assert_eq!(
            files.len(),
            2,
            "hidden file should still be collected; found: {files:?}"
        );
    }

    #[test]
    fn walker_processes_explicitly_passed_hidden_root() {
        // The root dir itself is always processed even if its name starts with '.'.
        let dir = tempfile::tempdir().unwrap();
        let hidden_root = dir.path().join(".myhidden");
        std::fs::create_dir(&hidden_root).unwrap();
        std::fs::write(hidden_root.join("template.mds"), "hello").unwrap();

        let files = collect_mds_files(&hidden_root, 64, None);
        assert_eq!(
            files.len(),
            1,
            "explicitly-passed hidden root should be processed; found: {files:?}"
        );
    }

    // ── collect_mds_files_detailed / WalkResult ───────────────────────────────

    #[test]
    fn walk_result_empty_dir_has_zero_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let result = collect_mds_files_detailed(dir.path(), 64, None);
        assert_eq!(result.files.len(), 0);
        assert_eq!(
            result.excluded_by_default, 0,
            "genuinely empty dir must have 0 excluded"
        );
    }

    #[test]
    fn walk_result_all_excluded_counts_skipped_files() {
        let dir = tempfile::tempdir().unwrap();
        // All files inside a hidden dir → excluded_by_default > 0, files empty.
        let hidden = dir.path().join(".prompts");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("a.mds"), "a").unwrap();
        std::fs::write(hidden.join("b.mds"), "b").unwrap();

        let result = collect_mds_files_detailed(dir.path(), 64, None);
        assert_eq!(result.files.len(), 0, "no files should be in results");
        assert_eq!(
            result.excluded_by_default, 2,
            "excluded_by_default must equal the count of skipped .mds files; got {}",
            result.excluded_by_default
        );
    }

    #[test]
    fn walk_result_mixed_counts_excluded_and_collects_normal() {
        let dir = tempfile::tempdir().unwrap();
        // One normal file + one in node_modules.
        std::fs::write(dir.path().join("normal.mds"), "hello").unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("excluded.mds"), "lib").unwrap();

        let result = collect_mds_files_detailed(dir.path(), 64, None);
        assert_eq!(result.files.len(), 1, "only normal.mds should be collected");
        assert_eq!(
            result.excluded_by_default, 1,
            "one file in node_modules should be counted as excluded"
        );
    }

    // ── is_within_default_excluded_dir single-component edge case ─────────────

    #[test]
    fn single_component_path_is_not_inside_excluded_dir() {
        // rel = "foo.mds" (single component): rel.parent() = Some(""), which has
        // no file_name(), so the loop terminates without false-positive.
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/foo.mds")
        ));
    }

    #[test]
    fn output_base_no_ext_dir_mode() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_base_no_ext(&source, &root, &base);
        assert_eq!(result, PathBuf::from("/out/src/chat"));
    }

    #[test]
    fn output_base_no_ext_next_to_source() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::NextToSource;
        let result = output_base_no_ext(&source, &root, &base);
        assert_eq!(result, PathBuf::from("/root/src/chat"));
    }
}
