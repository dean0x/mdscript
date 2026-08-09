//! Shared output-path machinery for build, check, watch, fmt, and lint subcommands.
//!
//! # What lives here
//!
//! - [`OutputBase`] / [`resolve_output_base`] / [`output_path_for`]: directory-mode
//!   path resolution used by watch and build-directory.
//! - [`collect_mds_files`] / [`is_partial`]: directory traversal helpers.
//! - [`probe_and_remove_stale`]: stale-output cleanup for format-flip (AC-FUNC-23).
//! - [`eprint_error`]: the single CLI stderr choke-point — escapes every report's
//!   message, help, and label text before miette renders it (CWE-150 / PF-014).
//! - [`atomic_write_file`]: temp-file-then-rename writer shared by `fmt` and `lint --fix`.
//! - [`preview_text_for`]: `--diff` preview output — neutralized on TTY, byte-faithful
//!   when piped, so redirected diffs stay applicable by `patch`/tooling.
//!
//! Single-file path helpers (`OutputKind`, `compile_to_content`, `compile_and_write`,
//! `resolve_output_path_for_kind`) remain in `build.rs`; they are imported here when
//! callers need both single-file and directory logic.

use std::borrow::Cow;
use std::io::{IsTerminal, Write as _};
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
        // The directory name is discovered by the walk — the user never types it — so it
        // is untrusted on every subcommand that shares this walker (build / check / fmt /
        // lint / watch).  Prose through `eprint_warning` (HUMAN), the path through
        // `safe_path` (WIRE): a directory name is never legitimately multi-line, and a
        // raw one here forged a standalone `Clean: …` line as well as emitting raw ESC.
        eprint_warning(&format!(
            "warning: directory depth limit ({max_depth}) reached at {}; \
             deeper files will not be processed",
            safe_path(dir)
        ));
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
                // Same shape as the depth-limit warning above: the path is walker-derived
                // and the `io::Error` Display embeds a path of its own, so both are WIRE.
                eprint_warning(&format!(
                    "warning: could not remove stale output {}: {}",
                    safe_path(&stale_path),
                    safe_inline(&e)
                ));
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
///
/// Note: Due to the temp-file-then-rename approach, this function does NOT
/// preserve hard links, ACLs, extended attributes (xattrs), or owner/group
/// metadata of the original file.
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
        match std::fs::metadata(path) {
            Ok(m) => Some(m.permissions().mode()),
            Err(e) => {
                eprint_error(miette::miette!(
                    "cannot get metadata for {}: {e}",
                    path.display()
                ));
                None
            }
        }
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

/// A terminal-safe view of a [`miette::Report`], built **before** rendering.
///
/// Overrides every prose surface the frame can render — the `Display` message, the
/// `help` text, each [`miette::LabeledSpan`]'s label, and the whole auxiliary
/// diagnostic graph (`source` cause chain, `related`, `diagnostic_source`) — with
/// [`mds::sanitize_control_chars`]-escaped copies (HUMAN mode, so `\n` and `\t` survive
/// and multi-line frames stay readable).  Everything the frame's geometry depends on —
/// `code`, `severity`, `url`, `source_code`, and each label's byte span — is delegated
/// to the inner report untouched, so the byte-length-preserving neutralization already
/// applied to source excerpts (via `mds::named_source_for_render`) keeps every span
/// offset and caret column exact.
///
/// # Why this is the PF-014-correct boundary
///
/// The rendered frame is never post-processed.  Running a sanitizer over an
/// already-rendered miette frame would escape miette's *own* ANSI SGR colour codes
/// into literal `\u001b[33m` noise on any colour-capable TTY — on completely benign
/// input — and desynchronise caret alignment.  CI cannot catch that regression
/// because the CLI tests pin `NO_COLOR=1` and pipe stderr.  Sanitizing the renderer's
/// *inputs* has neither failure mode.
///
/// # Why it wraps the whole `Report` rather than just `MdsError`
///
/// The CLI produces two error families: `MdsError` (compiler diagnostics) and
/// CLI-authored `miette::miette!()` reports, which do **not** downcast to `MdsError`
/// (see `build::exit_code`).  Both interpolate untrusted text — a template's include
/// alias in the first case, an `mds.json` value or a hostile filename in the second.
/// Wrapping at the `Report` level covers both, and keeps covering any error type added
/// later, so the guarantee holds by construction rather than by remembering to extend
/// a downcast ladder (avoids PF-004).
///
/// # The auxiliary graph (`source` / `related` / `diagnostic_source`)
///
/// A `Diagnostic` can hang three further diagnostic graphs off itself, all of which
/// miette renders: the `std::error::Error` cause chain, the `related()` siblings, and
/// `diagnostic_source()`.  All three carry prose, so all three must be escaped.
///
/// They cannot be forwarded by reference the way `code`/`severity`/`url` are: those
/// accessors return borrows of the *inner* report, so handing back a sanitized view
/// would require the wrapper to own it and hand out a borrow of itself.  This wrapper
/// therefore materialises the whole graph into owned [`SanitizedNode`]s at construction
/// (bounded by [`MAX_AUX_DEPTH`]) and forwards borrows of those.
///
/// Enforcing this with a `debug_assert!` that the graph is empty — which is what an
/// earlier revision did, on the grounds that no CLI error populates it today — is
/// exactly PF-005: `debug_assert!` is compiled out of release, so the invariant would
/// hold under test and be absent in the shipped binary.  The first error type to grow a
/// `#[source]` field would have silently dropped its cause chain from release stderr
/// while CI stayed green.
struct SanitizedReport {
    inner: miette::Report,
    message: String,
    help: Option<String>,
    source: Option<SanitizedNode>,
    related: Vec<SanitizedNode>,
    diagnostic_source: Option<SanitizedNode>,
}

/// Depth bound for materialising a report's cause / related / diagnostic-source graph.
///
/// Cause chains are finite in practice, but nothing in the `Error` trait forbids a cycle
/// (a `source()` that returns a sibling of itself would loop forever).  Rendering is not
/// a place to discover that, so the walk is explicitly bounded; a graph deeper than this
/// is truncated, which drops trailing context but never hangs or overflows the stack.
const MAX_AUX_DEPTH: usize = 16;

/// An owned, control-character-escaped snapshot of one node in a report's auxiliary
/// diagnostic graph.
///
/// Every prose surface (`message`, `help`, `code`, `url`, label text) is escaped with
/// HUMAN-mode [`mds::sanitize_control_chars`] when the node is built.  Label byte spans
/// are copied verbatim, exactly as [`SanitizedReport::labels`] does, so caret geometry
/// against the parent's already-neutralized source stays exact.
///
/// `source_code()` returns `None` by design: a `&dyn miette::SourceCode` cannot be
/// cloned out of the inner diagnostic, and miette falls back to the *parent* report's
/// source — which [`SanitizedReport::source_code`] forwards — when a nested diagnostic
/// supplies none.  So a nested diagnostic still renders against neutralized source.
struct SanitizedNode {
    message: String,
    help: Option<String>,
    code: Option<String>,
    url: Option<String>,
    severity: Option<miette::Severity>,
    labels: Option<Vec<miette::LabeledSpan>>,
    source: Option<Box<SanitizedNode>>,
    related: Vec<SanitizedNode>,
    diagnostic_source: Option<Box<SanitizedNode>>,
}

/// Escape one optional `Display` surface to an owned `String`.
fn escape_display(d: Option<Box<dyn std::fmt::Display + '_>>) -> Option<String> {
    d.map(|v| mds::sanitize_control_chars(&v.to_string()).into_owned())
}

/// Escape a `Diagnostic`'s label text, keeping each byte span verbatim.
///
/// `None` is preserved as `None` (rather than collapsed to an empty vector) so miette
/// distinguishes "no labels" from "labels, but none of them" exactly as it did before
/// the wrapper was introduced.
fn escape_labels(d: &dyn miette::Diagnostic) -> Option<Vec<miette::LabeledSpan>> {
    Some(
        d.labels()?
            .map(|label| {
                let text = label
                    .label()
                    .map(|t| mds::sanitize_control_chars(t).into_owned());
                let span = *label.inner();
                if label.primary() {
                    miette::LabeledSpan::new_primary_with_span(text, span)
                } else {
                    miette::LabeledSpan::new_with_span(text, span)
                }
            })
            .collect(),
    )
}

impl SanitizedNode {
    /// Build from a `Diagnostic` node (used for `related` / `diagnostic_source`).
    fn from_diagnostic(d: &dyn miette::Diagnostic, depth: usize) -> Self {
        Self {
            message: mds::sanitize_control_chars(&d.to_string()).into_owned(),
            help: escape_display(d.help()),
            code: escape_display(d.code()),
            url: escape_display(d.url()),
            severity: d.severity(),
            labels: escape_labels(d),
            source: Self::chain_from_error(std::error::Error::source(d), depth),
            related: Self::related_from(d, depth),
            diagnostic_source: Self::boxed_from_diagnostic(d.diagnostic_source(), depth),
        }
    }

    /// Build from a plain `Error` node (used for the `source()` cause chain, whose links
    /// expose no `Diagnostic` data — only a `Display` message and a further `source()`).
    fn from_error(e: &(dyn std::error::Error + 'static), depth: usize) -> Self {
        Self {
            message: mds::sanitize_control_chars(&e.to_string()).into_owned(),
            help: None,
            code: None,
            url: None,
            severity: None,
            labels: None,
            source: Self::chain_from_error(e.source(), depth),
            related: Vec::new(),
            diagnostic_source: None,
        }
    }

    fn chain_from_error(
        e: Option<&(dyn std::error::Error + 'static)>,
        depth: usize,
    ) -> Option<Box<SanitizedNode>> {
        if depth >= MAX_AUX_DEPTH {
            return None;
        }
        e.map(|e| Box::new(Self::from_error(e, depth + 1)))
    }

    fn boxed_from_diagnostic(
        d: Option<&dyn miette::Diagnostic>,
        depth: usize,
    ) -> Option<Box<SanitizedNode>> {
        if depth >= MAX_AUX_DEPTH {
            return None;
        }
        d.map(|d| Box::new(Self::from_diagnostic(d, depth + 1)))
    }

    fn related_from(d: &dyn miette::Diagnostic, depth: usize) -> Vec<SanitizedNode> {
        if depth >= MAX_AUX_DEPTH {
            return Vec::new();
        }
        d.related()
            .map(|rs| {
                rs.map(|r| SanitizedNode::from_diagnostic(r, depth + 1))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl std::fmt::Display for SanitizedNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Hand-written for the same reason as [`SanitizedReport`]'s: the derived `Debug` of a
/// miette type can be its full graphical render, which would bypass the escaping.
impl std::fmt::Debug for SanitizedNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SanitizedNode")
            .field("message", &self.message)
            .field("help", &self.help)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for SanitizedNode {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|n| n as &(dyn std::error::Error + 'static))
    }
}

impl miette::Diagnostic for SanitizedNode {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.code.as_deref())
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.severity
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.help.as_deref())
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.url.as_deref())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(self.labels.as_ref()?.iter().cloned()))
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        related_iter(&self.related)
    }

    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.diagnostic_source
            .as_deref()
            .map(|n| n as &dyn miette::Diagnostic)
    }
}

/// Box an optional `&str` as the `Display` trait object miette's accessors return.
fn boxed_str(s: Option<&str>) -> Option<Box<dyn std::fmt::Display + '_>> {
    s.map(|s| -> Box<dyn std::fmt::Display + '_> { Box::new(s) })
}

/// Shared `related()` body: `None` when empty, so miette omits the section entirely.
fn related_iter(
    nodes: &[SanitizedNode],
) -> Option<Box<dyn Iterator<Item = &dyn miette::Diagnostic> + '_>> {
    if nodes.is_empty() {
        return None;
    }
    Some(Box::new(nodes.iter().map(|n| n as &dyn miette::Diagnostic)))
}

impl SanitizedReport {
    fn new(inner: miette::Report) -> Self {
        let message = mds::sanitize_control_chars(&inner.to_string()).into_owned();
        let help = escape_display(inner.help());
        let source = SanitizedNode::chain_from_error(std::error::Error::source(&*inner), 0)
            .map(|boxed| *boxed);
        let related = SanitizedNode::related_from(inner.as_ref(), 0);
        let diagnostic_source =
            SanitizedNode::boxed_from_diagnostic(inner.diagnostic_source(), 0).map(|boxed| *boxed);

        Self {
            inner,
            message,
            help,
            source,
            related,
            diagnostic_source,
        }
    }
}

impl std::fmt::Display for SanitizedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Hand-written so the wrapper never surfaces the inner report's own `Debug`, which
/// under miette's `fancy` feature is the full graphical render of the *unsanitized*
/// error.
impl std::fmt::Debug for SanitizedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SanitizedReport")
            .field("message", &self.message)
            .field("help", &self.help)
            .finish_non_exhaustive()
    }
}

/// Forwards the **sanitized** cause chain, never the inner report's own.
impl std::error::Error for SanitizedReport {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|n| n as &(dyn std::error::Error + 'static))
    }
}

impl miette::Diagnostic for SanitizedReport {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.code()
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.inner.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.help.as_deref())
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.url()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.inner.source_code()
    }

    /// Byte spans are forwarded verbatim (they index the neutralized, byte-length-
    /// preserved source); only the label *text* is escaped.  `LintDiagnostic` uses its
    /// own message as the label text, so this is a real untrusted-text surface.
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(escape_labels(self.inner.as_ref())?.into_iter()))
    }

    /// The **sanitized** related-diagnostic snapshots, not the inner report's.
    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        related_iter(&self.related)
    }

    /// The **sanitized** diagnostic source, not the inner report's.
    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.diagnostic_source
            .as_ref()
            .map(|n| n as &dyn miette::Diagnostic)
    }
}

/// Wrap `report` so every prose surface it renders is control-character-escaped.
///
/// This is the construction-boundary half of the PF-014 design: callers hand it a
/// `Report` built from raw values, and it produces one whose message, help, and label
/// text are safe to hand to miette.  See [`SanitizedReport`] for the full rationale.
fn sanitize_report(report: miette::Report) -> miette::Report {
    miette::Report::new(SanitizedReport::new(report))
}

/// Sanitize `report`'s inputs and render it to a `String` — the pure transformation
/// extracted from [`eprint_error`] so it can be tested without touching stderr.
///
/// The rendered frame itself is never post-processed (PF-014); all escaping happens
/// in [`sanitize_report`], before miette sees the values.
///
/// Note: idempotency is a property of [`mds::sanitize_control_chars`] (calling it
/// twice on already-sanitized input is a no-op), not of this function (each call
/// re-renders the `Report` from scratch).  That idempotency is what lets
/// `render_diag_human` keep sanitizing its own inputs — it must, because it also
/// neutralizes the source excerpt and filename, which this boundary cannot do.
fn render_error_sanitized(report: miette::Report) -> String {
    let report = sanitize_report(report);
    format!("{report:?}")
}

/// Render a miette `Report` to stderr — the single choke-point for all CLI error
/// output.
///
/// All per-file error handlers in `main`, `build`, `fmt`, `lint`, and `watch` route
/// error rendering through this helper, so there is exactly one site to audit for
/// escape-injection safety (architecture-6 / avoids PF-004: a check enforced on the
/// primary path silently absent on a sibling path).
///
/// **This function escapes the report's message, help, label text, and auxiliary
/// diagnostic graph itself**, via [`sanitize_report`], for every report it is given —
/// `MdsError` and CLI-authored `miette::miette!()` alike.  Callers do not need to
/// pre-sanitize prose.
///
/// What callers still owe, because this boundary cannot supply it: the
/// [`miette::NamedSource`] attached to the report, whose two halves need different
/// treatments (byte-length-preserving neutralization for the span-indexed source, WIRE
/// escaping for the single-line filename).  Build it with
/// [`mds::named_source_for_render`] — `MdsError::at()` (compiler path),
/// `check_equivalence` (formatter) and `render_diag_human` (lint path) all do.
///
/// miette's own ANSI SGR styling is passed through untouched — carets and box-drawing
/// survive intact.
///
/// Note: status-line path display (`Clean:`, `Fixed:`, etc.) is handled by the
/// separate [`safe_path`] helper, not by this function.
pub(crate) fn eprint_error(report: miette::Report) {
    eprintln!("{}", render_error_sanitized(report));
}

/// Print a CLI warning to stderr with HUMAN-mode escaping applied to the whole line
/// (CWE-150 / PF-004 / #176).
///
/// Applies [`mds::sanitize_control_chars`] (preserves `\n` and `\t`; escapes all other
/// C0 / DEL / C1 controls and bidi / separator / BOM characters to their six-character
/// `\uXXXX` literals) before printing, so a hostile warning message cannot inject ANSI
/// terminal commands into stderr.
///
/// # This helper alone is NOT sufficient — it is the prose half of the rule
///
/// HUMAN mode preserves `\n` on purpose, so that a multi-line warning body still renders
/// as multiple lines. That means routing a hostile *identifier* through this function
/// closes CWE-150 on it and leaves CWE-117 open: `lint.rs`'s unknown-`mds.json`-rule
/// warning already called this helper, and a rule name of
/// `x<LF>Clean: totally-real.mds<LF>0 problems found` still emitted three standalone
/// lines byte-identical in form to genuine status output.
///
/// The governing rule is per FIELD, not per surface (spec §7.5):
///
/// | Part of the line | Mode | Helper |
/// |------------------|------|--------|
/// | warning prose / body | HUMAN | this function |
/// | interpolated filename or path | WIRE | [`safe_path`] |
/// | interpolated identifier, config value, or error cause | WIRE | [`safe_inline`] |
///
/// So the correct shape is `eprint_warning(&format!("warning: … {} …", safe_path(p)))` —
/// both escapes, not either one.
///
/// # Enumeration is not a guarantee — the guard is
///
/// Two earlier revisions of this rustdoc asserted coverage by listing the sites that had
/// been routed. Each list was correct when written and stale by the next review: first
/// `lint.rs`'s rule warning was missed, then the walker's own depth-limit warning inside
/// this very file. A guarantee stated as a property of a code path is only ever a
/// property of the sites someone remembered to enumerate — that is PF-004.
///
/// The property is therefore no longer asserted in prose here. It is enforced by
/// `crates/mds-cli/tests/print_discipline.rs`, which fails if any print macro under
/// `crates/mds-cli/src/**` interpolates a value that is not passed through one of the
/// escape helpers, and which applies the same rule to `format!` invocations nested
/// inside `eprint_warning` calls. `watch.rs`'s lifecycle status lines — previously
/// carved out as a pre-existing gap — are in scope and now routed like everything else.
pub(crate) fn eprint_warning(w: &str) {
    eprintln!("{}", mds::sanitize_control_chars(w));
}

/// Neutralize hostile control bytes in source text for `--diff` preview output.
///
/// `--diff` preview output: neutralized on TTY, byte-faithful when piped.
///
/// **`--diff` only.** `--check` on its own emits no preview text — just `Would fix:` /
/// `Would reformat:` status lines, which are sanitized unconditionally via
/// [`safe_path`] and never reach this function. The only caller is
/// [`render_unified_diff`], shared by `mds fmt --diff` and `mds lint --fix --diff`.
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
/// | `--diff` preview output | neutralized, TTY-gated | source excerpts via `neutralize_source_for_render`; piped path returns `Cow::Borrowed` |
#[must_use]
pub(crate) fn preview_text_for(writer_is_tty: bool, text: &str) -> Cow<'_, str> {
    if writer_is_tty {
        neutralize_source_for_render(text)
    } else {
        Cow::Borrowed(text)
    }
}

// ── Diff rendering ───────────────────────────────────────────────────────────

/// Render a unified diff between `original` and `modified` with optional colorization.
///
/// The single shared implementation used by both `fmt --diff` and `lint --diff`.
///
/// When stdout is a TTY, both strings are neutralized via [`preview_text_for`] before
/// diffing so hostile ESC/control bytes in template source cannot inject ANSI commands
/// into the rendered diff (CWE-150, security-11). When piped, the strings pass through
/// unchanged so redirected diff output remains byte-faithful and applicable by
/// `patch`/tooling.
#[must_use]
pub(crate) fn render_unified_diff(original: &str, modified: &str, label: &str) -> String {
    let is_tty = std::io::stdout().is_terminal();
    let original = preview_text_for(is_tty, original);
    let modified = preview_text_for(is_tty, modified);
    let diff = similar::TextDiff::from_lines(original.as_ref(), modified.as_ref());
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(label, label)
        .to_string();

    if unified.is_empty() || !is_tty {
        return unified;
    }
    colorize_unified_diff(&unified)
}

/// Colorize a unified diff string with ANSI codes.
///
/// Uses a state machine keyed on the first `@@` hunk marker to correctly color
/// removed (`-`) and added (`+`) lines inside hunks without miscoloring file-header
/// lines (`---`/`+++`) as hunk content when a removed or added line's own content
/// starts with `--` or `++`.
pub(crate) fn colorize_unified_diff(unified: &str) -> String {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const CYAN: &str = "\x1b[36m";
    const RESET: &str = "\x1b[0m";

    let mut out = String::with_capacity(unified.len() + 64);
    let mut in_hunk = false;
    for line in unified.split_inclusive('\n') {
        let color = if line.starts_with("@@") {
            in_hunk = true;
            CYAN
        } else if !in_hunk && (line.starts_with("---") || line.starts_with("+++")) {
            CYAN
        } else if in_hunk && line.starts_with('+') {
            GREEN
        } else if in_hunk && line.starts_with('-') {
            RED
        } else {
            ""
        };
        if color.is_empty() {
            out.push_str(line);
            continue;
        }
        out.push_str(color);
        match line.strip_suffix('\n') {
            Some(stripped) => {
                out.push_str(stripped);
                out.push_str(RESET);
                out.push('\n');
            }
            None => {
                out.push_str(line);
                out.push_str(RESET);
            }
        }
    }
    out
}

/// Sanitize a filesystem path for terminal display (CWE-150 / CWE-117 guard).
///
/// Converts the path to a display string and applies WIRE-mode
/// [`mds::sanitize_control_chars_wire`] so hostile filenames cannot inject ANSI
/// terminal commands (e.g. `ESC[2J`) *or* forge additional status lines.
///
/// # Why WIRE and not HUMAN
///
/// Status lines are single-line by construction: `Clean: {path}`, `Fixed: {path}`,
/// `Compiled to {path}` are emitted unframed and unindented, one per file. POSIX
/// permits a newline inside a filename, and the user never types the name — `mds lint .`
/// discovers it by directory walk. HUMAN mode preserves newlines (so that multi-line
/// diagnostic *messages* keep rendering), which would let a file whose name embeds
/// `<LF>Clean: real.mds<LF>OK: all-fine.mds` emit two attacker-authored lines
/// byte-identical in form to genuine output. A filename is never legitimately
/// multi-line, so escaping the newline to its six-character literal costs nothing and
/// closes the forgery.
///
/// This matches the treatment `files[].file` already gets on the JSON wire surface
/// (`LintResult::to_canonical_json`) and that miette frame headers get via
/// [`mds::named_source_for_render`] — the human status path was the inconsistent one.
///
/// All status-line path interpolations (`Clean:`, `Fixed:`, `Compiled to:`, etc.) in
/// `lint`, `fmt`, and `build` must route through this helper (avoids PF-004 /
/// security-5: unsanitized filename vector in status output).
pub(crate) fn safe_path(p: &std::path::Path) -> String {
    safe_inline(p.display())
}

/// [`safe_path`] for a filename that is already a `&str` (e.g. a `LintDiagnostic::file`
/// basename that never became a `Path`).
///
/// Exists so those sites cannot drift into open-coding a different escape mode — the
/// exact PF-004 shape that left `Clean: {filename}` on HUMAN mode while every other
/// status line was on WIRE.
pub(crate) fn safe_file_display(name: &str) -> String {
    safe_inline(name)
}

/// WIRE-escape any untrusted value that is interpolated into a **single-line** status,
/// warning, or error line.
///
/// This is the general form of [`safe_path`] / [`safe_file_display`]: the same WIRE
/// escape, for values that are neither a `Path` nor a filename — an `io::Error`
/// `Display` (which embeds a filesystem path), an `mds.json` rule name or config value,
/// a `--format` argument, a fix-rejection reason.
///
/// # Why WIRE, on human surfaces too
///
/// Per the governing per-field rule (spec §7.5): **on the diagnostic surfaces — the
/// `"version": 1` JSON wire, CLI status and warning lines, `[file:line:col]` frame
/// headers — untrusted identifiers, filenames and causes are WIRE-escaped, human output
/// included; only *prose* — a diagnostic message or help body — stays HUMAN.** (Source-map
/// paths and `CompileResult.dependencies` are the named carve-out: not diagnostics, not
/// escaped.) The discriminator is whether the
/// value is legitimately multi-line. A rule name, a path, a `--format` value and an
/// `io::Error` never are; a diagnostic body genuinely is. Leaving `\n` raw in the first
/// group buys nothing and lets the value forge a standalone status line that is
/// byte-identical in form to genuine output (CWE-117).
///
/// The surrounding warning prose stays HUMAN — pass the assembled string to
/// [`eprint_warning`], and WIRE-escape each interpolated value with this helper.
///
/// Idempotent (a property of [`mds::sanitize_control_chars_wire`]), so wrapping a value
/// that was already escaped at construction — e.g.
/// `mds::fix::FixOutcome::Rejected.reason` — is a no-op rather than a double escape.
/// That matters: it lets every call site apply the rule unconditionally instead of
/// tracking which values arrived pre-escaped (PF-004).
///
/// Enforced mechanically by `tests/print_discipline.rs`.
pub(crate) fn safe_inline(value: impl std::fmt::Display) -> String {
    mds::sanitize_control_chars_wire(&value.to_string()).into_owned()
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

    // ── sanitize_report: the CLI human message/help boundary (CWE-150 / #176) ────
    //
    // These are in-process unit tests so they can pin the COLOUR path, which the
    // subprocess e2e tests in `tests/security.rs` are structurally blind to (they set
    // `NO_COLOR=1` and pipe stderr).  Per the H4 test-determinism decision, colour is
    // selected explicitly via `GraphicalTheme` rather than inherited from the
    // environment.

    /// Render `report` through a colour-neutral handler — the deterministic in-process
    /// equivalent of what `render_error_sanitized` emits under `NO_COLOR=1`.
    fn render_nocolor(report: &miette::Report) -> String {
        use miette::{GraphicalReportHandler, GraphicalTheme};
        let mut buf = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .render_report(&mut buf, report.as_ref())
            .expect("render must not panic");
        buf
    }

    /// T-ESC-1 [security-11 / PF-013 / #176]: an `MdsError` whose message *and* help
    /// both interpolate attacker-controlled text is escaped on both surfaces before
    /// miette renders it.
    ///
    /// `UndefinedVariable` is the vector because `name` lands in the `#[error(...)]`
    /// message and in the `#[diagnostic(help(...))]` text, so one input exercises both.
    #[test]
    fn sanitize_report_escapes_message_and_help() {
        let hostile_name = format!("bad{}[31mNAME", '\u{1b}');
        let report = sanitize_report(miette::Report::new(mds::MdsError::UndefinedVariable {
            name: hostile_name,
            span: None,
            src: None,
        }));
        let rendered = render_nocolor(&report);

        // Non-vacuity: the diagnostic actually rendered, with its help line.
        assert!(
            rendered.contains("undefined variable"),
            "non-vacuity: expected the undefined-variable message; got: {rendered:?}"
        );
        assert!(
            rendered.contains("help:"),
            "non-vacuity: expected a help line; got: {rendered:?}"
        );
        // Negative.
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must not survive into the rendered frame; got: {rendered:?}"
        );
        // Positive: escaped on BOTH surfaces, so the count is at least two.
        assert!(
            rendered.matches("\\u001B").count() >= 2,
            "ESC must be escaped in the message AND the help text; got: {rendered:?}"
        );
    }

    /// T-ESC-2 [security-11 / PF-004 / #176]: a CLI-authored `miette::miette!()` report
    /// — which does NOT downcast to `MdsError` — is escaped by the same boundary.
    #[test]
    fn sanitize_report_escapes_cli_authored_miette_message() {
        let hostile = format!("cannot write fo{}[2Jo.mds", '\u{1b}');
        let report = sanitize_report(miette::miette!("{hostile}"));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("cannot write"),
            "non-vacuity: expected the miette!() message; got: {rendered:?}"
        );
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must not survive a miette!() report; got: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u001B"),
            "ESC must be escaped to the \\u001B literal; got: {rendered:?}"
        );
    }

    /// T-ESC-3 [security-11 / #176]: the widened hazard class (bidi controls, Trojan
    /// Source CVE-2021-42574) is escaped on this boundary too, not just C0/DEL/C1.
    #[test]
    fn sanitize_report_escapes_bidi_override_in_message() {
        let hostile = format!("alias{}reversed", '\u{202e}');
        let report = sanitize_report(miette::miette!("{hostile}"));
        let rendered = render_nocolor(&report);

        assert!(
            !rendered.contains('\u{202e}'),
            "raw U+202E must not survive; got: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u202E"),
            "U+202E must be escaped to the \\u202E literal; got: {rendered:?}"
        );
    }

    /// T-ESC-4 [PF-014 / #176]: HUMAN mode — a real newline in the message survives, so
    /// multi-line diagnostic frames stay readable.  This is the one deliberate
    /// difference from the WIRE boundary (`MdsError::serialize`), which escapes `\n`.
    #[test]
    fn sanitize_report_preserves_newline_in_message() {
        let report = sanitize_report(miette::miette!("line one\nline two"));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("line one"),
            "non-vacuity: message must render; got: {rendered:?}"
        );
        assert!(
            !rendered.contains("\\u000A"),
            "HUMAN mode must NOT escape newlines; got: {rendered:?}"
        );
    }

    /// T-ESC-5 [security-11 / #176]: label TEXT is escaped while the label's byte SPAN
    /// is forwarded verbatim, so the caret still lands on the right columns.
    ///
    /// `LintDiagnostic::labels()` uses its own message as the label text, so this is a
    /// real untrusted-text surface and not a hypothetical one.
    #[test]
    fn sanitize_report_escapes_label_text_but_keeps_span() {
        let hostile_label = format!("here{}[31m", '\u{1b}');
        let diag = miette::MietteDiagnostic::new("outer message")
            .with_label(miette::LabeledSpan::at(2..5, hostile_label));
        let report =
            sanitize_report(miette::Report::new(diag).with_source_code("abcdefgh".to_string()));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("here"),
            "non-vacuity: the label must render; got: {rendered:?}"
        );
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must not survive in label text; got: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u001B"),
            "label ESC must be escaped; got: {rendered:?}"
        );
        // Span geometry preserved: the source line and its caret still render.
        assert!(
            rendered.contains("abcdefgh"),
            "the labelled source excerpt must still render; got: {rendered:?}"
        );
    }

    /// T-ESC-6 [PF-014 / #176]: the regression this boundary exists to avoid.
    ///
    /// Rendering the sanitized report with a COLOUR theme must leave miette's own ANSI
    /// SGR codes intact (real ESC bytes present) while the hostile input is escaped to
    /// a literal.  An implementation that sanitized the rendered frame instead would
    /// strip miette's SGR into literal `\u001b[...` noise — this test fails loudly in
    /// that case, and the subprocess e2e tests cannot (they pin `NO_COLOR=1`).
    #[test]
    fn sanitize_report_colour_path_keeps_miette_sgr_and_escapes_hostile_input() {
        use miette::{GraphicalReportHandler, GraphicalTheme};

        let hostile = format!("hostile{}[31mtext", '\u{1b}');
        let report = sanitize_report(miette::miette!("{hostile}"));

        let mut coloured = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode())
            .render_report(&mut coloured, report.as_ref())
            .expect("render must not panic");

        // miette's own SGR codes survive — the frame was never post-processed.
        assert!(
            coloured.contains('\u{1b}'),
            "miette's own SGR codes must survive on the colour path; got: {coloured:?}"
        );
        // The hostile input is still escaped.
        assert!(
            coloured.contains("\\u001B"),
            "hostile ESC must be escaped even on the colour path; got: {coloured:?}"
        );
        // And miette's SGR was NOT itself escaped: the only escaped literal is the one
        // hostile byte, not the ~10 SGR sequences the frame contains.
        assert_eq!(
            coloured.matches("\\u001B").count(),
            1,
            "exactly one escaped literal (the hostile byte) — more means miette's own \
             SGR codes were escaped, which is the PF-014 regression; got: {coloured:?}"
        );
    }

    /// T-ESC-7 [#176]: clean input renders byte-identically with and without the
    /// sanitizing wrapper, so the boundary is inert on the overwhelmingly common path.
    #[test]
    fn sanitize_report_is_inert_on_clean_input() {
        let raw = miette::Report::new(mds::MdsError::UndefinedVariable {
            name: "user_name".to_string(),
            span: None,
            src: None,
        });
        let before = render_nocolor(&raw);
        let after = render_nocolor(&sanitize_report(raw));
        assert_eq!(
            before, after,
            "sanitizing must not alter the render of a clean diagnostic"
        );
    }

    // ── sanitize_report: the auxiliary diagnostic graph (PF-005 / #176) ─────────
    //
    // `source()` / `related()` / `diagnostic_source()` used to be reported as absent,
    // guarded only by a `debug_assert!` that no CLI error populates them. `debug_assert!`
    // is compiled out of release, so that invariant was real in tests and ABSENT in the
    // shipped binary: the first error type to grow a `#[source]` field would have had
    // its cause chain silently dropped from release stderr while CI stayed green.
    //
    // These tests are pure functional assertions on the `Diagnostic` impl, so they hold
    // identically in debug and release — which is the point.

    /// A two-link cause chain: an outer diagnostic whose `#[source]` carries hostile text.
    #[derive(Debug, thiserror::Error, miette::Diagnostic)]
    #[error("outer failure")]
    struct OuterWithCause {
        #[source]
        cause: InnerCause,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("inner cause: {0}")]
    struct InnerCause(String);

    /// A diagnostic that hangs a hostile `related` diagnostic off itself.
    #[derive(Debug, thiserror::Error, miette::Diagnostic)]
    #[error("parent diagnostic")]
    struct ParentWithRelated {
        #[related]
        related: Vec<RelatedChild>,
    }

    #[derive(Debug, thiserror::Error, miette::Diagnostic)]
    #[error("related child: {0}")]
    #[diagnostic(help("child help: {0}"))]
    struct RelatedChild(String);

    /// The hostile fragment shared by the auxiliary-graph tests: one C0 byte, one
    /// 3-byte bidi control, and the 2-byte bidi control #176 added.
    fn hostile_fragment() -> String {
        format!("bad{}[31m{}{}end", '\u{1b}', '\u{202e}', '\u{061c}')
    }

    /// Negative + positive assertions shared by T-AUX-1/2/3.
    fn assert_aux_text_escaped(rendered: &str, surface: &str) {
        for raw in ['\u{1b}', '\u{202e}', '\u{061c}'] {
            assert!(
                !rendered.contains(raw),
                "{surface}: raw U+{:04X} must not survive into the rendered frame; \
                 got: {rendered:?}",
                raw as u32
            );
        }
        // Uppercase literals from a lowercase-hex source vector: proof the byte really
        // decoded and was really escaped rather than passing through as literal text.
        for escaped in ["\\u001B", "\\u202E", "\\u061C"] {
            assert!(
                rendered.contains(escaped),
                "{surface}: {escaped} must appear in the rendered frame; got: {rendered:?}"
            );
        }
    }

    /// T-AUX-1 [PF-005 / security-11 / PF-013 / #176]: a report carrying a `#[source]`
    /// cause chain renders that cause, escaped — neither leaked raw nor dropped.
    #[test]
    fn sanitize_report_escapes_and_preserves_the_cause_chain() {
        let report = sanitize_report(miette::Report::new(OuterWithCause {
            cause: InnerCause(hostile_fragment()),
        }));
        let rendered = render_nocolor(&report);

        // Non-vacuity: the cause is actually rendered. This is the assertion that fails
        // if `source()` reverts to returning `None` — the release-build silent drop.
        assert!(
            rendered.contains("inner cause"),
            "non-vacuity: the cause chain must still render; got: {rendered:?}"
        );
        assert!(
            rendered.contains("outer failure"),
            "non-vacuity: the outer message must still render; got: {rendered:?}"
        );
        assert_aux_text_escaped(&rendered, "cause chain");
    }

    /// T-AUX-2 [PF-005 / #176]: `related()` diagnostics — message AND help — are
    /// escaped and preserved.
    #[test]
    fn sanitize_report_escapes_and_preserves_related_diagnostics() {
        let report = sanitize_report(miette::Report::new(ParentWithRelated {
            related: vec![RelatedChild(hostile_fragment())],
        }));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("parent diagnostic"),
            "non-vacuity: the parent message must render; got: {rendered:?}"
        );
        assert!(
            rendered.contains("related child"),
            "non-vacuity: the related diagnostic must still render; got: {rendered:?}"
        );
        assert!(
            rendered.contains("child help"),
            "non-vacuity: the related diagnostic's help must still render; got: {rendered:?}"
        );
        assert_aux_text_escaped(&rendered, "related diagnostics");
    }

    /// T-AUX-3 [PF-005 / #176]: the walk is depth-bounded, so a self-referential
    /// `source()` cannot hang or overflow the stack during rendering.
    ///
    /// `Cycle::source()` returns `self`, an infinite chain. Construction must terminate.
    #[test]
    fn sanitize_report_bounds_a_cyclic_cause_chain() {
        #[derive(Debug)]
        struct Cycle;
        impl std::fmt::Display for Cycle {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("cyclic cause")
            }
        }
        impl std::error::Error for Cycle {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&Cycle)
            }
        }
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("outer")]
        struct Outer(#[source] Cycle);

        let wrapped = SanitizedReport::new(miette::Report::new(Outer(Cycle)));

        // Walk the materialised chain and assert it is finite and within the bound.
        let mut depth = 0usize;
        let mut node = wrapped.source.as_ref();
        while let Some(n) = node {
            depth += 1;
            assert!(
                depth <= MAX_AUX_DEPTH,
                "the cause-chain walk must be bounded by MAX_AUX_DEPTH"
            );
            node = n.source.as_deref();
        }
        // Non-vacuity: the bound was actually reached, so this is not passing because
        // the chain was empty.
        assert_eq!(
            depth, MAX_AUX_DEPTH,
            "an infinite chain must be truncated at exactly MAX_AUX_DEPTH"
        );
    }

    // ── eprint_warning: the CLI warning sanitization boundary (CWE-150 / PF-004 / #176) ──
    //
    // eprint_warning is a thin wrapper around mds::sanitize_control_chars + eprintln!.
    // The tests below exercise the transformation directly (the pure function that the
    // wrapper applies) to keep assertions deterministic without capturing stderr.
    //
    // Test strategy — corrected.
    //
    // An earlier revision of this block claimed that "the only warning that interpolates
    // untrusted text is the resolver warning" (which needs a MAX_SOURCEMAP_SEGMENTS
    // overflow in a hostile-named module, so an e2e vector for it would be contrived),
    // and concluded that no e2e test was required because "every warning print in
    // main.rs and build.rs now calls eprint_warning". Both halves were false, and the
    // unreachability argument was the load-bearing one:
    //
    //   * lint.rs's unknown-`mds.json`-rule warning (added in e145e41) interpolates an
    //     arbitrary JSON object key — reachable in about thirty seconds by writing an
    //     mds.json into any linted directory. It is covered e2e by T-ESC-RULE-1 in
    //     tests/security.rs, whose vector now carries newlines as well as C0 and bidi
    //     controls, because routing it through eprint_warning (HUMAN, `\n` preserved)
    //     closed CWE-150 on it while leaving the CWE-117 line forgery open.
    //   * The walker's own depth-limit warning, ~40 lines up in THIS file, printed
    //     `dir.display()` through a bare eprintln!. It is neither main.rs nor build.rs,
    //     so the enumeration above walked straight past it. Covered e2e by T-ESC-WALK-1.
    //
    // The unit tests below stay — they pin the transformation deterministically without
    // capturing stderr — but they are no longer offered as a substitute for e2e vectors.
    //
    // Coverage is now enforced rather than enumerated: tests/print_discipline.rs fails if
    // ANY print macro under crates/mds-cli/src interpolates a value that is not passed
    // through safe_path / safe_file_display / safe_inline / sanitize_control_chars*, and
    // applies the same rule to `format!`s nested inside eprint_warning calls. That is
    // what makes a claim about warning-path coverage checkable instead of remembered.
    //
    // Guard-removal RED evidence (T-WARN-1):
    //   Replace `mds::sanitize_control_chars(&hostile)` with
    //   `std::borrow::Cow::Borrowed(hostile.as_str())` in the test body.
    //   The test fails with:
    //     assertion `left == right` failed: raw ESC must be absent from warning output
    //     left: true
    //     right: false
    //   because the unsanitized string still contains '\u{1b}'.
    //   Restoring the call makes the test green.

    /// T-WARN-1 [security-11 / PF-004 / PF-013 / #176]: a hostile warning string
    /// (one that interpolates a filename containing a raw ESC byte) is sanitized to
    /// its `\uXXXX` literal form — raw ESC is absent and the escaped form is present.
    ///
    /// This tests the exact transformation that `eprint_warning` applies before printing.
    #[test]
    fn eprint_warning_sanitizes_hostile_control_chars() {
        // Simulate the only real-world hostile vector: the resolver warning that
        // interpolates an untrusted module filename (mds-core/src/resolver.rs).
        let hostile = format!(
            "MAX_SOURCEMAP_SEGMENTS exceeded in imported module 'lib{}[2Jbar.mds'",
            '\u{1b}'
        );
        // This is exactly what eprint_warning applies before calling eprintln!.
        let result = mds::sanitize_control_chars(&hostile);

        // Non-vacuity: the plain-text content is preserved.
        assert!(
            result.contains("MAX_SOURCEMAP_SEGMENTS"),
            "non-vacuity: warning text must be preserved; got: {result:?}"
        );
        assert!(
            result.contains("lib"),
            "non-vacuity: filename prefix must be preserved; got: {result:?}"
        );
        // Negative: raw ESC absent.
        assert!(
            !result.contains('\u{1b}'),
            "raw ESC must be absent from warning output; hostile = {hostile:?}"
        );
        // Positive: escaped form present.
        assert!(
            result.contains("\\u001B"),
            "ESC must be escaped to \\u001B literal; got: {result:?}"
        );
    }

    /// T-WARN-2 [PF-013 / #176]: a clean warning string passes through unchanged —
    /// the sanitization is inert on the overwhelmingly common path.
    ///
    /// This corresponds to the "clean string passes through unchanged" requirement from
    /// the task brief: eprint_warning must never mutate a warning that contains no
    /// hazardous bytes.
    #[test]
    fn eprint_warning_clean_string_passes_through_unchanged() {
        let clean = "MAX_SOURCEMAP_SEGMENTS exceeded in imported module 'lib.mds'";
        let result = mds::sanitize_control_chars(clean);
        assert_eq!(
            result.as_ref(),
            clean,
            "clean warning must pass through unchanged; got: {result:?}"
        );
    }

    /// T-WARN-3 [PF-013 / #176]: the widened hazard class (bidi controls, Trojan
    /// Source CVE-2021-42574) is escaped by eprint_warning too — not just C0/ESC.
    #[test]
    fn eprint_warning_sanitizes_bidi_control_in_warning_text() {
        // U+202E RIGHT-TO-LEFT OVERRIDE: injecting this into a warning message
        // reverses how the rest of the terminal line renders in bidi-aware terminals.
        let hostile = format!("warning: module 'lib{}evil.mds'", '\u{202e}');
        let result = mds::sanitize_control_chars(&hostile);
        assert!(
            !result.contains('\u{202e}'),
            "raw U+202E must be absent from warning output; got: {result:?}"
        );
        assert!(
            result.contains("\\u202E"),
            "U+202E must be escaped to \\u202E literal; got: {result:?}"
        );
    }

    // ── render_unified_diff / colorize_unified_diff ───────────────────────────

    #[test]
    fn render_unified_diff_empty_for_identical_input() {
        let rendered = render_unified_diff("same\n", "same\n", "label");
        assert!(rendered.is_empty());
    }

    #[test]
    fn render_unified_diff_contains_unified_markers_for_changed_input() {
        let rendered = render_unified_diff("a\n", "b\n", "label");
        assert!(rendered.contains("---"));
        assert!(rendered.contains("+++"));
        assert!(rendered.contains("-a"));
        assert!(rendered.contains("+b"));
    }

    #[test]
    fn colorize_unified_diff_wraps_add_remove_lines_with_ansi_when_requested() {
        let unified = "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n";
        let colorized = colorize_unified_diff(unified);
        assert!(colorized.contains("\x1b[32m+new\x1b[0m"));
        assert!(colorized.contains("\x1b[31m-old\x1b[0m"));
        assert!(colorized.contains("\x1b[36m--- a\x1b[0m"));
    }

    #[test]
    fn colorize_unified_diff_correctly_colors_content_starting_with_dashes_or_pluses() {
        // Regression: a removed line whose content starts with "-- " produces
        // "--- ..." in the rendered unified diff. The old global prefix check
        // matched `starts_with("---")` and mis-colored it CYAN (file header)
        // instead of RED (removal). Same defect for "++" content → "+++ " →
        // mis-colored CYAN instead of GREEN.
        let unified = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n--- dashes content\n+++ plus content\n";
        let colorized = colorize_unified_diff(unified);
        // Inside the hunk: removal of a line whose content starts with "-- "
        assert!(colorized.contains("\x1b[31m--- dashes content\x1b[0m"));
        // Inside the hunk: addition of a line whose content starts with "++"
        assert!(colorized.contains("\x1b[32m+++ plus content\x1b[0m"));
        // File headers (before @@) must still be CYAN
        assert!(colorized.contains("\x1b[36m--- a\x1b[0m"));
        assert!(colorized.contains("\x1b[36m+++ b\x1b[0m"));
    }
}
