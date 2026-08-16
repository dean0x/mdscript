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
//! # `--quiet` status-message contract (#216, decision D4)
//!
//! Every *status* message on stderr is gated on `!quiet` in **all three input modes**
//! (directory, single file, stdin) — `Fixed:`, `Partially fixed:`, `Would fix:`,
//! `Clean:`, `fix rejected:`, the diagnostic-cap notice, and the directory summary.
//! PF-004: these are separate emitters per mode, so a gate added to one must be added
//! to all; issue #43/#173 was exactly this divergence for `Partially fixed:`.
//!
//! Two messages deliberately bypass `--quiet` because they signal a silent-CI-green-pass
//! hazard rather than status: the all-excluded diagnostic in [`run_lint_directory`] and
//! the directory summary when any file is in the error or resource-limited bucket.
//! A third emitter also bypasses `--quiet`: `output::collect_mds_files_inner`'s
//! depth-limit warning (fires on trees deeper than MAX_DEPTH=64) accepts no quiet
//! parameter — documented limitation per AC-Q05/PF-015, mirroring `build.rs`.
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

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mds::{effective_parent, FileSystem, MdsError, NativeFs, Severity};
use miette::Result;

use crate::build::{
    build_runtime_vars, ensure_existing_mds_file, load_config, read_stdin, resolve_input,
    RuntimeVarArgs,
};
use crate::output::{
    atomic_write_file, collect_mds_files_detailed, eprint_error, eprint_warning,
    relabel_stdin_error, render_unified_diff, safe_file_display, safe_inline, safe_path,
    STDIN_DISPLAY_LABEL,
};

// AC-224-15: No local rule-name list. The single source of truth is
// mds::KNOWN_LINT_RULES (composed from each rule module's own RULE const).
// No rule-name string literals from the registry appear in this directory.

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
            // Route through the single render choke point (avoids PF-004 /
            // architecture-6: hand-rolled sanitize_control_chars bypass).
            eprint_error(e);
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
            emit_analysis_failure_json_or_stderr(&mds_err, format, None);
            std::process::exit(2);
        }
        return run_lint_directory(&input, flags, runtime_vars);
    }

    // Single-file mode.
    // Check existence first, then extension (C4/F6): a non-existent path must report
    // mds::file_not_found, not mds::not_mds_file, regardless of the extension.
    // Route through emit_analysis_failure_json_or_stderr so --format json produces the
    // correct error envelope (L-CLI-JSON4 / AC-F-14). Do NOT use `?` here.
    if let Err(mds_err) = ensure_existing_mds_file(&input) {
        emit_analysis_failure_json_or_stderr(&mds_err, format, None);
        std::process::exit(mds_error_exit_code(&mds_err));
    }
    run_lint_file(&input, flags, runtime_vars)
}

// ── Config helpers ────────────────────────────────────────────────────────────

/// Load mds.json and extract the core `LintConfig`, warning about unknown rule names.
///
/// AD-224-1 (2026-08-12 ruling): an unknown rule name WARNS and lint CONTINUES.
/// The domain is asymmetric — severities are a closed set but rule names grow
/// every release — so hard-failing would break configs naming rules from a newer
/// mds when run with an older binary.
///
/// AD-224-5 (AC-224-21, AC-224-22): the warning goes to STDERR only (never
/// stdout — `--format json` stdout must remain valid parseable JSON), and is
/// SUPPRESSED under `--quiet` (AC-224-22, coordination point with PR4 D4): the
/// unknown-rule warning is never the causal reason for a non-zero exit — an
/// unknown rule has no enforcement and does not affect `result_exit_code`, which
/// counts actual lint findings, not config anomalies. A `--quiet` consumer who
/// receives a non-zero exit will always have a visible, causal lint finding.
/// `crates/mds-cli/src/main.rs:30` documents `--quiet` as suppressing
/// *"status and diagnostic output"*; this warning is status, not an error.
fn load_lint_config(dir: &Path, quiet: bool) -> Result<mds::LintConfig> {
    let config_opt = load_config(dir)?;
    match config_opt {
        None => Ok(mds::LintConfig::default()),
        Some((mds_config, _config_dir)) => {
            // into_core_config returns (config, Option<UnknownRuleNames>) in one step —
            // structurally forcing the caller to handle the unknowns report so detection
            // cannot be accidentally skipped (review finding at config.rs:104).
            // The safe_inline / print-discipline contract (AD-224-3, AC-224-6) and the
            // --quiet gate (AD-224-5, AC-224-22) are documented at `emit_unknown_rule_warning`.
            let (lint_config, unknown) = mds_config.lint.into_core_config();
            if let Some(unknown) = unknown {
                emit_unknown_rule_warning(&unknown, quiet);
            }
            Ok(lint_config)
        }
    }
}

/// Emit the unknown-rule warning for one config load, suppressed under `--quiet`.
///
/// Both `load_lint_config` (stdin/single-file path) and `LintDirCtx::config_for`
/// (directory path) call this function. One definition prevents the duplication
/// that already drifted once inside this PR on the CWE-117 escape path (PF-009:
/// the same work set represented twice drifts; ADR-008: the escape contract is
/// per-file, so every call site is equally security-relevant).
///
/// AC-224-3 (amended criterion, repo-owner ruling 2026-08-16): the criterion requires
/// a shared message body, a shared recognised-rules list, and a shared sort order
/// across all five surfaces. The CLI prefix `"warning: in mds.json: "` is permitted
/// by the amended criterion; it carries source-file provenance that the bindings
/// cannot provide because their rules arrive in the caller's options object, not a
/// config file. One formatter ([`mds::format_unknown_rule_names_warning`]) produces
/// the shared body; the CLI adds the provenance prefix before emitting on stderr.
///
/// AD-224-3 (AC-224-6): every value interpolated inside `eprint_warning`'s
/// `format!` must be a WHOLE-EXPRESSION `safe_inline` call — the one shape that
/// `print_discipline.rs`'s trace accepts without an allowlist entry. Keeping the
/// `eprint_warning` call here (in a named function inside `crates/mds-cli/src/`)
/// preserves that machine-checked coverage: the scanner enumerates every `.rs`
/// file under `src/` and checks every `eprint_warning(…)` call site it finds.
/// `safe_inline(&core_msg)` satisfies that constraint; the call is idempotent
/// because names are already wire-escaped inside `format_unknown_rule_names_warning`.
///
/// AD-224-5 (AC-224-22): no-op when `quiet` is true.
fn emit_unknown_rule_warning(unknown: &mds::UnknownRuleNames, quiet: bool) {
    if quiet {
        return;
    }
    // AC-224-3: shared message body; "in mds.json:" prefix carries source provenance.
    let core_msg = mds::format_unknown_rule_names_warning(unknown);
    eprint_warning(&format!("warning: in mds.json: {}", safe_inline(&core_msg)));
}

// ── Display-path remap ────────────────────────────────────────────────────────

/// Remap the `file` field in every diagnostic in `result` to `display`.
///
/// `mds::lint(path, …)` sets each diagnostic's `file` to the file's basename
/// (via `path.file_name()`). In directory mode the same basename appears for
/// every file, so the JSON output groups all findings under the same key.
/// This function replaces the field with the caller-supplied relative path so
/// the JSON output uses distinct, navigable paths.
///
/// **AD-211-4 (stdin relabel):** also used for stdin mode — called with
/// `STDIN_DISPLAY_LABEL` immediately after every `mds::lint_str_with` call so
/// that `diag.file` in the JSON wire output reads `"<stdin>"` rather than the
/// internal VFS key `"input.mds"` (`STRING_SOURCE_MAP_LABEL`).
///
/// Call this immediately after every `mds::lint` / `mds::lint_str_with` call.
fn set_diag_display_path(result: &mut mds::LintResult, display: &str) {
    for diag in &mut result.diagnostics {
        diag.file = Some(display.to_string());
    }
}

/// Returns the path of `path` relative to `root`, normalised to forward-slash
/// separators by joining path components with `/`.
///
/// Using `Path::components()` is the correct platform-aware join: on Windows a
/// backslash is a path separator, so each component is a directory or filename
/// segment; on Unix a backslash is an ordinary filename byte (POSIX forbids
/// only `/` and NUL), so a single component carries the whole
/// `\`-containing filename intact.  The naive `.replace('\\', "/")` alternative
/// would manufacture a path separator from an ordinary filename byte on Unix,
/// turning `sub/..\..\etc\evil.mds` into `sub/../../../etc/evil.mds` and
/// providing a directory-traversal vector (CWE-22/CWE-41).
///
/// The directory-mode sort applies `sanitize_control_chars_wire` on top of
/// this function's output so that the sort key matches the sanitized
/// `files[].file` value emitted by `to_canonical_json` for diagnostic entries,
/// keeping array position consistent with the emitted key order (AC-P1-10).
/// Error-only entries (`{"file": …, "error": …}`) push the raw display path
/// without a second sanitization pass — a pre-existing asymmetry; their array
/// position is still determined by the sanitized sort key.
///
/// Forward slashes (`/`, 0x2F) are used instead of
/// the native backslash separator (`\`, 0x5C) because bytes in the range
/// 0x30–0x5B (digits `0`–`9`, uppercase letters `A`–`Z`, and `[`) sort between
/// `/` and `\`; on Windows a flat file like `sub[abc.mds` would sort BEFORE a
/// nested path `sub\d.mds` using the native separator (0x5B < 0x5C), but AFTER
/// it with the emitted forward slash (0x5B > 0x2F), reversing the array order
/// relative to the emitted key order.
fn relative_display(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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

/// Render one lint diagnostic to stderr. All user-controlled text — message, help,
/// filename, and source — is sanitized at the input boundary so miette renders from
/// safe inputs; the frame itself is not post-processed (PF-014).
///
/// `--quiet` suppresses Warn and Info; Error always renders.
/// `named_source`: `(filename, source_text)` pair for span context rendering.
///
/// Sanitization strategy (PF-014):
/// - message/help: HUMAN-mode `sanitize_control_chars` → \\uXXXX escapes. `\n` is
///   preserved so a multi-line diagnostic frame keeps rendering.
/// - filename + source: `mds::named_source_for_render`, the shared boundary
///   `MdsError::at()` and the formatter also use — WIRE-mode escaping for the
///   single-line filename, byte-length-preserving neutralization for the span-indexed
///   source so miette's byte-offset slices stay valid.
///
/// The rendered miette frame is NOT post-processed, so miette's own SGR colour codes
/// are never corrupted.
fn render_diag_human(diag: &mds::LintDiagnostic, quiet: bool, named_source: (&str, &str)) {
    if quiet && matches!(diag.severity, Severity::Info | Severity::Warn) {
        return;
    }
    // Sanitize message and help at the input boundary via the core method.
    // fix_removals/fix_edits are set to None inside sanitized_for_render to avoid
    // unnecessary allocations (architecture-3 / rust-1).
    let sanitized = diag.sanitized_for_render();
    let (filename, src) = named_source;
    let report = miette::Report::from(sanitized)
        .with_source_code(mds::named_source_for_render(filename, src));
    eprint_error(report);
}

/// Render all diagnostics in a `LintResult` to stderr.
///
/// `named_source` is forwarded to `render_diag_human` for span context rendering.
fn render_result_human(result: &mds::LintResult, quiet: bool, named_source: (&str, &str)) {
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
    /// `applied_count` / `total_count` are used for the summary line.
    PartiallyFixed {
        new_source: String,
        residual: mds::LintResult,
        applied_count: usize,
        total_count: usize,
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
/// `display_label` is the caller-supplied display path (relative, forward-slash-normalised)
/// written into every `diag.file` in residual results via `set_diag_display_path`; it
/// becomes the `files[].file` wire key after `to_canonical_json` applies
/// `sanitize_control_chars_wire`.  This is the value that appears in the JSON wire output
/// and must be pre-sanitized by the caller for error-envelope entries that bypass
/// `to_canonical_json`.
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
    display_label: &str,
) -> FixFileOutcome {
    let is_standalone = result.is_standalone;
    let plan = mds::fix::plan_fixes_with_options(&result, source, is_standalone);

    // Treat overlap_rejected as "something to do" — don't short-circuit as NothingToFix
    // when edits were rejected due to overlap.  The incremental fallback (per-edit retry)
    // handles individual edits that survive the overlap check.
    if plan.edits.is_empty() && !plan.overlap_rejected {
        return FixFileOutcome::NothingToFix { original: result };
    }

    // Capture total edit count before moving plan into apply_fixes_incremental.
    // Used to compute the "{applied} of {total}" summary for PartiallyFixed output.
    let total_edits = plan.edits.len();

    // `legacy-interpolation` edits intentionally change compiled output (they migrate
    // `{x}` plain-text to `{{x}}` interpolation).  When the plan contains any such
    // edit, the output byte-equality gate below must be skipped — it only applies to
    // output-neutral fixes (dup-import removal, empty-block removal, etc.).
    let all_output_neutral = plan
        .edits
        .iter()
        .all(|e| mds::fix::is_output_neutral(&e.rule));

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
        // Skipped when the plan contains any output-changing rule (currently only
        // `legacy-interpolation`) — those fixes intentionally alter compiled output
        // and the gate would always reject them.
        if all_output_neutral {
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
        }

        Ok(residual)
    });

    match outcome {
        mds::fix::FixOutcome::Fixed {
            source: new_source,
            mut residual,
        } => {
            set_diag_display_path(&mut residual, display_label);
            FixFileOutcome::Fixed {
                new_source,
                residual,
            }
        }
        mds::fix::FixOutcome::PartiallyFixed {
            source: new_source,
            mut residual,
            rejected,
        } => {
            set_diag_display_path(&mut residual, display_label);
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
                applied_count: total_edits - rejected.len(),
                total_count: total_edits,
            }
        }
        mds::fix::FixOutcome::Rejected { source: _, reason } => FixFileOutcome::Rejected {
            reason,
            original: result,
        },
        mds::fix::FixOutcome::NothingToFix => FixFileOutcome::NothingToFix { original: result },
        // FixOutcome is #[non_exhaustive]: a wildcard arm is required when
        // matching from outside mds-core.  New variants added in future
        // releases should be plumbed here; until then, treat them as no-ops.
        _ => FixFileOutcome::NothingToFix { original: result },
    }
}

/// Outcome of the preview fix pipeline (`--fix --check` / `--fix --diff`).
///
/// Distinguishes "would fix", "rejected by reverify gate", and "nothing to fix"
/// so that callers can surface rejection reasons in `--fix --check` output
/// (PF-004: preview must use the same gated pipeline as apply and be equally honest
/// about outcomes).
enum PreviewOutcome {
    /// At least one edit would be applied; contains the would-be fixed source.
    WouldFix(String),
    /// Every edit was refused by the reverify gate (overlap or recompile failure);
    /// contains the human-readable rejection reason.
    Rejected(String),
    /// No fixable edits exist or the plan is empty with no overlap.
    NothingToFix,
}

/// Run the fix pipeline in preview mode (for `--fix --check` / `--fix --diff`).
///
/// Routes through the same gated pipeline as the write path — plan, reverify, and
/// apply — but does NOT write to disk.  Returns [`PreviewOutcome::WouldFix`] with
/// the would-be fixed source when at least one edit would be applied;
/// [`PreviewOutcome::Rejected`] with the rejection reason when the reverify gate
/// refused every edit; [`PreviewOutcome::NothingToFix`] when the plan is empty.
///
/// ADR-004 / PF-004: preview must use the same gated pipeline as apply so that
/// `--diff` shows exactly what `--fix` would write, and `--check` exits 1 iff
/// `--fix` would change the file.  The `Rejected` variant gives `--fix --check`
/// the same honesty as `--fix` (which prints "fix rejected: …" on refusal).
fn preview_fixes(
    result: &mds::LintResult,
    source: &str,
    base_dir: &Path,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    config: &mds::LintConfig,
) -> PreviewOutcome {
    let is_standalone = result.is_standalone;
    let plan = mds::fix::plan_fixes_with_options(result, source, is_standalone);

    if plan.edits.is_empty() && !plan.overlap_rejected {
        return PreviewOutcome::NothingToFix;
    }

    // Same output-neutral check as plan_and_apply_fixes: skip the equality gate when
    // the plan contains any output-changing rule (e.g. legacy-interpolation).
    let all_output_neutral = plan
        .edits
        .iter()
        .all(|e| mds::fix::is_output_neutral(&e.rule));

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

        // Output byte-equality gate — skipped for output-changing rules.
        if all_output_neutral {
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
        }

        Ok(residual)
    });

    match outcome {
        mds::fix::FixOutcome::Fixed {
            source: new_source, ..
        }
        | mds::fix::FixOutcome::PartiallyFixed {
            source: new_source, ..
        } => PreviewOutcome::WouldFix(new_source),
        mds::fix::FixOutcome::Rejected { reason, .. } => PreviewOutcome::Rejected(reason),
        mds::fix::FixOutcome::NothingToFix => PreviewOutcome::NothingToFix,
        // FixOutcome is #[non_exhaustive]: wildcard required from outside mds-core.
        _ => PreviewOutcome::NothingToFix,
    }
}

// ── Stdin mode ────────────────────────────────────────────────────────────────

fn run_lint_stdin(
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
) -> Result<()> {
    // Bind all five flags explicitly — `..` would silently drop unbound flags to
    // their defaults, which breaks `--fix --check` semantics (avoids PF-004).
    let LintFlags {
        fix,
        check,
        diff,
        quiet,
        format,
    } = flags;

    let (source, cwd) = read_stdin()?;
    // mds.json load/parse failure → JSON envelope in --format json mode (AC-F-14).
    let config = match load_lint_config(&cwd, quiet) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            // AD-211-5: config errors (MdsError::Io) carry no embedded NamedSource,
            // so the relabel is a no-op here. Passed anyway so the envelope rule holds
            // for EVERY stdin failure path — a future error variant routed here that
            // does carry a source inherits the sentinel instead of needing a new call.
            emit_analysis_failure_json_or_stderr(&mds_err, format, Some(&source));
            std::process::exit(2);
        }
    };

    let mut result = match mds::lint_str_with(&source, Some(&cwd), runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(e) => {
            // AD-211-5: relabel <source> → <stdin> in the rendered failure envelope.
            emit_analysis_failure_json_or_stderr(&e, format, Some(&source));
            std::process::exit(mds_error_exit_code(&e));
        }
    };

    // AD-211-4 / AD-211-1: relabel diag.file from STRING_SOURCE_MAP_LABEL →
    // STDIN_DISPLAY_LABEL at the CLI output boundary.  fix.rs never reads diag.file
    // (verified: zero reads in fix.rs), so this relabel is safe upstream of both
    // preview_fixes and plan_and_apply_fixes.  This single call ensures
    // "files[].file" emits "<stdin>" across all code paths (AC-P1-01); for the
    // write path, plan_and_apply_fixes relabels its internally produced residual
    // before returning.
    set_diag_display_path(&mut result, STDIN_DISPLAY_LABEL);

    // D4 (AD-216-11): status message, not an error — suppress under --quiet.
    // PF-004: stdin is a separate emitter from file/directory mode; the cap notice
    // must reach this path too (avoids the #43/#173 divergence class).
    if result.truncated && fix && !quiet {
        eprintln!(
            "diagnostic cap ({}) reached; further findings were suppressed — \
             re-run --fix to continue",
            mds::MAX_DIAGNOSTICS
        );
    }

    if fix {
        // ── Preview path: --fix --check and/or --fix --diff (never writes source) ───
        // Mirrors run_lint_file's preview path so stdin honours --check / --diff the
        // same way file targets do (PF-004: same gated pipeline as the write path).
        if check || diff {
            let preview = preview_fixes(&result, &source, &cwd, runtime_vars.clone(), &config);
            match preview {
                PreviewOutcome::WouldFix(ref fixed) => {
                    if diff {
                        let diff_str = render_unified_diff(&source, fixed, STDIN_DISPLAY_LABEL);
                        let _ = write_stdout(&diff_str);
                    }
                    if check {
                        if !quiet {
                            eprintln!("Would fix: {STDIN_DISPLAY_LABEL}");
                        }
                        std::process::exit(1);
                    }
                }
                PreviewOutcome::Rejected(ref reason) => {
                    if !quiet {
                        eprintln!("fix rejected: {}", safe_inline(reason));
                    }
                }
                PreviewOutcome::NothingToFix => {}
            }
            // After diff-only preview, or when nothing would change / fix rejected:
            // render diagnostics of the original result and exit by severity.
            // AD-211-1: pass STDIN_DISPLAY_LABEL so span context renders "<stdin>", not
            // the internal STRING_SOURCE_MAP_LABEL ("input.mds").
            let named_source = if format == LintFormat::Human {
                Some((STDIN_DISPLAY_LABEL, source.as_str()))
            } else {
                None
            };
            emit_result(format, &result, quiet, named_source);
            exit_by_severity(&result);
            return Ok(());
        }

        // ── Write path: apply fixes, emit fixed source to stdout ─────────────────
        let fix_outcome = plan_and_apply_fixes(
            result,
            &source,
            &cwd,
            runtime_vars,
            &config,
            STDIN_DISPLAY_LABEL,
        );
        let (output_src, diag_result) = match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                residual,
            } => (new_source, residual),
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
                applied_count,
                total_count,
            } => {
                if !quiet {
                    eprintln!(
                        "Partially fixed: {STDIN_DISPLAY_LABEL} ({applied_count} of {total_count} fixes applied)"
                    );
                }
                (new_source, residual)
            }
            FixFileOutcome::Rejected { reason, original } => {
                // D4 (AD-216-11): "fix rejected" is a status message — the safety
                // gate fired and the original diagnostics are unmodified.  Suppress under
                // --quiet, matching this function's own `PreviewOutcome::Rejected` arm and
                // the directory-mode emitters in `lint_one_file_accumulating` /
                // `lint_one_file_human`.  PF-004: stdin is a separate emitter from
                // directory mode and must not bypass the gate (the #43/#173 divergence
                // class).  Exit code is unaffected; error-severity residual diagnostics
                // still print through `render_result_human`.
                if !quiet {
                    eprintln!("fix rejected: {}", safe_inline(&reason));
                }
                (source, original)
            }
            FixFileOutcome::NothingToFix { original } => (source, original),
        };
        // Stdin diagnostics: pass source text for span context rendering.
        // AD-211-1: use STDIN_DISPLAY_LABEL so source frame header reads "<stdin>".
        let named_source = (STDIN_DISPLAY_LABEL, output_src.as_str());
        render_result_human(&diag_result, quiet, named_source);
        let _ = write_stdout(&output_src);
        exit_by_severity(&diag_result);
        return Ok(());
    }

    // Report-only mode: pass stdin source for span context rendering.
    // AD-211-1: use STDIN_DISPLAY_LABEL so span source frame reads "<stdin>".
    let named_source = if format == LintFormat::Human {
        Some((STDIN_DISPLAY_LABEL, source.as_str()))
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

    // effective_parent maps "" (bare filename) to "." — avoids PF-006.
    let base_dir = effective_parent(path);
    // mds.json load/parse failure → JSON envelope in --format json mode (AC-F-14).
    let config = match load_lint_config(base_dir, quiet) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            emit_analysis_failure_json_or_stderr(&mds_err, format, None);
            std::process::exit(2);
        }
    };
    // File read failure (not found, symlink, I/O) → JSON envelope in --format json mode (AC-F-14).
    let source = match read_source_file(path) {
        Ok(s) => s,
        Err(e) => {
            emit_analysis_failure_json_or_stderr(&e, format, None);
            std::process::exit(mds_error_exit_code(&e));
        }
    };
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<file>");

    let mut result = match mds::lint(path, runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(e) => {
            emit_analysis_failure_json_or_stderr(&e, format, None);
            std::process::exit(mds_error_exit_code(&e));
        }
    };
    // Remap the basename-only `file` label that mds::lint() sets → the display
    // filename so all four FixFileOutcome arms carry a consistent label.
    // Matches the explicit relabel in lint_one_file_accumulating and
    // lint_one_file_human; without this call the Rejected and NothingToFix
    // arms rely on mds::lint independently deriving the same basename — correct
    // today, but a silent coupling that would break if the two derivations ever
    // diverged.  Centralising the relabel here is the explicit invariant:
    // every FixFileOutcome carries `filename` as its display path.
    set_diag_display_path(&mut result, filename);

    // D4 (AD-216-11): status message, not an error — suppress under --quiet
    // (the global `--quiet` help text: "Suppress status and diagnostic output; errors
    // always print").  PF-004: single-file mode is a separate emitter from directory
    // mode; gating only the directory copy would re-create the #43/#173 divergence.
    if result.truncated && fix && !quiet {
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
        let fix_outcome =
            plan_and_apply_fixes(result, &source, base_dir, runtime_vars, &config, filename);
        match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                residual,
            } => {
                emit_result(format, &residual, quiet, named_source);
                atomic_write_file(path, &new_source)?;
                if !quiet {
                    eprintln!("Fixed: {}", safe_path(path));
                }
                exit_by_severity(&residual);
            }
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
                applied_count,
                total_count,
            } => {
                if !quiet {
                    eprintln!(
                        "Partially fixed: {} ({applied_count} of {total_count} fixes applied)",
                        safe_path(path)
                    );
                }
                emit_result(format, &residual, quiet, named_source);
                atomic_write_file(path, &new_source)?;
                exit_by_severity(&residual);
            }
            FixFileOutcome::Rejected { reason, original } => {
                // D4 (AD-216-11): status message — suppress under --quiet,
                // matching this function's own `PreviewOutcome::Rejected` arm and the
                // directory-mode emitters.  PF-004: single-file mode is a separate
                // emitter and must honour the same gate.
                if !quiet {
                    eprintln!("fix rejected: {}", safe_inline(&reason));
                }
                emit_result(format, &original, quiet, named_source);
                exit_by_severity(&original);
            }
            FixFileOutcome::NothingToFix { original } => {
                emit_result(format, &original, quiet, named_source);
                if !quiet && format == LintFormat::Human && original.diagnostics.is_empty() {
                    eprintln!("Clean: {}", safe_file_display(filename));
                }
                exit_by_severity(&original);
            }
        }
        return Ok(());
    }

    // ── Preview path: --fix --check and/or --fix --diff ─────────────────
    // Route preview through the same gated pipeline as the write path.
    // Previously called apply_plan_unchecked directly, bypassing the reverify gate —
    // a diff or check result could misrepresent what --fix would actually do.
    if fix && (check || diff) {
        let preview = preview_fixes(&result, &source, base_dir, runtime_vars, &config);
        match preview {
            PreviewOutcome::WouldFix(ref fixed) => {
                if diff {
                    let label = safe_path(path);
                    let diff_str = render_unified_diff(&source, fixed, &label);
                    let _ = write_stdout(&diff_str);
                }
                if check {
                    if !quiet {
                        eprintln!("Would fix: {}", safe_path(path));
                    }
                    std::process::exit(1);
                }
            }
            PreviewOutcome::Rejected(ref reason) => {
                // Surface the rejection reason so --fix --check is as honest as --fix.
                if !quiet {
                    eprintln!("fix rejected: {}", safe_inline(reason));
                }
            }
            PreviewOutcome::NothingToFix => {}
        }
        // After diff-only preview (or when nothing would change / fix rejected),
        // render diagnostics and exit by severity.
        emit_result(format, &result, quiet, named_source);
        exit_by_severity(&result);
        return Ok(());
    }

    // ── Report-only mode (no --fix) ───────────────────────────────────────────
    emit_result(format, &result, quiet, named_source);
    if !quiet && format == LintFormat::Human && result.diagnostics.is_empty() {
        eprintln!("Clean: {}", safe_file_display(filename));
    }
    exit_by_severity(&result);
    Ok(())
}

// ── Directory mode ────────────────────────────────────────────────────────────

/// Compile-time context for directory-mode lint.
///
/// Groups the parameters resolved once at the start of a directory lint run and
/// threaded into every per-file call — removes the `#[allow(clippy::too_many_arguments)]`
/// suppressions on `lint_one_file_accumulating` and `lint_one_file_human`
/// (issue #6 / zero-warnings policy). Pattern mirrors `FileCompileCtx` / `DirWatchCtx`
/// in watch.rs.
///
/// Two caches serve two independent fast paths into the config resolution logic.
/// They are kept SEPARATE so the key namespaces never collide: a directory that
/// happens to contain an `mds.json` would appear as a `base_dir` key in one run
/// and as a `config_dir` key in another, and a single map would conflate them.
///
/// - **Fast path 1** (`base_dir_cache`): if a file's parent directory has already
///   been resolved, return the cached config without re-walking the ancestor chain.
/// - **Fast path 2** (`config_dir_cache`): if a DIFFERENT `base_dir` resolves to
///   the SAME `mds.json`, return the cached config and skip emitting a duplicate
///   warning (AC-224-19: "at most once per distinct config directory").
///
/// NOTE: `load_config(base_dir)` is called BEFORE consulting `config_dir_cache`,
/// so the file I/O and ancestor walk still happen for each new `base_dir` — fast
/// path 2 prevents the DUPLICATE WARNING, not the re-parse. For a tree of N files
/// under one directory, fast path 1 amortises the cost to a single read (the common
/// case). The AC-224-19 threshold (200 files, one directory) is met by fast path 1.
///
/// The `RefCell` provides interior mutability so per-file helpers can populate the
/// caches through a shared `&LintDirCtx` reference.
struct LintDirCtx<'a> {
    lint_root: &'a Path,
    flags: LintFlags,
    runtime_vars: &'a Option<HashMap<String, mds::Value>>,
    /// Fast-path-1 cache: `base_dir → config`. Every directory whose files have
    /// been linted at least once is recorded here; a second file in the same
    /// directory returns immediately without calling `load_config`.
    base_dir_cache: RefCell<HashMap<PathBuf, Rc<mds::LintConfig>>>,
    /// Fast-path-2 cache: `config_dir → config`. Keyed by the RESOLVED directory
    /// that contains `mds.json`, not the file's parent. Prevents a duplicate
    /// unknown-rule warning when multiple `base_dir`s share one root `mds.json`.
    config_dir_cache: RefCell<HashMap<PathBuf, Rc<mds::LintConfig>>>,
}

impl<'a> LintDirCtx<'a> {
    /// Return the `LintConfig` for the directory `base_dir`.
    ///
    /// Consults `base_dir_cache` (fast path 1) and `config_dir_cache` (fast path 2)
    /// before loading from disk; see the struct doc for the two-cache design and the
    /// AC-224-19 rationale. On config-load failure returns `Err(MdsError::Io{..})` so
    /// the caller can record a per-file error and continue linting the rest of the tree.
    fn config_for(&self, base_dir: &Path) -> Result<Rc<mds::LintConfig>, MdsError> {
        // Fast path 1: base_dir was already resolved in a previous call (common case
        // for multiple files in the same directory — avoids the ancestor walk).
        {
            let cache = self.base_dir_cache.borrow();
            if let Some(cfg) = cache.get(base_dir) {
                return Ok(Rc::clone(cfg));
            }
        }

        // Walk the ancestor chain to find the mds.json governing this directory.
        // We call `load_config` directly (not `load_lint_config`) so we can inspect
        // the RESOLVED config directory before deciding whether to emit the warning:
        // multiple subdirectories can resolve to the same root mds.json, and
        // AC-224-19 requires the warning fires at most once per distinct config dir.
        let raw = load_config(base_dir).map_err(|e| MdsError::Io {
            message: format!("{e}"),
        })?;

        match raw {
            None => {
                // No mds.json found: use the default config, keyed by base_dir only.
                let rc = Rc::new(mds::LintConfig::default());
                self.base_dir_cache
                    .borrow_mut()
                    .insert(base_dir.to_path_buf(), Rc::clone(&rc));
                Ok(rc)
            }
            Some((mds_config, config_dir)) => {
                // Fast path 2: a different base_dir already resolved to this same
                // config directory (e.g. a/file.mds and b/file.mds both governed by
                // root/mds.json). Return the cached config without emitting a
                // duplicate warning (AC-224-19). Note: load_config above still ran
                // — fast path 2 suppresses the duplicate WARNING, not the re-parse.
                //
                // `config_dir_cache` and `base_dir_cache` are different RefCells, so
                // holding the shared borrow on `config_dir_cache` through the body is
                // safe — the body only mutably borrows `base_dir_cache`.
                if let Some(rc) = self
                    .config_dir_cache
                    .borrow()
                    .get(&config_dir)
                    .map(Rc::clone)
                {
                    // Alias base_dir → cached config so fast path 1 fires on the
                    // next call for a file in this same directory.
                    self.base_dir_cache
                        .borrow_mut()
                        .insert(base_dir.to_path_buf(), Rc::clone(&rc));
                    return Ok(rc);
                }

                // First load for this config_dir: build the config and emit the warning.
                // Contract documentation (safe_inline, --quiet, AC-224-3) lives at
                // `emit_unknown_rule_warning` — the single emitter shared with
                // load_lint_config (PF-009).
                let (lint_config, unknown) = mds_config.lint.into_core_config();
                if let Some(unknown) = unknown {
                    emit_unknown_rule_warning(&unknown, self.flags.quiet);
                }

                // Populate BOTH caches. `config_dir_cache` enables fast path 2 for
                // future base_dirs that resolve to this same mds.json; `base_dir_cache`
                // enables fast path 1 for future files in this same directory.
                let rc = Rc::new(lint_config);
                self.base_dir_cache
                    .borrow_mut()
                    .insert(base_dir.to_path_buf(), Rc::clone(&rc));
                self.config_dir_cache
                    .borrow_mut()
                    .insert(config_dir, Rc::clone(&rc));
                Ok(rc)
            }
        }
    }
}

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
///
/// **Directory summary (AD-216-3):** after processing all files, emits one summary
/// line to stderr in the form
/// `{clean} clean, {warn} with warnings, {error} with errors, {limit} resource-limited`.
/// "With errors" covers both error-severity lint findings AND per-file analysis failures
/// (read error, config error, lint call failure) — the same conflation `mds build`'s
/// "failed" bucket makes (D3-a).  "Resource-limited" counts files where `mds::lint`
/// returned `MdsError::ResourceLimit` — distinct from lint findings.
///
/// **Summary / quiet contract (AD-216-6):** the summary is suppressed under `--quiet`
/// unless at least one file is in the `error` or `resource-limited` bucket.  Warn-only
/// runs are silent under `--quiet` (mirrors `mds fmt`, which does not force its summary
/// on a `changed_count`-only run).  Exit codes are unaffected (AD-216-2).
///
/// **D1 decision (AC-Q14):** `mds lint --quiet <dir>` on a warn-only tree exits 1 with
/// no output.  `mds lint --fix --check --quiet <dir>` with pending fixes exits 1 with
/// no output.  Both are intentional: warnings are status output, which `main.rs:30`
/// says `--quiet` suppresses.
///
/// **D2 decision (AC-Q16):** the JSON stdout envelope (`{"files":…,"truncated":…,"version":1}`)
/// is unchanged — no `"summary"` key is added.  The summary is emitted to stderr only,
/// keeping stdout a single clean JSON document in all non-`--diff` modes.  (`--fix --diff`
/// is a legal invocation that writes the unified diff to stdout ahead of the envelope,
/// so `--fix --diff --format json` produces non-JSON-parseable stdout by design.)
///
/// **Channel discipline (AD-216-8):** the summary is emitted in BOTH human and JSON
/// format modes — format governs where machine-readable output goes (stdout), not
/// whether status output (stderr) appears.  JSON consumers using `--format json`
/// (without `--fix --diff`) continue to receive valid parseable JSON on stdout
/// regardless of stderr content; `--fix --diff` writes unified diffs to stdout
/// before the JSON envelope and is incompatible with JSON-consumer pipelines.
fn run_lint_directory(
    dir: &Path,
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
) -> Result<()> {
    const MAX_DEPTH: usize = 64;
    let LintFlags { quiet, format, .. } = flags;

    // A6/D20: config is now discovered per-file (each file walks up to its nearest
    // mds.json). The single root config load is removed; base_dir_cache and
    // config_dir_cache in LintDirCtx amortise repeated loads for files in the same
    // directory and across subdirectories sharing one root mds.json.

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, None);
    let mut files = walk.files;

    // AD-216-9: empty-dir early return emits no summary — the per-file loop never
    // runs, so all four counters stay at zero and there is nothing meaningful to
    // print.  The all-excluded diagnostic (below) bypasses --quiet and exits 2:
    // parity with `build`/`check`/`fmt` (a silent non-zero exit here would be a
    // bug, not a feature).
    if files.is_empty() {
        if walk.excluded_by_default > 0 {
            // Always emit — not suppressed by --quiet (avoids silent CI green pass).
            // Exit 2: usage error consistent with lint's exit-code table.
            eprintln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was linted",
                walk.excluded_by_default
            );
            std::process::exit(2);
        }
        if !quiet {
            eprintln!("No .mds files found in {}", safe_path(dir));
        }
        return Ok(());
    }

    // F1: sort by (sanitized_display_key, raw_os_path) so that:
    // 1. Array position is consistent with the sanitized `files[].file` key
    //    emitted by `to_canonical_json` for diagnostic entries (AC-P1-10).
    // 2. An OsString secondary key breaks any ties when two different non-UTF-8
    //    filenames produce the same `to_string_lossy` string — rare in practice,
    //    but ensures deterministic order regardless of readdir enumeration order.
    //
    // `Path::Ord` (component-wise) diverges from byte-order when a path-separator
    // character appears WITHIN a filename component — e.g. `api-utils.mds` sorts
    // AFTER `api/x.mds` under Path::Ord ("api" < "api-utils"), but BEFORE under
    // byte-wise string order ('-' = 0x2D < '/' = 0x2F).  Sorting on the relative
    // display string keeps the CLI wire contract consistent with the BTreeMap
    // ordering that `to_canonical_json` applies on the binding surfaces (PF-007).
    //
    // `relative_display` normalises to forward slashes so byte-wise order is
    // identical on Unix and Windows — the sort key and the emitted JSON `file`
    // key are the same String by construction, so array position matches key
    // order on both platforms.  `sanitize_control_chars_wire` is
    // then applied so the sort key matches the emitted key produced by
    // `to_canonical_json`: POSIX filenames may legally contain control bytes
    // (e.g. 0x01), and sorting on the raw (unsanitized) string would place a
    // control-byte filename at a position inconsistent with its `\uXXXX`-escaped
    // emitted key, violating AC-P1-10.  For the vast majority of paths (no control
    // bytes), `sanitize_control_chars_wire` returns `Cow::Borrowed` — no extra
    // heap allocation beyond the String conversion.
    //
    // `sort_by_cached_key` computes each key once — O(n) allocations, not O(n log n)
    // (AC-P1-22).
    files.sort_by_cached_key(|p| {
        (
            mds::sanitize_control_chars_wire(&relative_display(p, dir)).into_owned(),
            p.as_os_str().to_os_string(),
        )
    });

    let mut max_tally = FileTally::Clean;
    let mut json_files: Vec<serde_json::Value> = Vec::new();
    let mut any_truncated = false;

    let mut any_would_fix = false;

    // AD-216-3/5: four counters for the directory summary, one per FileTally variant.
    // Names are file-unique (print_discipline.rs limit 2, :106-110): `clean_count`,
    // `warn_file_count`, `error_file_count`, `limit_file_count` — none reuse an already-
    // exempted name from another summary line in this file.
    let mut clean_count: usize = 0;
    let mut warn_file_count: usize = 0;
    let mut error_file_count: usize = 0;
    let mut limit_file_count: usize = 0;

    let ctx = LintDirCtx {
        lint_root: dir,
        flags,
        runtime_vars: &runtime_vars,
        base_dir_cache: RefCell::new(HashMap::new()),
        config_dir_cache: RefCell::new(HashMap::new()),
    };

    for file in &files {
        let tally = if format == LintFormat::Json {
            lint_one_file_accumulating(
                file,
                &ctx,
                &mut json_files,
                &mut any_truncated,
                &mut any_would_fix,
            )
        } else {
            lint_one_file_human(file, &ctx, &mut any_truncated, &mut any_would_fix)
        };
        // AD-216-5: exhaustive match — a future FileTally variant becomes a compile
        // error here rather than being silently uncounted in the summary.
        match tally {
            FileTally::Clean => clean_count += 1,
            FileTally::WarnOnly => warn_file_count += 1,
            FileTally::Error => error_file_count += 1,
            FileTally::ResourceLimit => limit_file_count += 1,
        }
        if tally > max_tally {
            max_tally = tally;
        }
    }

    // Emit JSON envelope BEFORE any early exit so consumers always receive parseable
    // output regardless of exit code (AC-F-14 / issue #36).
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

    // AD-216-7: emit the summary AFTER the JSON envelope (so consumers always get a
    // parseable JSON object on stdout) and BEFORE the --fix --check exit (so a
    // --fix --check run that would exit 1 still prints the summary on the way out).
    //
    // AD-216-6: suppress under --quiet unless error- or resource-limited files are
    // present.  Warn-only runs are silent under --quiet (D1-a — mirrors fmt.rs:342:
    // `changed_count` does not force the summary under --quiet).
    //
    // AD-216-8: emitted in both human and JSON format modes.  --format governs the
    // machine-readable channel (stdout); the summary is status output (stderr) and is
    // governed only by --quiet.
    //
    // AD-216-4: format — `{clean} clean, {warn} with warnings, {error} with errors,
    // {limit} resource-limited`.  Fixed arity, comma-separated; matches the sibling
    // shape used by `build`/`check`/`fmt` and keeps the line greppable across all
    // summary-emitting subcommands.
    if !quiet || error_file_count > 0 || limit_file_count > 0 {
        eprintln!(
            "{clean_count} clean, {warn_file_count} with warnings, \
             {error_file_count} with errors, {limit_file_count} resource-limited"
        );
    }

    // --fix --check: exit 1 if any file would have been modified (accumulate-and-continue,
    // so every file is checked before exiting).
    if flags.check && any_would_fix {
        std::process::exit(1);
    }

    if max_tally.exit_code() != 0 {
        std::process::exit(max_tally.exit_code());
    }
    Ok(())
}

/// Lint one file in directory mode, accumulating results into a JSON array.
fn lint_one_file_accumulating(
    file: &Path,
    ctx: &LintDirCtx<'_>,
    json_files: &mut Vec<serde_json::Value>,
    any_truncated: &mut bool,
    any_would_fix: &mut bool,
) -> FileTally {
    // Bind quiet so the PartiallyFixed arm can honour it (issue #43 / #173).
    let LintFlags {
        fix,
        check,
        diff,
        quiet,
        ..
    } = ctx.flags;

    // Compute a display path relative to the lint root so JSON `file` keys
    // are navigable and unique across the whole directory tree (not just basenames).
    // `relative_display` normalises to forward slashes.  `to_canonical_json` then
    // sanitizes the key via `sanitize_control_chars_wire`; `run_lint_directory`
    // sorts on that same sanitized string, so emitted array order and emitted file
    // key order are consistent for all inputs including control-byte filenames
    // (AC-P1-10).
    let display_path = relative_display(file, ctx.lint_root);
    // Error-only entries (`{"file": …, "error": …}`) bypass `to_canonical_json`
    // and therefore bypass its `sanitize_control_chars_wire` pass.  Pre-sanitize
    // here so the `file` key in error entries is treated identically to the `file`
    // key in diagnostic entries — hostile filenames cannot inject control, bidi,
    // or separator characters into either entry type (spec.md §lint-json `file`
    // contract; ADR-008).
    let file_key = mds::sanitize_control_chars_wire(&display_path).into_owned();

    // `source` is only consumed in the fix branch (below); the report-only/JSON
    // path does not need it — mds::lint() reads the file independently (I-06).
    // effective_parent maps "" (bare filename) to "." — avoids PF-006.
    let base_dir = effective_parent(file);

    // A6: per-file config discovery — load (and cache) the nearest mds.json for
    // this file's directory. A config-load failure is a per-file error; the rest
    // of the tree continues linting.
    let config = match ctx.config_for(base_dir) {
        Ok(c) => c,
        Err(ref e) => {
            json_files.push(serde_json::json!({
                "file": file_key,
                "error": e.serialize()
            }));
            return FileTally::Error;
        }
    };

    let mut result = match mds::lint(file, ctx.runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(ref e) => {
            json_files.push(serde_json::json!({
                "file": file_key,
                "error": e.serialize()
            }));
            return if matches!(e, MdsError::ResourceLimit { .. }) {
                FileTally::ResourceLimit
            } else {
                FileTally::Error
            };
        }
    };
    // Remap basename-only file field → relative display path.
    set_diag_display_path(&mut result, &display_path);

    if result.truncated {
        *any_truncated = true;
        // D4 (AD-216-11): this is a status message, not an error — suppress
        // under --quiet (main.rs:30 "Suppress status and diagnostic output").
        if fix && !quiet {
            eprintln!(
                "{}: diagnostic cap ({}) reached; further findings were suppressed — \
                 re-run --fix to continue",
                safe_path(file),
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
                    "file": file_key,
                    "error": e.serialize()
                }));
                return FileTally::Error;
            }
        };
        let fix_outcome = plan_and_apply_fixes(
            result,
            &source,
            base_dir,
            ctx.runtime_vars.clone(),
            &config,
            &display_path,
        );
        match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                residual,
            } => {
                accumulate_result_json(&residual, json_files);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {}", safe_path(file), safe_inline(&e));
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
                applied_count,
                total_count,
            } => {
                // Unified message + quiet guard (issue #43 / #173).
                if !quiet {
                    eprintln!(
                        "Partially fixed: {} ({applied_count} of {total_count} fixes applied)",
                        safe_path(file)
                    );
                }
                accumulate_result_json(&residual, json_files);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {}", safe_path(file), safe_inline(&e));
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::Rejected { reason, original } => {
                // D4 (AD-216-11): "fix rejected" is a status message — the safety
                // gate fired, the original diagnostics remain unmodified.  Suppress under
                // --quiet (main.rs:30).  The residual lint findings (and the exit code)
                // are unaffected: this message describes the --fix attempt, not the findings.
                if !quiet {
                    eprintln!(
                        "{}: fix rejected: {}",
                        safe_path(file),
                        safe_inline(&reason)
                    );
                }
                accumulate_result_json(&original, json_files);
                tally_from_result(&original)
            }
            FixFileOutcome::NothingToFix { original } => {
                accumulate_result_json(&original, json_files);
                tally_from_result(&original)
            }
        }
    } else if fix && (check || diff) {
        // Directory-mode preview — route through gated pipeline.
        let source = match read_source_file(file) {
            Ok(s) => s,
            Err(e) => {
                json_files.push(serde_json::json!({
                    "file": file_key,
                    "error": e.serialize()
                }));
                return FileTally::Error;
            }
        };
        match preview_fixes(
            &result,
            &source,
            base_dir,
            ctx.runtime_vars.clone(),
            &config,
        ) {
            PreviewOutcome::WouldFix(ref fixed) => {
                *any_would_fix = true;
                if diff {
                    let label = safe_path(file);
                    let diff_str = render_unified_diff(&source, fixed, &label);
                    let _ = write_stdout(&diff_str);
                }
            }
            PreviewOutcome::Rejected(ref reason) => {
                // D4 (AD-216-11): status message — suppress under --quiet.
                if !quiet {
                    eprintln!("{}: fix rejected: {}", safe_path(file), safe_inline(reason));
                }
            }
            PreviewOutcome::NothingToFix => {}
        }
        accumulate_result_json(&result, json_files);
        tally_from_result(&result)
    } else {
        accumulate_result_json(&result, json_files);
        tally_from_result(&result)
    }
}

/// Lint one file in directory mode, rendering diagnostics to stderr (human mode).
fn lint_one_file_human(
    file: &Path,
    ctx: &LintDirCtx<'_>,
    any_truncated: &mut bool,
    any_would_fix: &mut bool,
) -> FileTally {
    let LintFlags {
        fix,
        check,
        diff,
        quiet,
        ..
    } = ctx.flags;

    // Compute a display path relative to the lint root for human rendering.
    // `relative_display` normalises to forward slashes, matching the unsanitized
    // base used by `run_lint_directory`'s sort (AC-P1-10).  Human rendering
    // shows the real filename bytes rather than sanitized `\uXXXX` escapes.
    let display_path = relative_display(file, ctx.lint_root);

    let source = match read_source_file(file) {
        Ok(s) => s,
        Err(e) => {
            eprint_error(miette::Report::from(e));
            return FileTally::Error;
        }
    };

    // effective_parent maps "" (bare filename) to "." — avoids PF-006.
    let base_dir = effective_parent(file);

    // A6: per-file config discovery — load (and cache) the nearest mds.json for
    // this file's directory. A config-load failure is a per-file error; the rest
    // of the tree continues linting.
    let config = match ctx.config_for(base_dir) {
        Ok(c) => c,
        Err(e) => {
            eprint_error(miette::Report::from(e));
            return FileTally::Error;
        }
    };

    // Named source for span rendering: relative display path + source text.
    let named_source = (display_path.as_str(), source.as_str());

    let mut result = match mds::lint(file, ctx.runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(ref e) => {
            eprint_error(miette::Report::from(e.clone()));
            return if matches!(e, MdsError::ResourceLimit { .. }) {
                FileTally::ResourceLimit
            } else {
                FileTally::Error
            };
        }
    };
    // Remap basename-only file field → relative display path.
    set_diag_display_path(&mut result, &display_path);

    if result.truncated {
        *any_truncated = true;
        // D4 (AD-216-11): this is a status message, not an error — suppress
        // under --quiet (main.rs:30 "Suppress status and diagnostic output").
        if fix && !quiet {
            eprintln!(
                "{}: diagnostic cap ({}) reached; further findings were suppressed — \
                 re-run --fix to continue",
                safe_path(file),
                mds::MAX_DIAGNOSTICS
            );
        }
    }

    if fix && !check && !diff {
        let fix_outcome = plan_and_apply_fixes(
            result,
            &source,
            base_dir,
            ctx.runtime_vars.clone(),
            &config,
            &display_path,
        );
        match fix_outcome {
            FixFileOutcome::Fixed {
                new_source,
                residual,
            } => {
                render_result_human(&residual, quiet, named_source);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {}", safe_path(file), safe_inline(&e));
                    return FileTally::Error;
                }
                if !quiet {
                    eprintln!("Fixed: {}", safe_path(file));
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::PartiallyFixed {
                new_source,
                residual,
                applied_count,
                total_count,
            } => {
                // Unified message format (issue #43 / #173).
                if !quiet {
                    eprintln!(
                        "Partially fixed: {} ({applied_count} of {total_count} fixes applied)",
                        safe_path(file)
                    );
                }
                render_result_human(&residual, quiet, named_source);
                if let Err(e) = atomic_write_file(file, &new_source) {
                    eprintln!("error writing {}: {}", safe_path(file), safe_inline(&e));
                    return FileTally::Error;
                }
                tally_from_result(&residual)
            }
            FixFileOutcome::Rejected { reason, original } => {
                // D4 (AD-216-11): "fix rejected" is a status message — the safety
                // gate fired, the original diagnostics remain unmodified.  Suppress under
                // --quiet (main.rs:30).
                if !quiet {
                    eprintln!(
                        "{}: fix rejected: {}",
                        safe_path(file),
                        safe_inline(&reason)
                    );
                }
                render_result_human(&original, quiet, named_source);
                tally_from_result(&original)
            }
            FixFileOutcome::NothingToFix { original } => {
                render_result_human(&original, quiet, named_source);
                tally_from_result(&original)
            }
        }
    } else if fix && (check || diff) {
        // Directory-mode preview — route through gated pipeline.
        match preview_fixes(
            &result,
            &source,
            base_dir,
            ctx.runtime_vars.clone(),
            &config,
        ) {
            PreviewOutcome::WouldFix(ref fixed) => {
                *any_would_fix = true;
                if diff {
                    let label = safe_path(file);
                    let diff_str = render_unified_diff(&source, fixed, &label);
                    let _ = write_stdout(&diff_str);
                }
                if check && !quiet {
                    eprintln!("Would fix: {}", safe_path(file));
                }
            }
            PreviewOutcome::Rejected(ref reason) => {
                if !quiet {
                    eprintln!("{}: fix rejected: {}", safe_path(file), safe_inline(reason));
                }
            }
            PreviewOutcome::NothingToFix => {}
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
        render_result_human(
            result,
            quiet,
            named_source.expect("Human format requires named_source"),
        );
    }
}

/// AD-211-5 (2026-08-12 ruling): this envelope is the single CLI choke-point for
/// **lint's** analysis failures (config load, IO, resolution, parse).  When
/// `stdin_source` is `Some(source_text)` the embedded source identity in the rendered
/// output is replaced with [`STDIN_DISPLAY_LABEL`], so every CLI diagnostic context
/// for stdin input uses the uniform sentinel instead of the core's internal
/// `SOURCE_LABEL` (`"<source>"`) that `resolve_source_intrinsic` embeds in `MdsError`
/// spans.
///
/// **State it as a rule about this envelope, not about stdin:** every
/// `MdsError` reaching this function for a stdin run labels its source `<stdin>`.
/// Any error later routed here — a config rejection, a new IO failure — inherits
/// that label instead of inventing a second convention.
///
/// The JSON leg needs no relabel and takes none: `MdsError::serialize()` emits
/// `code` / `message` / `help` / `span`, and no `MdsError` `Display` template
/// interpolates `ctx.file_str`, so the source identity never reaches
/// `error.message`.  `cli_lint.rs::stdin_analysis_failure_labels_source_as_stdin`
/// pins that on both channels rather than leaving it as an assumption.
///
/// For errors from a file source, pass `stdin_source: None`; the error's embedded
/// `NamedSource` (which already carries the correct filename) is used as-is.
///
/// JSON format → stdout envelope; human → stderr via miette.
fn emit_analysis_failure_json_or_stderr(
    e: &MdsError,
    format: LintFormat,
    stdin_source: Option<&str>,
) {
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
        // Route through the single render choke point (avoids PF-004 /
        // architecture-6: hand-rolled sanitize_control_chars bypass).
        let report = match stdin_source {
            Some(src) => relabel_stdin_error(e, src),
            None => miette::Report::from(e.clone()),
        };
        eprint_error(report);
    }
}

/// Extract and accumulate per-file JSON entries from a `LintResult` into `json_files`.
fn accumulate_result_json(result: &mds::LintResult, json_files: &mut Vec<serde_json::Value>) {
    let inner = result.to_canonical_json();
    if let Some(arr) = inner["files"].as_array() {
        json_files.extend(arr.iter().cloned());
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{preview_fixes, PreviewOutcome};
    use mds::{FixLineSpan, LintDiagnostic, LintResult, Severity};
    use std::path::Path;

    /// ISS-13: CLI-level genuine partial overlaps are structurally impossible with
    /// the current 10 rules.  Every pair of real-rule fix spans is either disjoint
    /// (different AST blocks) or in a containment relationship (same block, different
    /// rules) — the latter is resolved by `dedup_contained_or_identical` before the
    /// overlap detector is reached.  No MDS fixture can drive `overlap_rejected=true`
    /// through the CLI binary.
    ///
    /// This unit test covers rejection surfacing at the deepest level reachable from
    /// the CLI crate: `preview_fixes` with a crafted `LintResult` whose `fix_removals`
    /// produce the same genuine partial overlap used in the mds-core ISS-02 regression
    /// test (fix.rs: `a4_partial_overlap_still_rejected_after_dedup`).
    ///
    /// Math (identical to ISS-02):
    ///   source = "line0\nline1\nline2\n"
    ///   Edit A: FixLineSpan::range_inclusive(0, 6)  → ByteEdit [0,  12)
    ///   Edit B: FixLineSpan::range_inclusive(6, 12) → ByteEdit [6,  18)
    ///   A.end=12 > B.start=6, B.end=18 > A.end=12 → partial overlap → overlap_rejected.
    ///
    /// `preview_fixes` must return `PreviewOutcome::Rejected` — the rejection must be
    /// surfaced (not silently swallowed as `NothingToFix`).  This pins PF-004: the
    /// preview path uses the same gated pipeline as the write path and is equally
    /// honest about overlap refusals.
    #[test]
    fn preview_fixes_surfaces_rejected_on_overlap() {
        let source = "line0\nline1\nline2\n";
        // Edit A covers bytes [0, 12): line0 start through line1 end (inclusive).
        let diag_a = LintDiagnostic::new("duplicate-import", Severity::Error, "a")
            .with_fix_removals(vec![FixLineSpan::range_inclusive(
                0, // inside line0
                6, // inside line1; extend_to_line_end(6) = 12
            )]);
        // Edit B covers bytes [6, 18): line1 start through line2 end (inclusive).
        // Partially overlaps A at [6, 12).
        let diag_b =
            LintDiagnostic::new("empty-block", Severity::Warn, "b").with_fix_removals(vec![
                FixLineSpan::range_inclusive(
                    6,  // inside line1
                    12, // inside line2; extend_to_line_end(12) = 18
                ),
            ]);
        let result = LintResult::new(vec![diag_a, diag_b]);

        let outcome = preview_fixes(
            &result,
            source,
            Path::new("."),
            None,
            &mds::LintConfig::default(),
        );

        assert!(
            matches!(outcome, PreviewOutcome::Rejected(_)),
            "preview_fixes must return Rejected for an overlap_rejected plan, \
             not NothingToFix or WouldFix (PF-004 — preview must be as honest as apply)"
        );
    }

    /// Regression: `relative_display` must NOT treat a literal backslash in a
    /// Unix filename as a path separator.
    ///
    /// On Unix, POSIX forbids only `/` and NUL in filenames; `\` is an ordinary
    /// byte.  The old `to_string_lossy().replace('\\', "/")` implementation
    /// turned `sub/..\..\etc\evil.mds` into `sub/../../../etc/evil.mds`,
    /// providing a directory-traversal vector (CWE-22/CWE-41) and causing key
    /// collisions on the published lint JSON wire surface when two distinct files
    /// (e.g. `a/b.mds` and `a\b.mds`) were linted together.
    ///
    /// The new `Path::components()` join preserves `\` as a literal filename
    /// byte on Unix and normalises it to a separator on Windows, which is the
    /// correct platform-aware behaviour.
    ///
    /// Path construction uses Rust string literals containing a backslash byte
    /// (0x5C) — not a control byte, so the Source hygiene gate does not flag it.
    #[cfg(unix)]
    #[test]
    fn relative_display_preserves_literal_backslash_on_unix() {
        use super::relative_display;
        use std::path::Path;

        let root = Path::new("/lint-root");

        // A real subdirectory: /lint-root/a/b.mds  (two path components under root)
        let real_subdir = Path::new("/lint-root/a/b.mds");

        // A top-level file whose NAME contains a literal backslash: /lint-root/a\b.mds
        // On Unix the backslash is just a filename byte; Path treats this as ONE
        // component under root (not two).  The string "a\\b.mds" in Rust source
        // is the byte sequence a, 0x5C, b, ., m, d, s — no control bytes.
        let backslash_name = Path::new("/lint-root/a\\b.mds");

        let display_subdir = relative_display(real_subdir, root);
        let display_backslash = relative_display(backslash_name, root);

        assert_eq!(
            display_subdir, "a/b.mds",
            "real subdirectory path should emit forward-slash-separated key"
        );
        assert_eq!(
            display_backslash, "a\\b.mds",
            "a literal backslash filename byte must be preserved in the emitted key on Unix"
        );
        assert_ne!(
            display_subdir, display_backslash,
            "a literal-backslash filename must not collide with the same letters \
             separated by a real slash (was broken by the old .replace() approach)"
        );
    }

    /// Regression: the directory-mode sort key must be the SANITIZED display path
    /// so that sort position matches the sanitized `files[].file` key emitted by
    /// `to_canonical_json` for diagnostic entries, including control-byte filenames
    /// (AC-P1-10).
    ///
    /// Without sanitization: a file whose name begins with 0x01 (a C0 control byte)
    /// sorts BEFORE "P.mds" in the raw byte order (0x01 < 0x50), but its sanitized
    /// emitted key starts with `\` (0x5C, the JSON-escape prefix for byte 0x01),
    /// placing it AFTER "P.mds" in emitted-key order — violating AC-P1-10.
    ///
    /// With the sanitized sort key the two orderings agree: "P.mds" (emitted "P.mds")
    /// sorts before the control-byte file (whose JSON-emitted key starts with `\`) in both
    /// the array position and the emitted key comparison.
    ///
    /// The control byte is constructed at runtime via char::from(1u8) so that no
    /// literal control byte or \uXXXX escape appears in the source file (PF-018 /
    /// Source hygiene gate).
    #[cfg(unix)]
    #[test]
    fn sort_key_sanitizes_control_byte_filenames() {
        use super::relative_display;
        use std::path::Path;

        let root = Path::new("/lint-root");

        // Build a filename starting with byte 0x01 at runtime — not as a literal
        // control byte in source (PF-018).
        let ctrl_char = char::from(1u8); // U+0001, a C0 control character
        let ctrl_filename = format!("{ctrl_char}a.mds");
        let ctrl_path_buf = root.join(&ctrl_filename);
        let ctrl_path: &Path = &ctrl_path_buf;
        let normal_path = Path::new("/lint-root/P.mds");

        let ctrl_raw = relative_display(ctrl_path, root);
        let normal_raw = relative_display(normal_path, root);

        // Raw (unsanitized) order: 0x01 < 'P' (0x50) → control-byte file sorts first.
        assert!(
            ctrl_raw < normal_raw,
            "raw display: control-byte path ({ctrl_raw:?}) must sort before P.mds by \
             unsanitized byte order (confirms old sort would have been wrong)"
        );

        // Sanitized sort keys: byte 0x01 becomes a JSON escape starting with '\' (0x5C).
        // 0x5C > 'P' (0x50), so P.mds sorts first — matching the emitted key order.
        let ctrl_sort_key = mds::sanitize_control_chars_wire(&ctrl_raw).into_owned();
        let normal_sort_key = mds::sanitize_control_chars_wire(&normal_raw).into_owned();

        assert!(
            normal_sort_key < ctrl_sort_key,
            "sanitized sort key: P.mds ({normal_sort_key:?}) must sort before \
             control-byte file ({ctrl_sort_key:?}) — matching the emitted key order \
             (AC-P1-10)"
        );
    }
}
