//! `lint` subcommand — static analysis of MDS templates beyond `mds check` (issue #61).
//!
//! # Input modes
//!
//! - File path: lint a single `.mds` file (base_dir = file's parent).
//! - Directory: recursively lint every `.mds` file INCLUDING partials, accumulate
//!   and continue past per-file failures, exit = max severity across all files.
//! - `-` (stdin): read from stdin, report diagnostics to stderr, exit by severity.
//!   With `--fix`: fixed source → stdout, diagnostics → stderr.
//!
//! # Channel discipline
//!
//! - Human diagnostics → **stderr** via miette Report.
//! - `--format json` output → **stdout** only (single JSON object, trailing newline).
//! - `--quiet` suppresses warning+info human diagnostics and summaries, NOT errors.
//!
//! # Exit codes (via direct `std::process::exit`, NEVER via `exit_code()`)
//!
//! - 0: clean (no Warn/Error findings)
//! - 1: warning-severity findings only, no errors
//! - 2: any error-severity finding OR analysis failure (parse/resolve/IO/config-load)
//!   OR usage error
//! - 3: ResourceLimit
//!
//! With `--fix`, residual post-fix findings determine the exit code.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};

use mds::{FileSystem, MdsError, NativeFs, Severity};
use miette::Result;

use crate::build::{build_runtime_vars, load_config, read_stdin, resolve_input, RuntimeVarArgs};
use crate::output::collect_mds_files;

/// Known lint rule names — used to warn about unknown names in mds.json config.
const KNOWN_RULES: &[&str] = &[
    "unused-variable",
    "unused-import",
    "unused-function",
    "shadow-variable",
    "empty-block",
    "redundant-else",
    "unreachable-branch",
    "duplicate-import",
    "duplicate-export",
];

pub(crate) struct LintArgs {
    pub(crate) input: Option<PathBuf>,
    /// Apply fixes automatically (Tier A always, Tier B when standalone).
    pub(crate) fix: bool,
    /// Preview --fix: exit 1 if any file would change; never writes.
    pub(crate) check: bool,
    /// Preview --fix: print unified diff; never writes.
    pub(crate) diff: bool,
    pub(crate) quiet: bool,
    pub(crate) format: LintFormat,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) set_string_vars: Vec<(String, String)>,
}

/// Output format for lint diagnostics.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LintFormat {
    Human,
    Json,
}

/// Bundled mode flags to avoid positional-bool transposition hazard.
#[derive(Clone, Copy)]
struct LintFlags {
    fix: bool,
    check: bool,
    diff: bool,
    quiet: bool,
    format: LintFormat,
}

/// Entry point for `mds lint`. Always returns `Ok(())`; exits are via
/// `std::process::exit()` for lint-specific codes, or via the outer `run()`
/// error handler for setup failures (which also exit 2 via this function's catch).
pub(crate) fn run_lint(args: LintArgs) -> Result<()> {
    match do_lint(args) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("{e:?}");
            std::process::exit(2);
        }
    }
}

/// Inner runner — all setup errors propagate as `Err`; `run_lint` catches and exits 2.
fn do_lint(args: LintArgs) -> Result<()> {
    let LintArgs {
        input,
        fix,
        check,
        diff,
        quiet,
        format,
        vars,
        set_vars,
        set_string_vars,
    } = args;

    let flags = LintFlags {
        fix,
        check,
        diff,
        quiet,
        format,
    };

    let runtime_vars = build_runtime_vars(RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    })?;

    let (input, _auto_detected) = resolve_input(input, "lint")?;

    // USAGE ERROR: --fix + --format json + stdin (AC-F-22b).
    // DELIBERATE EXCEPTION (AC-F-14): the JSON envelope is deferred for this 3-way combo;
    // it stays a plain stderr usage message. All other top-level analysis failures route
    // through emit_analysis_failure_json_or_stderr when --format json is active.
    if fix && format == LintFormat::Json && input == Path::new("-") {
        eprintln!(
            "error: --fix --format json with stdin input is not supported; \
             use `mds lint --fix -` for filter mode or `mds lint --format json` for JSON output"
        );
        std::process::exit(2);
    }

    // Stdin mode.
    if input == Path::new("-") {
        return run_lint_stdin(flags, runtime_vars);
    }

    // Directory mode.
    if input.is_dir() {
        // Reject a symlinked directory root (build/check/fmt parity).
        if input
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            // Directory-root symlink → JSON envelope in --format json mode (AC-F-14).
            let mds_err = MdsError::Io {
                message: format!(
                    "directory argument must not be a symlink: {}",
                    input.display()
                ),
            };
            emit_analysis_failure_json_or_stderr(&mds_err, format);
            std::process::exit(2);
        }
        return run_lint_directory(&input, flags, runtime_vars);
    }

    // Single-file mode.
    ensure_mds_extension(&input)?;
    run_lint_file(&input, flags, runtime_vars)
}

/// Reject non-.mds extension (exit 2 via caller catch).
fn ensure_mds_extension(path: &Path) -> Result<()> {
    if path.extension().and_then(|e| e.to_str()) == Some("mds") {
        Ok(())
    } else {
        Err(miette::Error::from(MdsError::NotMdsFile {
            path: path.display().to_string(),
        }))
    }
}

// ── Config helpers ────────────────────────────────────────────────────────────

/// Load mds.json and extract the core `LintConfig`, warning about unknown rule names.
fn load_lint_config(dir: &Path) -> Result<mds::LintConfig> {
    let config_opt = load_config(dir)?;
    match config_opt {
        None => Ok(mds::LintConfig::default()),
        Some((mds_config, _config_dir)) => {
            // Warn on unknown rule names — unknown NAMES are ignored for forward compat.
            for name in mds_config.lint.rules.keys() {
                if !KNOWN_RULES.contains(&name.as_str()) {
                    eprintln!("warning: unknown lint rule '{name}' in mds.json; ignoring");
                }
            }
            Ok(mds_config.lint.into_core_config())
        }
    }
}

// ── Display-path remap ────────────────────────────────────────────────────────

/// Remap the `file` field in every diagnostic in `result` to `display`.
///
/// `mds::lint(path, …)` sets each diagnostic's `file` to the file's basename
/// (via `path.file_name()`). In directory mode the same basename appears for
/// every file, so the JSON output groups all findings under the same key.
/// This function replaces the field with the caller-supplied relative path so
/// the JSON output uses distinct, navigable paths (bug 4 fix).
///
/// Call this immediately after every `mds::lint` that runs in directory mode.
fn set_diag_display_path(result: &mut mds::LintResult, display: &str) {
    for diag in &mut result.diagnostics {
        diag.file = Some(display.to_string());
    }
}

// ── Read source file ──────────────────────────────────────────────────────────

/// Read raw source of `path`: symlink-checked and size-capped (mirrors fmt.rs).
///
/// Returns `MdsError` (not `miette::Error`) so callers can feed the error into
/// `emit_analysis_failure_json_or_stderr` without downcasting (AC-F-14).
fn read_source_file(path: &Path) -> std::result::Result<String, MdsError> {
    let canonical = NativeFs::check_symlink(path)?;
    let path_str = canonical.to_str().ok_or_else(|| MdsError::Io {
        message: format!("path is not valid UTF-8: {}", path.display()),
    })?;
    NativeFs::new().read(path_str)
}

// ── stdout write ──────────────────────────────────────────────────────────────

/// Write `s` to stdout, treating a broken pipe as a clean early exit rather
/// than an error — matches Unix filter conventions (e.g. `mds lint --fix - | head
/// -n1` closing the pipe early must not surface as a crash or failure).
/// Flushes explicitly so a fixed source not ending in `\n` is never silently
/// truncated before the `std::process::exit` that may follow immediately.
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

// ── Human diagnostic rendering ────────────────────────────────────────────────

/// Render one lint diagnostic to stderr, applying `sanitize_control_chars` at the boundary.
///
/// `--quiet` suppresses Warn and Info; Error always renders.
/// `named_source`: optional `(filename, source_text)` pair attached to the miette Report so
/// the renderer can show the offending source line + caret (span highlighting). When absent,
/// diagnostics are still rendered but without source context.
fn render_diag_human(diag: &mds::LintDiagnostic, quiet: bool, named_source: Option<(&str, &str)>) {
    if quiet && matches!(diag.severity, Severity::Info | Severity::Warn) {
        return;
    }
    // Sanitize at the render boundary (AC-F-16): message and help only; raw bytes
    // are preserved in the stored diagnostic and in JSON output via to_canonical_json().
    let sanitized = mds::LintDiagnostic {
        rule: diag.rule.clone(),
        severity: diag.severity,
        message: mds::sanitize_control_chars(&diag.message),
        help: diag.help.as_deref().map(mds::sanitize_control_chars),
        span: diag.span.clone(),
        file: diag.file.clone(),
    };
    // Attach the source code so miette can render the span with source context
    // (source line + caret underline). The labels() implementation on LintDiagnostic
    // returns the span; with_source_code() provides the text miette reads to render it.
    if let Some((filename, src)) = named_source {
        let report = miette::Report::from(sanitized)
            .with_source_code(miette::NamedSource::new(filename, src.to_string()));
        eprintln!("{report:?}");
    } else {
        eprintln!("{:?}", miette::Report::from(sanitized));
    }
}

/// Render all diagnostics in a `LintResult` to stderr.
///
/// `named_source` is forwarded to `render_diag_human` for span context rendering.
fn render_result_human(result: &mds::LintResult, quiet: bool, named_source: Option<(&str, &str)>) {
    for diag in &result.diagnostics {
        render_diag_human(diag, quiet, named_source);
    }
}

// ── Exit code helpers ─────────────────────────────────────────────────────────

/// Compute the lint exit code from a `LintResult` (not considering analysis failures).
///
/// - 2: at least one Error-severity finding
/// - 1: at least one Warn-severity finding (no errors)
/// - 0: clean (Info/Off only, or empty)
fn result_exit_code(result: &mds::LintResult) -> i32 {
    let has_error = result
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    let has_warn = result
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Warn);
    if has_error {
        2
    } else if has_warn {
        1
    } else {
        0
    }
}

/// Compute exit code for an `MdsError` from the lint pipeline.
///
/// - 3: ResourceLimit
/// - 2: everything else (parse, resolve, IO, usage, etc.)
fn mds_error_exit_code(err: &MdsError) -> i32 {
    match err {
        MdsError::ResourceLimit { .. } => 3,
        _ => 2,
    }
}

/// Exit the process with the severity-derived lint exit code when non-zero;
/// no-op for clean (`exit == 0`) results.
///
/// Exit codes: 0 clean / 1 warn-only / 2 error-or-analysis-failure / 3 resource-limit.
fn exit_by_severity(result: &mds::LintResult) {
    let exit = result_exit_code(result);
    if exit != 0 {
        std::process::exit(exit);
    }
}

// ── Atomic write ──────────────────────────────────────────────────────────────

/// Write `content` to `path` atomically via a temp file in the same directory.
///
/// Re-checks for symlink immediately before the write cycle (TOCTOU protection,
/// AC-F-21). Uses `tempfile::Builder` for the temp file so cleanup is automatic
/// on drop if the rename fails.
fn atomic_write_file(path: &Path, content: &str) -> Result<()> {
    // Re-check for symlink right before writing (TOCTOU guard).
    NativeFs::check_symlink(path).map_err(miette::Error::from)?;

    let parent = path.parent().unwrap_or(Path::new("."));
    // Temp file in same directory so rename is always intra-filesystem.
    let mut tmp = tempfile::Builder::new()
        .prefix(".mds-lint-fix-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(|e| miette::miette!("cannot create temp file in {}: {e}", parent.display()))?;
    tmp.write_all(content.as_bytes())
        .map_err(|e| miette::miette!("cannot write temp file: {e}"))?;
    tmp.flush()
        .map_err(|e| miette::miette!("cannot flush temp file: {e}"))?;
    // persist() atomically renames the temp file to the target path.
    tmp.persist(path)
        .map_err(|e| miette::miette!("cannot rename temp file to {}: {e}", path.display()))?;
    Ok(())
}

// ── Fix pipeline helpers ──────────────────────────────────────────────────────

/// Outcome of the `--fix` pipeline for one file.
enum FixFileOutcome {
    Fixed {
        new_source: String,
        residual: mds::LintResult,
    },
    /// Some edits applied, some individually rejected by the per-edit reverify gate.
    ///
    /// Produced when `apply_fixes_incremental` falls back to the per-edit path and not
    /// all edits pass. The `new_source` is the partially-fixed text; `residual` carries
    /// remaining diagnostics (from the last successful per-edit reverify).
    PartiallyFixed {
        new_source: String,
        residual: mds::LintResult,
    },
    Rejected {
        reason: String,
        original: mds::LintResult,
    },
    NothingToFix {
        original: mds::LintResult,
    },
}

/// Run the `--fix` pipeline for a single file's lint result.
///
/// `base_dir` is the file's parent (for reverify recompile).
///
/// ## Reverify gate (AC-F-20)
///
/// The reverify closure checks three conditions:
/// 1. Recompile-success — fixed source must still compile.
/// 2. No-new-untargeted-diagnostics — edit must not introduce new problems.
/// 3. Output byte-equality — when the original source is standalone-compilable,
///    compiled output of the fixed source must be byte-identical to the original.
///
/// If the original source does not compile (e.g. missing runtime vars), the
/// output-diff is skipped — conditions 1 and 2 still apply, and Tier B is
/// already suggestion-only (non-standalone) in that scenario.
fn plan_and_apply_fixes(
    result: mds::LintResult,
    source: &str,
    base_dir: &Path,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    config: &mds::LintConfig,
) -> FixFileOutcome {
    let is_standalone = result.is_standalone;
    let plan = mds::fix::plan_fixes_with_options(&result, source, is_standalone);

    // Bug 12 fix: treat overlap_rejected as "something to do" — don't short-circuit
    // as NothingToFix when edits were rejected due to overlap.  The incremental
    // fallback (per-edit retry) handles individual edits that survive the overlap check.
    if plan.edits.is_empty() && !plan.overlap_rejected {
        return FixFileOutcome::NothingToFix { original: result };
    }

    // AC-F-20 output-delta baseline: compile the original source once.
    // If it fails (e.g. missing runtime vars at eval time), skip the output-diff —
    // existing gates (recompile-success, no-new-diagnostics) still apply.
    let original_output =
        mds::compile_str_collecting_warnings(source, Some(base_dir), runtime_vars.clone())
            .ok()
            .map(|r| r.output);

    let base_dir_owned = base_dir.to_path_buf();
    let config_clone = config.clone();
    // apply_fixes_incremental requires F: Fn (not FnOnce) — the reverify closure
    // may be called up to plan.edits.len()+1 times (batch attempt + per-edit fallback).
    // All captured variables are either borrowed (&) or cloned inside, so the closure
    // satisfies Fn without any extra effort.
    let outcome = mds::fix::apply_fixes_incremental(source, plan, &result, |fixed| {
        let residual = mds::lint_str_with(
            fixed,
            Some(&base_dir_owned),
            runtime_vars.clone(),
            &config_clone,
        )?;

        // AC-F-20 output byte-equality gate: refuse if the fix changes compiled output.
        // All shipped auto-fixes (dup-import/dup-export removal, empty-block removal,
        // dead-branch removal) are output-neutral by design — any delta indicates a bug.
        if let Some(ref orig_out) = original_output {
            match mds::compile_str_collecting_warnings(
                fixed,
                Some(&base_dir_owned),
                runtime_vars.clone(),
            ) {
                Ok(fixed_compile) if fixed_compile.output != *orig_out => {
                    return Err(MdsError::Io {
                        message: "lint --fix would change compiled output; \
                                  edit reverted to preserve template semantics"
                            .to_string(),
                    });
                }
                _ => {} // outputs match, or fixed source didn't compile (lint_str_with caught it)
            }
        }

        Ok(residual)
    });

    match outcome {
        mds::fix::FixOutcome::Fixed {
            source: new_source,
            residual,
        } => FixFileOutcome::Fixed {
            new_source,
            residual,
        },
        mds::fix::FixOutcome::PartiallyFixed {
            source: new_source,
            residual,
            ..
        } => FixFileOutcome::PartiallyFixed {
            new_source,
            residual,
        },
        mds::fix::FixOutcome::Rejected { source: _, reason } => FixFileOutcome::Rejected {
            reason,
            original: result,
        },
        mds::fix::FixOutcome::NothingToFix => FixFileOutcome::NothingToFix { original: result },
    }
}

/// Run the fix pipeline in preview mode (for `--fix --check` / `--fix --diff`).
///
/// Routes through the same gated pipeline as the write path — plan, reverify, and
/// apply — but does NOT write to disk.  Returns the would-be fixed source when at
/// least one edit would be applied (Fixed or PartiallyFixed outcome); `None` when
/// everything was rejected or there is nothing to fix.
///
/// ADR-004 / PF-004: preview must use the same gated pipeline as apply so that
/// `--diff` shows exactly what `--fix` would write, and `--check` exits 1 iff
/// `--fix` would change the file.
fn preview_fixes(
    result: &mds::LintResult,
    source: &str,
    base_dir: &Path,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    config: &mds::LintConfig,
) -> Option<String> {
    let is_standalone = result.is_standalone;
    let plan = mds::fix::plan_fixes_with_options(result, source, is_standalone);

    if plan.edits.is_empty() && !plan.overlap_rejected {
        return None;
    }

    let original_output =
        mds::compile_str_collecting_warnings(source, Some(base_dir), runtime_vars.clone())
            .ok()
            .map(|r| r.output);

    let base_dir_owned = base_dir.to_path_buf();
    let config_clone = config.clone();
    let outcome = mds::fix::apply_fixes_incremental(source, plan, result, |fixed| {
        let residual = mds::lint_str_with(
            fixed,
            Some(&base_dir_owned),
            runtime_vars.clone(),
            &config_clone,
        )?;

        if let Some(ref orig_out) = original_output {
            match mds::compile_str_collecting_warnings(
                fixed,
                Some(&base_dir_owned),
                runtime_vars.clone(),
            ) {
                Ok(fixed_compile) if fixed_compile.output != *orig_out => {
                    return Err(MdsError::Io {
                        message: "lint --fix would change compiled output; \
                                  edit reverted to preserve template semantics"
                            .to_string(),
                    });
                }
                _ => {}
            }
        }

        Ok(residual)
    });

    match outcome {
        mds::fix::FixOutcome::Fixed {
            source: new_source, ..
        }
        | mds::fix::FixOutcome::PartiallyFixed {
            source: new_source, ..
        } => Some(new_source),
        mds::fix::FixOutcome::Rejected { .. } | mds::fix::FixOutcome::NothingToFix => None,
    }
}

// ── Stdin mode ────────────────────────────────────────────────────────────────

fn run_lint_stdin(
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
) -> Result<()> {
    let LintFlags {
        fix, quiet, format, ..
    } = flags;

    let (source, cwd) = read_stdin()?;
    // mds.json load/parse failure → JSON envelope in --format json mode (AC-F-14).
    let config = match load_lint_config(&cwd) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            emit_analysis_failure_json_or_stderr(&mds_err, format);
            std::process::exit(2);
        }
    };

    let result = match mds::lint_str_with(&source, Some(&cwd), runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(e) => {
            emit_analysis_failure_json_or_stderr(&e, format);
            std::process::exit(mds_error_exit_code(&e));
        }
    };

    if fix {
        // --fix stdin: apply fixes, emit fixed source to stdout, diagnostics to stderr.
        let fix_outcome = plan_and_apply_fixes(result, &source, &cwd, runtime_vars, &config);
        let (output_src, diag_result) = match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                residual,
            } => (new_source, residual),
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
            } => {
                eprintln!(
                    "partial fix: some edits were individually rejected by the reverify gate"
                );
                (new_source, residual)
            }
            FixFileOutcome::Rejected { reason, original } => {
                eprintln!("fix rejected: {reason}");
                (source, original)
            }
            FixFileOutcome::NothingToFix { original } => (source, original),
        };
        // Stdin diagnostics: pass source text for span context rendering (bug 19).
        let named_source = Some(("input.mds", output_src.as_str()));
        render_result_human(&diag_result, quiet, named_source);
        let _ = write_stdout(&output_src);
        exit_by_severity(&diag_result);
        return Ok(());
    }

    // Report-only mode: pass stdin source for span context rendering (bug 19).
    let named_source = if format == LintFormat::Human {
        Some(("input.mds", source.as_str()))
    } else {
        None
    };
    emit_result(format, &result, quiet, named_source);
    exit_by_severity(&result);
    Ok(())
}

// ── Single-file mode ──────────────────────────────────────────────────────────

fn run_lint_file(
    path: &Path,
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
) -> Result<()> {
    let LintFlags {
        fix,
        check,
        diff,
        quiet,
        format,
    } = flags;

    let base_dir = path.parent().unwrap_or(Path::new("."));
    // mds.json load/parse failure → JSON envelope in --format json mode (AC-F-14).
    let config = match load_lint_config(base_dir) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            emit_analysis_failure_json_or_stderr(&mds_err, format);
            std::process::exit(2);
        }
    };
    // File read failure (not found, symlink, I/O) → JSON envelope in --format json mode (AC-F-14).
    let source = match read_source_file(path) {
        Ok(s) => s,
        Err(e) => {
            emit_analysis_failure_json_or_stderr(&e, format);
            std::process::exit(mds_error_exit_code(&e));
        }
    };
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<file>");

    let result = match mds::lint(path, runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(e) => {
            emit_analysis_failure_json_or_stderr(&e, format);
            std::process::exit(mds_error_exit_code(&e));
        }
    };

    if result.truncated && fix {
        eprintln!(
            "diagnostic cap ({}) reached; further findings were suppressed — \
             re-run --fix to continue",
            mds::MAX_DIAGNOSTICS
        );
    }

    // Named source for span rendering: (display filename, source text).
    let named_source = Some((filename, source.as_str()));

    // ── Write path: --fix without preview ────────────────────────────────────
    if fix && !check && !diff {
        let fix_outcome = plan_and_apply_fixes(result, &source, base_dir, runtime_vars, &config);
        match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                residual,
            } => {
                emit_result(format, &residual, quiet, named_source);
                if !quiet {
                    eprintln!("Fixed: {}", path.display());
                }
                atomic_write_file(path, &new_source)?;
                exit_by_severity(&residual);
            }
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
            } => {
                if !quiet {
                    eprintln!(
                        "Partially fixed: {} (some edits rejected by reverify gate)",
                        path.display()
                    );
                }
                emit_result(format, &residual, quiet, named_source);
                atomic_write_file(path, &new_source)?;
                exit_by_severity(&residual);
            }
            FixFileOutcome::Rejected { reason, original } => {
                eprintln!("fix rejected: {reason}");
                emit_result(format, &original, quiet, named_source);
                exit_by_severity(&original);
            }
            FixFileOutcome::NothingToFix { original } => {
                emit_result(format, &original, quiet, named_source);
                if !quiet && format == LintFormat::Human && original.diagnostics.is_empty() {
                    eprintln!("Clean: {filename}");
                }
                exit_by_severity(&original);
            }
        }
        return Ok(());
    }

    // ── Preview path: --fix --check and/or --fix --diff ───────────────────────
    // Bug 5 fix: route preview through the same gated pipeline as the write path.
    // Previously called apply_plan_unchecked directly, bypassing the reverify gate —
    // a diff or check result could misrepresent what --fix would actually do.
    if fix && (check || diff) {
        let preview_source = preview_fixes(&result, &source, base_dir, runtime_vars, &config);
        if let Some(ref fixed) = preview_source {
            if diff {
                let label = path.display().to_string();
                let diff_str = render_diff_lint(&source, fixed, &label);
                let _ = write_stdout(&diff_str);
            }
            if check {
                if !quiet {
                    eprintln!("Would fix: {}", path.display());
                }
                std::process::exit(1);
            }
        }
        // After diff-only preview (or when nothing would change), render diagnostics
        // and exit by severity.
        emit_result(format, &result, quiet, named_source);
        exit_by_severity(&result);
        return Ok(());
    }

    // ── Report-only mode (no --fix) ───────────────────────────────────────────
    emit_result(format, &result, quiet, named_source);
    if !quiet && format == LintFormat::Human && result.diagnostics.is_empty() {
        eprintln!("Clean: {filename}");
    }
    exit_by_severity(&result);
    Ok(())
}

// ── Directory mode ────────────────────────────────────────────────────────────

/// Aggregate exit-code category for directory mode.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileTally {
    Clean = 0,
    WarnOnly = 1,
    Error = 2,
    ResourceLimit = 3,
}

impl FileTally {
    fn exit_code(self) -> i32 {
        self as i32
    }
}

fn tally_from_result(result: &mds::LintResult) -> FileTally {
    match result_exit_code(result) {
        2 => FileTally::Error,
        1 => FileTally::WarnOnly,
        _ => FileTally::Clean,
    }
}

/// Lint every `.mds` file under `dir`, INCLUDING `_`-prefixed partials.
///
/// Accumulate-and-continue past per-file failures. Exit = max severity across files.
/// Output is path-sorted (F1 — `collect_mds_files` does NOT sort).
fn run_lint_directory(
    dir: &Path,
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
) -> Result<()> {
    const MAX_DEPTH: usize = 64;
    let LintFlags { quiet, format, .. } = flags;

    // mds.json load/parse failure → JSON envelope in --format json mode (AC-F-14).
    let config = match load_lint_config(dir) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            emit_analysis_failure_json_or_stderr(&mds_err, format);
            std::process::exit(2);
        }
    };

    let mut files = collect_mds_files(dir, MAX_DEPTH, None);

    if files.is_empty() {
        if !quiet {
            eprintln!("No .mds files found in {}", dir.display());
        }
        return Ok(());
    }

    // F1: path-sort explicitly — collect_mds_files does NOT guarantee order.
    files.sort();

    let mut max_tally = FileTally::Clean;
    let mut json_files: Vec<serde_json::Value> = Vec::new();
    let mut any_truncated = false;

    let mut any_would_fix = false;

    for file in &files {
        let tally = if format == LintFormat::Json {
            lint_one_file_accumulating(
                file,
                dir,
                flags,
                &runtime_vars,
                &config,
                &mut json_files,
                &mut any_truncated,
                &mut any_would_fix,
            )
        } else {
            lint_one_file_human(
                file,
                dir,
                flags,
                &runtime_vars,
                &config,
                &mut any_truncated,
                &mut any_would_fix,
            )
        };
        if tally > max_tally {
            max_tally = tally;
        }
    }

    // --fix --check: exit 1 if any file would have been modified (accumulate-and-continue,
    // so every file is checked before exiting).
    if flags.check && any_would_fix {
        std::process::exit(1);
    }

    if format == LintFormat::Json {
        let json = serde_json::json!({
            "version": 1,
            "files": json_files,
            "truncated": any_truncated,
        });
        let _ = write_stdout(&format!(
            "{}\n",
            serde_json::to_string(&json).expect("canonical lint JSON is always serializable")
        ));
    }

    if max_tally.exit_code() != 0 {
        std::process::exit(max_tally.exit_code());
    }
    Ok(())
}

/// Lint one file in directory mode, accumulating results into a JSON array.
#[allow(clippy::too_many_arguments)]
fn lint_one_file_accumulating(
    file: &Path,
    lint_root: &Path,
    flags: LintFlags,
    runtime_vars: &Option<std::collections::HashMap<String, mds::Value>>,
    config: &mds::LintConfig,
    json_files: &mut Vec<serde_json::Value>,
    any_truncated: &mut bool,
    any_would_fix: &mut bool,
) -> FileTally {
    let LintFlags {
        fix, check, diff, ..
    } = flags;

    // Bug 4: compute a display path relative to the lint root so JSON `file` keys
    // are navigable and unique across the whole directory tree (not just basenames).
    let display_path = file
        .strip_prefix(lint_root)
        .unwrap_or(file)
        .display()
        .to_string();

    // `source` is only consumed in the fix branch (below); the report-only/JSON
    // path does not need it — mds::lint() reads the file independently (I-06).
    let base_dir = file.parent().unwrap_or(Path::new("."));

    let mut result = match mds::lint(file, runtime_vars.clone(), config) {
        Ok(r) => r,
        Err(ref e) => {
            json_files.push(serde_json::json!({
                "file": display_path,
                "error": e.serialize()
            }));
            return if matches!(e, MdsError::ResourceLimit { .. }) {
                FileTally::ResourceLimit
            } else {
                FileTally::Error
            };
        }
    };
    // Bug 4: remap basename-only file field → relative display path.
    set_diag_display_path(&mut result, &display_path);

    if result.truncated {
        *any_truncated = true;
        if fix {
            eprintln!(
                "{}: diagnostic cap ({}) reached; further findings were suppressed — \
                 re-run --fix to continue",
                file.display(),
                mds::MAX_DIAGNOSTICS
            );
        }
    }

    if fix && !check && !diff {
        // Read source here only (not earlier) — the report-only path does not need
        // it. On a read error at this point (rare: lint just succeeded above),
        // surface the same error format as other per-file I/O failures and bail.
        let source = match read_source_file(file) {
            Ok(s) => s,
            Err(e) => {
                // Per-file I/O failure in directory mode: accumulate structured error (AC-F-14).
                json_files.push(serde_json::json!({
                    "file": display_path,
                    "error": e.serialize()
                }));
                return FileTally::Error;
            }
        };
        let fix_outcome =
            plan_and_apply_fixes(result, &source, base_dir, runtime_vars.clone(), config);
        match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                mut residual,
            } => {
                set_diag_display_path(&mut residual, &display_path);
                accumulate_result_json(&residual, json_files);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {e}", file.display());
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::PartiallyFixed {
                new_source,
                mut residual,
            } => {
                eprintln!(
                    "{}: partial fix (some edits rejected by reverify gate)",
                    file.display()
                );
                set_diag_display_path(&mut residual, &display_path);
                accumulate_result_json(&residual, json_files);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {e}", file.display());
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::Rejected { reason, original } => {
                eprintln!("{}: fix rejected: {reason}", file.display());
                accumulate_result_json(&original, json_files);
                tally_from_result(&original)
            }
            FixFileOutcome::NothingToFix { original } => {
                accumulate_result_json(&original, json_files);
                tally_from_result(&original)
            }
        }
    } else if fix && (check || diff) {
        // Bug 5: directory-mode preview — route through gated pipeline.
        let source = match read_source_file(file) {
            Ok(s) => s,
            Err(e) => {
                json_files.push(serde_json::json!({
                    "file": display_path,
                    "error": e.serialize()
                }));
                return FileTally::Error;
            }
        };
        if let Some(fixed) = preview_fixes(&result, &source, base_dir, runtime_vars.clone(), config)
        {
            *any_would_fix = true;
            if diff {
                let label = file.display().to_string();
                let diff_str = render_diff_lint(&source, &fixed, &label);
                let _ = write_stdout(&diff_str);
            }
        }
        accumulate_result_json(&result, json_files);
        tally_from_result(&result)
    } else {
        accumulate_result_json(&result, json_files);
        tally_from_result(&result)
    }
}

/// Lint one file in directory mode, rendering diagnostics to stderr (human mode).
#[allow(clippy::too_many_arguments)]
fn lint_one_file_human(
    file: &Path,
    lint_root: &Path,
    flags: LintFlags,
    runtime_vars: &Option<std::collections::HashMap<String, mds::Value>>,
    config: &mds::LintConfig,
    any_truncated: &mut bool,
    any_would_fix: &mut bool,
) -> FileTally {
    let LintFlags {
        fix,
        check,
        diff,
        quiet,
        ..
    } = flags;

    // Bug 4: compute a display path relative to the lint root for human rendering.
    let display_path = file
        .strip_prefix(lint_root)
        .unwrap_or(file)
        .display()
        .to_string();

    let source = match read_source_file(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{:?}", miette::Report::from(e));
            return FileTally::Error;
        }
    };

    let base_dir = file.parent().unwrap_or(Path::new("."));
    // Named source for span rendering: relative display path + source text.
    let named_source = Some((display_path.as_str(), source.as_str()));

    let mut result = match mds::lint(file, runtime_vars.clone(), config) {
        Ok(r) => r,
        Err(ref e) => {
            eprintln!("{:?}", miette::Report::from(e.clone()));
            return if matches!(e, MdsError::ResourceLimit { .. }) {
                FileTally::ResourceLimit
            } else {
                FileTally::Error
            };
        }
    };
    // Bug 4: remap basename-only file field → relative display path.
    set_diag_display_path(&mut result, &display_path);

    if result.truncated {
        *any_truncated = true;
        if fix {
            eprintln!(
                "{}: diagnostic cap ({}) reached; further findings were suppressed — \
                 re-run --fix to continue",
                file.display(),
                mds::MAX_DIAGNOSTICS
            );
        }
    }

    if fix && !check && !diff {
        let fix_outcome =
            plan_and_apply_fixes(result, &source, base_dir, runtime_vars.clone(), config);
        match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                mut residual,
            } => {
                set_diag_display_path(&mut residual, &display_path);
                render_result_human(&residual, quiet, named_source);
                if !quiet {
                    eprintln!("Fixed: {}", file.display());
                }
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {e}", file.display());
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::PartiallyFixed {
                new_source,
                mut residual,
            } => {
                if !quiet {
                    eprintln!(
                        "{}: partial fix (some edits rejected by reverify gate)",
                        file.display()
                    );
                }
                set_diag_display_path(&mut residual, &display_path);
                render_result_human(&residual, quiet, named_source);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {e}", file.display());
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::Rejected { reason, original } => {
                eprintln!("{}: fix rejected: {reason}", file.display());
                render_result_human(&original, quiet, named_source);
                tally_from_result(&original)
            }
            FixFileOutcome::NothingToFix { original } => {
                render_result_human(&original, quiet, named_source);
                tally_from_result(&original)
            }
        }
    } else if fix && (check || diff) {
        // Bug 5: directory-mode preview — route through gated pipeline.
        if let Some(fixed) = preview_fixes(&result, &source, base_dir, runtime_vars.clone(), config)
        {
            *any_would_fix = true;
            if diff {
                let label = file.display().to_string();
                let diff_str = render_diff_lint(&source, &fixed, &label);
                let _ = write_stdout(&diff_str);
            }
            if check && !quiet {
                eprintln!("Would fix: {}", file.display());
            }
        }
        render_result_human(&result, quiet, named_source);
        tally_from_result(&result)
    } else {
        render_result_human(&result, quiet, named_source);
        tally_from_result(&result)
    }
}

// ── Shared emit helpers ───────────────────────────────────────────────────────

/// Emit a `LintResult` to the appropriate channel based on format.
///
/// `named_source` is forwarded to human rendering for span context; ignored in JSON mode.
fn emit_result(
    format: LintFormat,
    result: &mds::LintResult,
    quiet: bool,
    named_source: Option<(&str, &str)>,
) {
    if format == LintFormat::Json {
        let json = result.to_canonical_json();
        let _ = write_stdout(&format!(
            "{}\n",
            serde_json::to_string(&json).expect("canonical lint JSON is always serializable")
        ));
    } else {
        render_result_human(result, quiet, named_source);
    }
}

/// Emit an `MdsError` analysis failure.
/// JSON format → stdout envelope; human → stderr via miette.
fn emit_analysis_failure_json_or_stderr(e: &MdsError, format: LintFormat) {
    if format == LintFormat::Json {
        let envelope = serde_json::json!({
            "version": 1,
            "error": e.serialize()
        });
        let _ = write_stdout(&format!(
            "{}\n",
            serde_json::to_string(&envelope).expect("canonical lint JSON is always serializable")
        ));
    } else {
        eprintln!("{:?}", miette::Report::from(e.clone()));
    }
}

/// Extract and accumulate per-file JSON entries from a `LintResult` into `json_files`.
fn accumulate_result_json(result: &mds::LintResult, json_files: &mut Vec<serde_json::Value>) {
    let inner = result.to_canonical_json();
    if let Some(arr) = inner["files"].as_array() {
        json_files.extend(arr.iter().cloned());
    }
}

// ── Diff rendering ────────────────────────────────────────────────────────────

/// Render a unified diff between `original` and `fixed` with optional colorization.
fn render_diff_lint(original: &str, fixed: &str, label: &str) -> String {
    let diff = similar::TextDiff::from_lines(original, fixed);
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
