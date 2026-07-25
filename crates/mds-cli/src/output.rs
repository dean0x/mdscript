//! Shared output-path machinery for build, check, watch, fmt, and lint subcommands.
//!
//! # What lives here
//!
//! - [`OutputBase`] / [`resolve_output_base`] / [`output_path_for`]: directory-mode
//!   path resolution used by watch and build-directory.
//! - [`collect_mds_files`] / [`is_partial`]: directory traversal helpers.
//! - [`probe_and_remove_stale`]: stale-output cleanup for format-flip (AC-FUNC-23).
//! - [`eprint_error`]: sanitized stderr render for directory-mode error loops (PF-004).
//! - [`atomic_write_file`]: temp-file-then-rename writer shared by `fmt` and `lint --fix`.
//! - [`preview_text_for`]: `--diff`/`--check` preview output — neutralized on TTY,
//!   byte-faithful when piped, so redirected diffs stay applicable by `patch`/tooling.
//!
//! Single-file path helpers (`OutputKind`, `compile_to_content`, `compile_and_write`,
//! `resolve_output_path_for_kind`) remain in `build.rs`; they are imported here when
//! callers need both single-file and directory logic.

use std::borrow::Cow;
use std::io::Write as _;
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

// ── Atomic file write ─────────────────────────────────────────────────────────

/// Write `content` to `path` atomically via a temp-file-then-rename cycle.
///
/// Centralising this helper in `output.rs` ensures both `fmt` and `lint --fix`
/// route through the same write path (avoids PF-004 — a check enforced on the
/// primary path silently absent on a sibling path).
///
/// Safety properties:
/// - Re-checks for symlink immediately before the write (TOCTOU guard, AC-F-21).
/// - Temp file lives in the SAME directory as the target so the rename is
///   always intra-filesystem (atomic on POSIX, near-atomic on Windows).
/// - On Unix, captures and restores the original file mode (masked to `& 0o7777`
///   to strip filesystem-type bits before passing to `Permissions::from_mode`).
///   `tempfile::Builder` defaults to mode 0600; without this step a 0644 source
///   file would silently become owner-only after the rename.
/// - Calls `sync_all()` (not `flush()` — `flush()` is a no-op on unbuffered
///   `File`) for crash durability before the rename.
pub(crate) fn atomic_write_file(path: &Path, content: &str) -> Result<()> {
    use mds::{effective_parent, NativeFs};

    // Re-check for symlink right before writing (TOCTOU guard).
    NativeFs::check_symlink(path)
        .map_err(|e| miette::miette!("cannot write {}: {e}", path.display()))?;

    // effective_parent maps "" (bare filename) and None to "." — avoids PF-006.
    let parent = effective_parent(path);

    // Capture original permissions before creating the temp file.
    #[cfg(unix)]
    let original_mode: Option<u32> = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).map(|m| m.permissions().mode()).ok()
    };

    // Temp file in same directory so rename is always intra-filesystem.
    let mut tmp = tempfile::Builder::new()
        .prefix(".mds-tmp-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(|e| miette::miette!("cannot create temp file for {}: {e}", path.display()))?;

    // Restore original permissions before writing; mask off file-type bits
    // (high bits of st_mode) so only the permission bits reach from_mode.
    #[cfg(unix)]
    if let Some(mode) = original_mode {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|e| {
                miette::miette!(
                    "cannot set permissions on temp file for {}: {e}",
                    path.display()
                )
            })?;
    }

    tmp.write_all(content.as_bytes())
        .map_err(|e| miette::miette!("cannot write {}: {e}", path.display()))?;

    // sync_all() flushes data + metadata to storage (flush() is a no-op on
    // unbuffered File and provides no crash durability guarantee).
    tmp.as_file()
        .sync_all()
        .map_err(|e| miette::miette!("cannot fsync {}: {e}", path.display()))?;

    // persist() atomically renames the temp file to the target path.
    tmp.persist(path)
        .map_err(|e| miette::miette!("cannot rename temp file to {}: {e}", path.display()))?;

    Ok(())
}

// ── Sanitized stderr render ───────────────────────────────────────────────────

/// Re-export: byte-length-preserving source neutralization before miette rendering.
///
/// Lives in `mds-core` so both `MdsError::at()` (compiler error path) and
/// `render_diag_human` (lint diagnostic path) use a single canonical implementation
/// (avoids PF-004 / PF-014 parallel-path drift).
pub(crate) use mds::neutralize_source_for_render;

/// Render a miette `Report` to a `String` — the pure transformation extracted from
/// [`eprint_error`] so it can be tested without touching stderr.
///
/// Under the input-sanitizing design (PF-014), this function does NOT apply
/// `sanitize_control_chars` to the rendered frame — that would escape miette's own
/// ANSI SGR colour codes on any colour-capable TTY.  Hostile control characters must
/// be neutralized from the Report's *inputs* (message, help, and source text) before
/// this function is called.
///
/// Note: idempotency is a property of [`mds::sanitize_control_chars`] (calling it
/// twice on already-sanitized input is a no-op), not of this function (each call
/// re-renders the `Report` from scratch).
fn render_error_sanitized(report: &miette::Report) -> String {
    format!("{report:?}")
}

/// Render a miette `Report` to stderr — the single choke-point for all CLI error
/// output.
///
/// All per-file error handlers in `main`, `build`, `lint`, and `watch` route error
/// rendering through this helper, so there is exactly one site to audit for escape-
/// injection safety (architecture-6 / avoids PF-004: a check enforced on the primary
/// path silently absent on a sibling path).
///
/// Callers must pre-sanitize the Report's inputs before calling this function:
/// - **message / help**: via [`mds::sanitize_control_chars`]
/// - **source text**: via [`mds::neutralize_source_for_render`] (byte-length-preserving
///   so span byte-offsets remain valid after substitution)
/// - **filename**: via [`mds::sanitize_control_chars`]
///
/// miette's own ANSI SGR styling is passed through untouched — carets and box-drawing
/// survive intact.
///
/// Note: status-line path display (`Clean:`, `Fixed:`, etc.) is handled by the
/// separate [`safe_path`] helper, not by this function.
pub(crate) fn eprint_error(report: miette::Report) {
    eprintln!("{}", render_error_sanitized(&report));
}

/// Neutralize hostile control bytes in source text for `--diff`/`--check` preview output.
///
/// `--diff`/`--check` preview output: neutralized on TTY, byte-faithful when piped.
///
/// When `writer_is_tty` is `true`, returns [`neutralize_source_for_render`]`(text)` —
/// a byte-length-preserving substitution that maps C0/DEL/C1 controls and the widened
/// bidi/format hazard class (added in #176) to `?` or U+00A0/U+FFFD so hostile template
/// source cannot inject ANSI terminal commands into the rendered diff (CWE-150).
///
/// When `writer_is_tty` is `false`, returns `Cow::Borrowed(text)` unchanged so
/// redirected diff output (e.g. `mds fmt --diff > patch.diff`) stays byte-faithful
/// and applicable by `patch`/tooling.
///
/// This is a pure, allocation-free helper for clean inputs; the TTY detection
/// (`std::io::stdout().is_terminal()`) is performed at the call site so this function
/// is testable without a real TTY (avoids PF-014: sanitize renderer inputs, not the
/// rendered frame).
///
/// # Byte-length invariant
///
/// `neutralize_source_for_render` is byte-length-preserving — every substitution
/// produces a replacement of the same UTF-8 byte count — so diff hunk byte offsets
/// remain coherent after substitution on the TTY path. Never use
/// [`mds::sanitize_control_chars`] here: it expands 1–2-byte controls to 6 bytes,
/// desynchronising all offsets that follow.
///
/// # Boundary table entry (consistent with `crates/mds-core/src/lint/diagnostic.rs`)
///
/// | Boundary | Mode | Content |
/// |----------|------|---------|
/// | `--diff`/`--check` preview output | HUMAN, TTY-gated | source excerpts via `neutralize_source_for_render`; piped path returns `Cow::Borrowed` |
#[must_use]
pub(crate) fn preview_text_for(writer_is_tty: bool, text: &str) -> Cow<'_, str> {
    if writer_is_tty {
        neutralize_source_for_render(text)
    } else {
        Cow::Borrowed(text)
    }
}

/// Sanitize a filesystem path for terminal display (CWE-150 guard).
///
/// Converts the path to a display string and applies [`mds::sanitize_control_chars`]
/// so hostile filenames containing control sequences (e.g. `ESC[2J`) cannot inject
/// ANSI terminal commands into status lines.
///
/// All status-line path interpolations (`Clean:`, `Fixed:`, `Compiled to:`, etc.) in
/// `lint`, `fmt`, and `build` must route through this helper (avoids PF-004 /
/// security-5: unsanitized filename vector in status output).
pub(crate) fn safe_path(p: &std::path::Path) -> String {
    mds::sanitize_control_chars(&p.display().to_string()).into_owned()
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

    // ── preview_text_for: TTY-gated source neutralization (security-11) ──────────

    /// T-12a [security-11 / PF-013]: preview_text_for(true, hostile) neutralizes ESC
    /// and preserves byte length (so diff hunks remain coherent after substitution).
    ///
    /// Positive assertion required by PF-013: the neutralized form MUST be present,
    /// not merely asserted absent in the raw form.
    #[test]
    fn preview_text_for_tty_neutralizes_esc_preserves_byte_length() {
        let hostile = "hello\x1bworld"; // ESC (U+001B) is 1-byte C0
        let result = preview_text_for(true, hostile);
        // Byte-length invariant: neutralize_source_for_render is byte-length-preserving.
        assert_eq!(
            result.len(),
            hostile.len(),
            "byte length must be preserved on TTY path"
        );
        // ESC must be absent from TTY output (security gate).
        assert!(
            !result.contains('\x1b'),
            "raw ESC must be absent from TTY output; got: {result:?}"
        );
        // PF-013 positive assertion: the substituted '?' must be present.
        assert!(
            result.contains('?'),
            "ESC must be replaced with '?' on TTY path; got: {result:?}"
        );
    }

    /// T-12b [security-11 / PF-013]: preview_text_for(false, ...) returns Cow::Borrowed
    /// so piped diff output is byte-identical to the raw source (patch/tooling safety).
    #[test]
    fn preview_text_for_not_tty_returns_borrowed_passthrough() {
        let hostile = "hello\x1bworld";
        let result = preview_text_for(false, hostile);
        // Must be Cow::Borrowed — no allocation, byte-identical to input.
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "piped path must return Cow::Borrowed (no allocation); got Owned"
        );
        assert_eq!(
            result.as_ref(),
            hostile,
            "piped path must be byte-identical to input"
        );
    }

    /// T-12c [security-11 / PF-013]: preview_text_for(true, clean) returns unchanged content
    /// because neutralize_source_for_render returns Cow::Borrowed for clean inputs.
    #[test]
    fn preview_text_for_tty_clean_string_unchanged() {
        let clean = "hello world\n";
        let result = preview_text_for(true, clean);
        assert_eq!(
            result.as_ref(),
            clean,
            "clean string must be unchanged on TTY path"
        );
    }

    // ── safe_path: CWE-150 status-line guard (security-5/6) ──────────────────────

    /// T-11a [security-5 / PF-013]: safe_path escapes a raw ESC byte in a filename
    /// to the 6-char \\uXXXX literal so it cannot inject ANSI into a status line.
    #[test]
    fn safe_path_sanitizes_esc_byte() {
        let raw = format!("dir/fo{}o.mds", '\x1b');
        let p = std::path::Path::new(&raw);
        let result = safe_path(p);
        assert!(
            !result.contains('\x1b'),
            "raw ESC must be absent from safe_path output"
        );
        assert!(
            result.contains("\\u001B"),
            "ESC must be escaped to \\u001B; got: {result:?}"
        );
    }

    /// T-11b [security-5 / PF-013]: safe_path passes a clean path through unchanged
    /// (no unnecessary allocation or mutation).
    #[test]
    fn safe_path_passes_clean_path_unchanged() {
        let p = std::path::Path::new("dir/normal.mds");
        assert_eq!(safe_path(p), "dir/normal.mds");
    }

    // ── T-10a/b/c: neutralize_source_for_render + colour path (── PF-014) ────────

    /// T-10a [PF-014 / AC-F2]: neutralize_source_for_render removes C0 controls
    /// (except \n/\t), DEL, and 2-byte C1 controls while preserving total byte length
    /// so that miette span offsets remain valid after substitution.
    #[test]
    fn neutralize_source_removes_c0_del_c1_preserving_byte_length() {
        // ASCII NUL (C0), DEL (0x7F), and U+0085 NEL (C1, 2-byte UTF-8) are hostile.
        // \n and \t are allowed through unchanged.
        let raw = "a\x00b\x7fc\u{0085}d\ne\tf";
        let out = neutralize_source_for_render(raw);
        // Byte length must be identical (span safety invariant).
        assert_eq!(out.len(), raw.len(), "byte length preserved");
        // Hostile bytes are replaced; safe bytes survive.
        assert!(!out.contains('\x00'), "NUL removed");
        assert!(!out.contains('\x7f'), "DEL removed");
        assert!(!out.contains('\u{0085}'), "C1 NEL removed");
        assert!(out.contains('\n'), "LF preserved");
        assert!(out.contains('\t'), "TAB preserved");
    }

    /// T-10b [reliability-8 / PF-014]: When the span starts AFTER a control character
    /// the substituted byte at that position must still form a valid char boundary so
    /// miette can slice the excerpt without panicking.
    #[test]
    fn neutralize_source_caret_alignment_with_span_after_control_char() {
        use miette::{GraphicalReportHandler, GraphicalTheme, NamedSource, SourceSpan};

        // "a<ESC>bc" where span covers 'b' (byte offset 2..3, AFTER the ESC byte).
        // If neutralization breaks the byte-length invariant, miette panics here.
        let raw = "a\x1bbc";
        let clean = neutralize_source_for_render(raw);
        assert_eq!(clean.len(), raw.len(), "byte length invariant");

        // Build a minimal miette report whose source excerpt exercises the span.
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("test")]
        struct SpanErr {
            #[source_code]
            src: NamedSource<String>,
            #[label("here")]
            span: SourceSpan,
        }

        let report = miette::Report::new(SpanErr {
            src: NamedSource::new("test.mds", clean.into_owned()),
            span: (2, 1).into(), // byte 2..3 = 'b'
        });

        let mut buf = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .render_report(&mut buf, report.as_ref())
            .expect("render must not panic after neutralization");
        // The caret must point at 'b', not produce garbage.
        assert!(buf.contains('b'), "caret points at b");
        // No raw ESC byte survives into the rendered output.
        assert!(!buf.contains('\x1b'), "no raw ESC in rendered output");
    }

    /// T-10c [testing-2 / PF-014]: miette's own SGR colour codes survive
    /// (render_error_sanitized never post-processes the frame) while a hostile
    /// OSC sequence embedded in the source is neutralised at the input stage.
    #[test]
    fn colour_path_miette_sgr_survives_hostile_osc_is_removed() {
        use miette::{GraphicalReportHandler, GraphicalTheme, NamedSource, SourceSpan};

        // A 2-byte C1 U+009D = OSC opener (hostile). After neutralize it becomes
        // U+00A0 NBSP, same byte length; no OSC survives into the render.
        // U+009D in UTF-8 is 0xC2 0x9D (2 bytes). We use the char directly.
        let hostile_char = '\u{009D}'; // C1 OSC opener
        let raw = format!("good{}text", hostile_char);
        let clean = neutralize_source_for_render(&raw);
        assert_eq!(clean.len(), raw.len(), "byte length preserved");
        assert!(
            !clean.contains(hostile_char),
            "C1 OSC neutralised in source"
        );

        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("colour test")]
        struct ColourErr {
            #[source_code]
            src: NamedSource<String>,
            #[label("here")]
            span: SourceSpan,
        }

        let src_len = clean.len();
        let report = miette::Report::new(ColourErr {
            src: NamedSource::new("colour.mds", clean.into_owned()),
            span: (0, src_len).into(),
        });

        // Colour-enabled renderer — miette will emit real SGR codes.
        let mut coloured = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode())
            .render_report(&mut coloured, report.as_ref())
            .expect("render must not panic");

        // miette's own ANSI codes must survive in the coloured output.
        assert!(
            coloured.contains('\x1b'),
            "miette SGR codes present in coloured output"
        );
        // But the hostile C1 byte is gone from the rendered string.
        assert!(
            !coloured.contains(hostile_char),
            "hostile C1 absent from rendered output"
        );
    }
}
