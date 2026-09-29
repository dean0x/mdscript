//! `fmt` subcommand — opinionated, safety-gated auto-formatter (issue #60).
//!
//! Dispatches on the resolved input: `-` (stdin, base_dir = None, formats to
//! stdout as a filter), a directory (recurses, INCLUDING partials — formatting
//! rewrites source, unlike build/check which skip partials because they don't
//! emit their own compiled output), or a single file (base_dir = the file's
//! parent, matching how imports resolve for that file when compiled normally).
//!
//! # Channel discipline
//!
//! - Plain filter-mode formatted content (stdin, no flags) and `--diff` output
//!   go to STDOUT.
//! - All status lines, summaries, and errors go to STDERR.
//! - `--quiet` suppresses status/summaries but never errors (AC-CF-8).
//!
//! # Exit codes (reuses the existing `exit_code` mapping — no new codes)
//!
//! - 0: formatted OK / nothing to change / diff preview
//! - 1: `--check` found something that would change (after printing the
//!   summary), OR a format/parse error (`MdsError` non-io, including
//!   `FormatterInvariant`) via `Err` -> `exit_code`
//! - 2: file not found / not `.mds` / I/O / bad UTF-8 — a rewrite or a stdout write
//!   that fails included, in directory mode too, where one such file makes the run
//!   exit 2 (#157); a closed stdout is not a failure
//! - 3: oversized source (in directory mode, a failed file that leaves the run at 1)

use std::path::{Path, PathBuf};

use mds::effective_parent;
use miette::Result;

use crate::build::{ensure_existing_mds_file, load_config, read_stdin, resolve_input};
use crate::output::{
    atomic_write_file, collect_mds_files_detailed, render_unified_diff, stdout_failure,
    write_stdout, Durability, StdoutOutcome,
};

pub(crate) struct FmtArgs {
    pub(crate) input: Option<PathBuf>,
    pub(crate) check: bool,
    pub(crate) diff: bool,
    pub(crate) quiet: bool,
}

/// Bundled mode flags passed to every per-input helper — avoids the silent
/// transposition hazard of three consecutive positional `bool` parameters.
#[derive(Clone, Copy)]
struct FmtFlags {
    check: bool,
    diff: bool,
    quiet: bool,
}

pub(crate) fn run_fmt(args: FmtArgs) -> Result<()> {
    let FmtArgs {
        input,
        check,
        diff,
        quiet,
    } = args;

    let (input, auto_detected) = resolve_input(input, "fmt")?;
    if auto_detected && !quiet {
        crate::output::ewriteln!("Formatting {}", crate::output::safe_path(&input));
    }

    let flags = FmtFlags { check, diff, quiet };

    if input == Path::new("-") {
        return run_fmt_stdin(flags);
    }

    if input.is_dir() {
        // #413: the one directory-argument check every directory-mode subcommand makes
        // (a symlink, the filesystem root, a forbidden character — all `mds::io`).
        crate::input::resolve_directory_argument(&input).map_err(miette::Error::from)?;
        return run_fmt_directory(&input, flags);
    }

    ensure_existing_mds_file(&input).map_err(miette::Error::from)?;
    run_fmt_file(&input, flags)
}

/// Read the raw source of `path` for formatting: symlink-checked, size-capped,
/// UTF-8-validated (PF-004 parity with `read_stdin` and the resolver).
///
/// `mds fmt` needs the RAW, unparsed source text (not a compiled result), so it
/// can't go through `mds::compile*`. It shares `mds lint`'s reader, so the two
/// subcommands report an unreadable, symlinked or undecodable path identically —
/// as an `MdsError`, which `exit_code` maps to exit 2 (#217).
fn read_source_file(path: &Path) -> Result<String> {
    crate::lint::read_source_file(path).map_err(miette::Error::from)
}

/// The result of formatting one file's content: whether it changed, and the
/// formatted string. Callers apply mode-specific policy (write-if-changed,
/// `--check` tally, `--diff` rendering) on top of this.
struct FmtResult {
    formatted: String,
    changed: bool,
}

/// Format `source` and return a [`FmtResult`] with change detection.
///
/// `file_name` is threaded into lexer and safety-gate errors so diagnostics
/// name the file rather than showing a blank path.
fn format_source_named(
    source: &str,
    base_dir: Option<&Path>,
    file_name: &str,
) -> Result<FmtResult> {
    let formatted =
        mds::format_str_named(source, base_dir, file_name).map_err(miette::Error::from)?;
    let changed = formatted != source;
    Ok(FmtResult { formatted, changed })
}

// ── stdin mode ───────────────────────────────────────────────────────────────

fn run_fmt_stdin(flags: FmtFlags) -> Result<()> {
    use crate::output::STDIN_DISPLAY_LABEL;

    let FmtFlags { check, diff, quiet } = flags;
    let source = read_stdin()?;
    // AD-211-3: one definition of the stdin sentinel, shared with lint/check/build.
    // `None` is the working directory, shown as "." (see `read_stdin`).
    let result = format_source_named(&source, None, STDIN_DISPLAY_LABEL)?;

    // A closed stdout is not an error here: the reader is gone, and the verdict below
    // still stands (#157).
    if diff {
        let rendered = render_unified_diff(&source, &result.formatted, STDIN_DISPLAY_LABEL);
        write_stdout(rendered.as_bytes()).into_batch_result()?;
    } else if !check {
        // Plain filter mode: formatted content is the output.
        write_stdout(result.formatted.as_bytes()).into_batch_result()?;
    }

    if check && result.changed {
        if !quiet {
            crate::output::ewriteln!("Would reformat: {STDIN_DISPLAY_LABEL}");
        }
        crate::output::exit(1);
    }
    Ok(())
}

// ── single-file mode ─────────────────────────────────────────────────────────

fn run_fmt_file(path: &Path, flags: FmtFlags) -> Result<()> {
    let FmtFlags { check, diff, quiet } = flags;
    let source = read_source_file(path)?;
    // effective_parent maps "" (bare filename) to "." so that resolve_base_dir
    // (called by format_str_named → assert_equivalent) receives a canonicalisable
    // path and does not silently fall through to the structural_equivalent fallback
    // that would swallow a genuine mds::syntax error. avoids PF-006, applies ADR-001.
    let base_dir = Some(effective_parent(path));
    let file_name = path.display().to_string();
    let result = format_source_named(&source, base_dir, &file_name)?;

    if diff && result.changed {
        let label = crate::output::safe_path(path);
        let rendered = render_unified_diff(&source, &result.formatted, &label);
        write_stdout(rendered.as_bytes()).into_batch_result()?;
    }

    let read_only = check || diff;
    if !read_only {
        if result.changed {
            // Atomic write preserves file permissions and avoids truncate-then-write
            // data loss on crash or full disk (avoids the issue fixed for lint by
            // commit c5aa086 — both write paths now share the same helper).
            atomic_write_file(path, &result.formatted, Durability::Fsync)?;
            if !quiet {
                crate::output::ewriteln!("Formatted: {}", crate::output::safe_path(path));
            }
        } else if !quiet {
            crate::output::ewriteln!("Unchanged: {}", crate::output::safe_path(path));
        }
        return Ok(());
    }

    if check {
        if result.changed {
            if !quiet {
                crate::output::ewriteln!("Would reformat: {}", crate::output::safe_path(path));
            }
            crate::output::exit(1);
        }
        if !quiet {
            crate::output::ewriteln!("Unchanged: {}", crate::output::safe_path(path));
        }
    }
    Ok(())
}

// ── directory mode ───────────────────────────────────────────────────────────

/// Outcome of formatting a single file in directory mode; tallied by the caller.
enum FileOutcome {
    /// Normal mode: the file was reformatted and written.
    Formatted,
    /// Normal mode: the file was already formatted (no write needed).
    Unchanged,
    /// `--check` / `--diff` mode: the file would change.
    WouldChange,
    /// `--check` / `--diff` mode: the file is already formatted.
    NoChange,
    /// Any per-file error (read, format, diff-output, or write).
    Failed,
}

/// Format one file in directory mode: read → format → (optional) diff → (optional) write.
///
/// All per-file error and status lines are printed as side effects so the
/// directory loop only needs to tally the returned [`FileOutcome`].
///
/// A diff-output failure other than a closed stdout, and a rewrite that fails, are
/// returned as [`FileOutcome::Failed`] and counted in `fail_count` — consistent with how
/// read and format errors are treated in the surrounding loop — and recorded as I/O
/// failures, so the run exits at least 2; so is a read or format error in the I/O and
/// file-system class. A failing stdout is reported once for the run; every file whose
/// diff it lost counts as failed. A closed stdout is not a failure: the diff has no
/// reader, and the file's outcome stands (#157).
fn format_one_file(file: &Path, flags: FmtFlags) -> FileOutcome {
    let FmtFlags { check, diff, quiet } = flags;
    let file_name = file.display().to_string();
    let source = match read_source_file(file) {
        Ok(s) => s,
        Err(e) => {
            // File path is embedded in the miette report; sanitize for ESC injection safety
            // (avoids PF-004 parallel-path gap — uses the shared render helper).
            crate::output::eprint_file_failure(e);
            return FileOutcome::Failed;
        }
    };
    // effective_parent maps "" (bare filename) to "." — avoids PF-006, applies ADR-001.
    let base_dir = Some(effective_parent(file));
    let result = match format_source_named(&source, base_dir, &file_name) {
        Ok(r) => r,
        Err(e) => {
            // MdsError::Syntax embeds user-controlled source fragments that may contain
            // raw ESC bytes; file_name is threaded into the report by format_source_named.
            crate::output::eprint_file_failure(e);
            return FileOutcome::Failed;
        }
    };

    if diff && result.changed {
        let rendered = render_unified_diff(&source, &result.formatted, &file_name);
        match write_stdout(rendered.as_bytes()) {
            StdoutOutcome::Written | StdoutOutcome::Closed => {}
            StdoutOutcome::Failed(e) => {
                crate::output::eprint_io_failure(stdout_failure(&e));
                return FileOutcome::Failed;
            }
            StdoutOutcome::FailedAgain => return FileOutcome::Failed,
        }
    }

    let read_only = check || diff;
    if read_only {
        if result.changed {
            FileOutcome::WouldChange
        } else {
            FileOutcome::NoChange
        }
    } else if !result.changed {
        FileOutcome::Unchanged
    } else {
        // Atomic write preserves file permissions and avoids truncate-then-write
        // data loss on crash or full disk — same guarantee as lint --fix (avoids
        // the divergence introduced after commit c5aa086 hardened the lint path).
        match atomic_write_file(file, &result.formatted, Durability::Fsync) {
            Ok(()) => {
                if !quiet {
                    crate::output::ewriteln!("Formatted: {}", crate::output::safe_path(file));
                }
                FileOutcome::Formatted
            }
            Err(e) => {
                crate::output::eprint_io_failure(e);
                FileOutcome::Failed
            }
        }
    }
}

/// Format every `.mds` file under `dir`, INCLUDING `_`-prefixed partials.
///
/// Deliberate divergence from `run_build_directory` / `run_check_directory`:
/// `is_partial` governs output EMISSION (a partial produces no standalone
/// compiled file), but formatting rewrites SOURCE, and a partial's source is
/// just as much a candidate for reformatting as any other file.
///
/// Continue-on-error: a per-file failure does not abort the run. Non-zero
/// exit when any file failed, or (under `--check`) when any file would change; 2 when an
/// I/O or file-system failure was recorded (#157).
fn run_fmt_directory(dir: &Path, flags: FmtFlags) -> Result<()> {
    // Directory recursion depth cap, matching `run_build_directory` /
    // `run_check_directory` which also declare MAX_DEPTH as a function-local
    // constant (build.rs line ~744).
    const MAX_DEPTH: usize = 64;

    // Validate mds.json even though `fmt` doesn't act on its `fmt` section's
    // content yet — consistent with build, lint and watch, which also fail loudly
    // on a malformed config rather than silently ignoring it.
    let _ = load_config(dir)?;

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, None);
    let files = walk.files;

    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // Always emit — not suppressed by --quiet (avoids silent CI green pass).
            crate::output::ewriteln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was formatted",
                walk.excluded_by_default
            );
            crate::output::exit(1);
        }
        // #204: an empty tree is "nothing to format", not success (mirrors build.rs).
        // Emitted even under --quiet and exit 1.  This arm sits BEFORE the `read_only`
        // split below, so `--check` and `--diff` behave identically on an empty tree.
        crate::output::ewriteln!(
            "no .mds files found in {}; nothing was formatted",
            crate::output::safe_path(dir)
        );
        crate::output::exit(1);
    }

    let read_only = flags.check || flags.diff;
    let mut changed_count: usize = 0;
    let mut unchanged_count: usize = 0;
    let mut fail_count: usize = 0;

    for file in &files {
        match format_one_file(file, flags) {
            FileOutcome::Formatted | FileOutcome::WouldChange => changed_count += 1,
            FileOutcome::Unchanged | FileOutcome::NoChange => unchanged_count += 1,
            FileOutcome::Failed => fail_count += 1,
        }
    }

    if read_only {
        // The `changed_count > 0` disjunct was deliberately removed: --quiet
        // suppresses status/summaries (including "N would reformat"), never
        // errors. Only fail_count > 0 forces a summary under --quiet, matching
        // the non-read_only branch's `!quiet || fail_count > 0` contract and
        // the single-file --check path (which is fully silent under --quiet,
        // exiting 1 with no message when a file would change).
        if !flags.quiet || fail_count > 0 {
            crate::output::ewriteln!(
                "{changed_count} would reformat, {unchanged_count} unchanged, {fail_count} failed"
            );
        }
    } else if !flags.quiet || fail_count > 0 {
        crate::output::ewriteln!(
            "{changed_count} formatted, {unchanged_count} unchanged, {fail_count} failed"
        );
    }

    if fail_count > 0 || (flags.check && changed_count > 0) {
        crate::output::exit(1);
    }
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mds::MdsError;

    #[test]
    fn ensure_existing_mds_file_accepts_existing_mds() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.mds");
        std::fs::write(&p, "").unwrap();
        assert!(
            ensure_existing_mds_file(&p).is_ok(),
            "existing .mds file must pass"
        );
    }

    #[test]
    fn ensure_existing_mds_file_rejects_non_mds_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.txt");
        std::fs::write(&p, "").unwrap();
        let err = ensure_existing_mds_file(&p).expect_err("existing .txt file must be rejected");
        // Extension error (not existence error).
        assert!(
            matches!(err, MdsError::NotMdsFile { .. }),
            "error must be NotMdsFile for existing .txt; got: {err:?}"
        );
    }

    #[test]
    fn ensure_existing_mds_file_rejects_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nonexistent.mds");
        let err = ensure_existing_mds_file(&p).expect_err("nonexistent file must be rejected");
        // Existence error (not extension error) — even though extension is .mds.
        assert!(
            matches!(err, MdsError::FileNotFound { .. }),
            "error must be FileNotFound for missing path; got: {err:?}"
        );
    }

    #[test]
    fn ensure_existing_mds_file_missing_non_mds_reports_not_found() {
        // Non-existent path with wrong extension must still report FileNotFound (C4/F6):
        // existence is checked before extension.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nonexistent.txt");
        let err = ensure_existing_mds_file(&p).expect_err("nonexistent .txt file must be rejected");
        assert!(
            matches!(err, MdsError::FileNotFound { .. }),
            "must be FileNotFound (not NotMdsFile) for non-existent path; got: {err:?}"
        );
    }

    #[test]
    fn format_source_detects_no_change() {
        let result = format_source_named("Hello!\n", None, "<source>").unwrap();
        assert!(!result.changed);
        assert_eq!(result.formatted, "Hello!\n");
    }

    #[test]
    fn format_source_detects_change() {
        let result =
            format_source_named("Hello!\r\n\r\n\r\n\r\nBye.\r\n", None, "<source>").unwrap();
        assert!(result.changed);
        assert!(!result.formatted.contains('\r'));
    }
}
