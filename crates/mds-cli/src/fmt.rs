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
//! - 2: file not found / not `.mds` / I/O / bad UTF-8
//! - 3: oversized source

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};

use mds::{effective_parent, FileSystem, MdsError};
use miette::Result;

use crate::build::{load_config, read_stdin, resolve_input};
use crate::output::collect_mds_files_detailed;

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
        eprintln!("Formatting {}", input.display());
    }

    let flags = FmtFlags { check, diff, quiet };

    if input == Path::new("-") {
        return run_fmt_stdin(flags);
    }

    if input.is_dir() {
        // Reject a symlinked directory root, matching build/check parity (commit aa0c538).
        if input
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(miette::miette!(
                "directory argument must not be a symlink: {}",
                input.display()
            ));
        }
        return run_fmt_directory(&input, flags);
    }

    ensure_mds_extension(&input)?;
    run_fmt_file(&input, flags)
}

/// Reject an explicit-file input whose extension isn't `.mds` (AC-CF, exit 2).
///
/// `MdsError::NotMdsFile` already maps to exit code 2 via `exit_code` — reused
/// rather than inventing a new error shape.
fn ensure_mds_extension(path: &Path) -> Result<()> {
    let is_mds = path.extension().and_then(|e| e.to_str()) == Some("mds");
    if is_mds {
        Ok(())
    } else {
        Err(miette::Error::from(MdsError::NotMdsFile {
            path: path.display().to_string(),
        }))
    }
}

/// Read the raw source of `path` for formatting: symlink-checked, size-capped,
/// UTF-8-validated (PF-004 parity with `read_stdin` and the resolver).
///
/// `mds fmt` needs the RAW, unparsed source text (not a compiled result), so
/// it can't go through `mds::compile*`. Reuses `NativeFs::check_symlink`
/// (never re-implements canonicalize — PF-003) and `NativeFs::read`'s
/// TOCTOU-safe read-then-size-check instead of a bare `std::fs::read`.
fn read_source_file(path: &Path) -> Result<String> {
    let canonical = mds::NativeFs::check_symlink(path).map_err(miette::Error::from)?;
    let path_str = canonical
        .to_str()
        .ok_or_else(|| miette::miette!("path is not valid UTF-8: {}", path.display()))?;
    mds::NativeFs::new()
        .read(path_str)
        .map_err(miette::Error::from)
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
    let FmtFlags { check, diff, quiet } = flags;
    let (source, cwd) = read_stdin()?;
    let result = format_source_named(&source, Some(&cwd), "<stdin>")?;

    if diff {
        print_diff(&render_diff(&source, &result.formatted, "<stdin>"))?;
    } else if !check {
        // Plain filter mode: formatted content is the output.
        write_stdout(&result.formatted)?;
    }

    if check && result.changed {
        if !quiet {
            eprintln!("Would reformat: <stdin>");
        }
        std::process::exit(1);
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
        let label = path.display().to_string();
        print_diff(&render_diff(&source, &result.formatted, &label))?;
    }

    let read_only = check || diff;
    if !read_only {
        if result.changed {
            std::fs::write(path, &result.formatted)
                .map_err(|e| miette::miette!("cannot write {}: {e}", path.display()))?;
            if !quiet {
                eprintln!("Formatted: {}", path.display());
            }
        } else if !quiet {
            eprintln!("Unchanged: {}", path.display());
        }
        return Ok(());
    }

    if check {
        if result.changed {
            if !quiet {
                eprintln!("Would reformat: {}", path.display());
            }
            std::process::exit(1);
        }
        if !quiet {
            eprintln!("Unchanged: {}", path.display());
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
/// A diff-output failure (non-broken-pipe stdout error) is returned as
/// [`FileOutcome::Failed`] and counted in `fail_count` — consistent with how
/// read and format errors are treated in the surrounding loop.
fn format_one_file(file: &Path, flags: FmtFlags) -> FileOutcome {
    let FmtFlags { check, diff, quiet } = flags;
    let file_name = file.display().to_string();
    let source = match read_source_file(file) {
        Ok(s) => s,
        Err(e) => {
            // File path is embedded in the miette report; sanitize for ESC injection safety
            // (avoids PF-004 parallel-path gap — uses the shared render helper).
            crate::output::eprint_error(e);
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
            crate::output::eprint_error(e);
            return FileOutcome::Failed;
        }
    };

    if diff && result.changed {
        let label = file_name.clone();
        if let Err(e) = print_diff(&render_diff(&source, &result.formatted, &label)) {
            crate::output::eprint_error(e);
            return FileOutcome::Failed;
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
        match std::fs::write(file, &result.formatted) {
            Ok(()) => {
                if !quiet {
                    eprintln!("Formatted: {}", file.display());
                }
                FileOutcome::Formatted
            }
            Err(e) => {
                eprintln!("error: cannot write {}: {e}", file.display());
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
/// exit when any file failed, or (under `--check`) when any file would change.
fn run_fmt_directory(dir: &Path, flags: FmtFlags) -> Result<()> {
    // Directory recursion depth cap, matching `run_build_directory` /
    // `run_check_directory` which also declare MAX_DEPTH as a function-local
    // constant (build.rs line ~744).
    const MAX_DEPTH: usize = 64;

    // Validate mds.json even though `fmt` doesn't act on its `fmt` section's
    // content yet — consistent with build/check, which also fail loudly on a
    // malformed config rather than silently ignoring it.
    let _ = load_config(dir)?;

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, None);
    let files = walk.files;

    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // Always emit — not suppressed by --quiet (avoids silent CI green pass).
            eprintln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was formatted",
                walk.excluded_by_default
            );
            std::process::exit(1);
        }
        if !flags.quiet {
            eprintln!("No .mds files found in {}", dir.display());
        }
        return Ok(());
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
            eprintln!(
                "{changed_count} would reformat, {unchanged_count} unchanged, {fail_count} failed"
            );
        }
    } else if !flags.quiet || fail_count > 0 {
        eprintln!("{changed_count} formatted, {unchanged_count} unchanged, {fail_count} failed");
    }

    if fail_count > 0 || (flags.check && changed_count > 0) {
        std::process::exit(1);
    }
    Ok(())
}

// ── stdout writing (broken-pipe-safe) ────────────────────────────────────────

/// Write `s` to stdout, treating a broken pipe as a clean early exit rather
/// than an error — matches Unix filter conventions (e.g. `mds fmt - | head
/// -n1` closing the pipe early must not surface as a crash or failure).
fn write_stdout(s: &str) -> Result<()> {
    let mut stdout = std::io::stdout();
    if let Err(e) = stdout.write_all(s.as_bytes()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(miette::miette!("cannot write to stdout: {e}"));
    }
    if let Err(e) = stdout.flush() {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(miette::miette!("cannot flush stdout: {e}"));
        }
    }
    Ok(())
}

fn print_diff(rendered: &str) -> Result<()> {
    if rendered.is_empty() {
        return Ok(());
    }
    write_stdout(rendered)
}

// ── unified diff rendering ───────────────────────────────────────────────────

/// Render a unified diff between `original` and `formatted`, colorized with
/// raw ANSI escapes ONLY when stdout is a terminal (mirrors `watch.rs`'s
/// `clear_terminal`, which does the same TTY gate for its own raw escapes;
/// this repo has no color crate dependency).
fn render_diff(original: &str, formatted: &str, label: &str) -> String {
    let diff = similar::TextDiff::from_lines(original, formatted);
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(label, label)
        .to_string();

    if unified.is_empty() || !std::io::stdout().is_terminal() {
        return unified;
    }
    colorize_unified_diff(&unified)
}

fn colorize_unified_diff(unified: &str) -> String {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const CYAN: &str = "\x1b[36m";
    const RESET: &str = "\x1b[0m";

    let mut out = String::with_capacity(unified.len() + 64);
    // `---`/`+++` file headers appear before the first `@@` hunk marker.
    // Inside a hunk a removed line starts with `-` and an added line starts
    // with `+` — but if that line's *content* itself starts with `-` or `+`,
    // the rendered diff line looks like `---` or `+++`, which the old global
    // prefix check mis-colored as CYAN (file header) instead of RED/GREEN.
    // A state machine keyed on the first `@@` avoids the ambiguity.
    let mut in_hunk = false;
    for line in unified.split_inclusive('\n') {
        let color = if line.starts_with("@@") {
            in_hunk = true;
            CYAN
        } else if !in_hunk && (line.starts_with("---") || line.starts_with("+++")) {
            // File header — always precedes the first @@ hunk.
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

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_mds_extension_accepts_dot_mds() {
        assert!(ensure_mds_extension(Path::new("foo.mds")).is_ok());
    }

    #[test]
    fn ensure_mds_extension_rejects_other_extensions() {
        assert!(ensure_mds_extension(Path::new("foo.txt")).is_err());
        assert!(ensure_mds_extension(Path::new("foo")).is_err());
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

    #[test]
    fn render_diff_empty_for_identical_input() {
        let rendered = render_diff("same\n", "same\n", "label");
        assert!(rendered.is_empty());
    }

    #[test]
    fn render_diff_contains_unified_markers_for_changed_input() {
        let rendered = render_diff("a\n", "b\n", "label");
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
