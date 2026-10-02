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
//! Each input is a [`LintSource`], which gives it the names lint shows for it.
//!
//! # Channel discipline
//!
//! - Human diagnostics → **stderr** via miette Report.
//! - `--format json` output → **stdout** only: exactly one JSON object, with a trailing
//!   newline, on every exit but a usage error clap reports — the findings document, or the
//!   error document of a failure that stops the run; `--fix --diff` writes its diffs
//!   first (#309).
//! - `--quiet` suppresses warning+info human diagnostics and summaries, NOT errors.
//!
//! # Results
//!
//! Linting an input prints nothing: [`lint_input`] works out a [`FileReport`] (#173), and
//! [`render`] shows it through the format's [`ResultSink`] (`lint_sink.rs`), which holds
//! every status line and document lint shows for its inputs (#309). A run's exit code is
//! [`run_lint`]'s return value, which `main` exits with. `tests/print_discipline.rs` holds
//! lint's writer-macro calls, stdout writes and exits out of this file.
//!
//! # Diagnostic path anchors (directory-mode divergence, R3 / CWE-209)
//!
//! Two path families appear in lint output and deliberately use different
//! anchor directories in directory mode:
//!
//! - **Rule diagnostics** display *walk-root-relative* paths: `diag.file` is
//!   overwritten via [`set_diag_display_path`] with the path relative to the
//!   linted directory (the walk root), keeping per-file output stable and
//!   sortable within one run.
//! - **Analysis-failure frames** (parse/resolve/IO/config-load errors) display
//!   *project-root-relative* paths: mds-core relativizes `NamedSource` names and
//!   message paths at construction time against the `.mdsroot` / `.git` walk-up
//!   root (R3), and [`read_source_file`] anchors the same root for raw-read
//!   errors.
//!
//! The two anchors differ, but both are safe: neither surface ever shows an
//! absolute path (basename fallback outside the root).
//!
//! # Exit codes (returned by [`run_lint`] for `main`'s exit funnel, never `main`'s `exit_code()`)
//!
//! - 0: clean (no Warn/Error findings)
//! - 1: warning-severity findings only, no errors
//! - 2: any error-severity finding OR analysis failure (parse/resolve/IO/config-load)
//!   OR usage error
//! - 3: ResourceLimit
//!
//! With `--fix`, residual post-fix findings determine the exit code.
//!
//! A closed stdout or stderr never changes the code. A stdout write that fails for
//! another reason — the JSON report, a diff, the fixed source — is reported once as
//! `mds::io`, and the funnel lifts the code to at least 2; in directory mode a file whose
//! diff it lost counts under "with errors" (#157).
//!
//! A panic ends the run with 101 through the funnel (#389). A panic in an entry's
//! analysis — `mds::lint` on a directory's entry, or the fix pipeline — fails that input
//! alone: a directory counts it under "with errors" and goes on to its other entries.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mds::{effective_parent, FileSystem, MdsError, NativeFs, Severity};
use miette::Result;

use crate::build::{
    build_runtime_vars, emit_duplicate_var_warnings, ensure_existing_mds_file, load_config,
    read_stdin, resolve_input, ProjectConfig, RuntimeVarArgs,
};
use crate::lint_sink::{HumanSink, JsonSink, ResultSink};
use crate::output::{
    catch_compile, collect_mds_files_detailed, eprint_warning, render_unified_diff, safe_inline,
    safe_path, Panicked, RootPaths, WriteTarget, STDIN_DISPLAY_LABEL,
};
use crate::write::{atomic_write_file, Durability, Parents};

// AC-224-15: No local rule-name list. The single source of truth is
// mds::KNOWN_LINT_RULES (composed from each rule module's own RULE const).
// No rule-name string literals from the registry appear in this directory.

/// Why `mds lint --fix --format json -` is refused (`mds::io`, exit 2).
const STDIN_FIX_JSON_REFUSAL: &str = "--fix --format json with stdin input is not supported; \
     use `mds lint --fix -` for filter mode or `mds lint --format json` for JSON output";

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

/// One input of `mds lint`, and the names lint shows for it (#173).
///
/// Stdin, a file argument and each file of a directory get their names here, so the
/// three modes cannot drift into three conventions:
///
/// - [`display_label`](Self::display_label) — the name the input's diagnostics carry:
///   each `diag.file`, hence the JSON `files[].file` key, and the name a human
///   diagnostic frame renders the source under.
/// - [`diff_label`](Self::diff_label) — the name a `--fix --diff` header shows.
///
/// The status lines that name the input — `Clean:`, `Fixed:`, `Partially fixed:`,
/// `Would fix:`, and in a directory run `fix rejected:` and the cap notice — show the
/// same text as the diff header, the path as typed or walked (#390), but the result sink
/// (`lint_sink.rs`) spells it inside the writer macro, as [`STDIN_DISPLAY_LABEL`] or
/// `safe_path` of the path, because the print-discipline guard accepts only an escape call
/// or an allowlisted name there.
pub(crate) enum LintSource<'a> {
    /// `mds lint -`: the source comes from stdin and has no path.
    Stdin,
    /// `mds lint <file>`: the path as typed, and its file name.
    File { typed: &'a Path, name: &'a str },
    /// A file under `mds lint <dir>`: the directory argument, the path the walk produced
    /// below it, and its key relative to it (see [`relative_display`]). A `--fix` rewrite
    /// is anchored at `root` (#160).
    DirEntry {
        root: &'a Path,
        path: &'a Path,
        key: &'a str,
    },
}

impl<'a> LintSource<'a> {
    /// A file argument, named by its file name.
    ///
    /// `mds::io` when the path has no file name in UTF-8, which cannot happen once
    /// `mds::lint` has accepted the path: [`ensure_existing_mds_file`] accepted it, so it
    /// ends in a file name with the `.mds` extension, and `mds::lint` refuses a path that
    /// is not valid UTF-8 before it reads anything. [`read_source_file`] is not enough:
    /// it validates the canonical path it opens, not the path as typed.
    fn file(typed: &'a Path) -> std::result::Result<Self, MdsError> {
        let name = typed
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| MdsError::Io {
                message: format!(
                    "path has no UTF-8 file name: {}",
                    mds::escape_path_for_message(&typed.to_string_lossy())
                ),
            })?;
        Ok(Self::File { typed, name })
    }

    /// The name the input's diagnostics carry: [`STDIN_DISPLAY_LABEL`] for stdin, the
    /// file name of a file argument, the root-relative key of a directory entry.
    pub(crate) fn display_label(&self) -> &'a str {
        match *self {
            Self::Stdin => STDIN_DISPLAY_LABEL,
            Self::File { name, .. } => name,
            Self::DirEntry { key, .. } => key,
        }
    }

    /// The name mds-core linted the input under, which decides whether it is a partial:
    /// [`mds::STRING_SOURCE_MAP_LABEL`] for stdin (`mds::lint_str_with`'s name, never a
    /// partial), and a file's file name — the last component of the path `mds::lint` was
    /// given — for a file argument or a directory's entry. The [`ReverifyGate`] lints
    /// every fix candidate under it (#309).
    fn lint_name(&self) -> &'a str {
        match *self {
            Self::Stdin => mds::STRING_SOURCE_MAP_LABEL,
            Self::File { name, .. } => name,
            // The key joins the entry's path components below the directory argument
            // with `/`, so its last segment is the path's file name.
            Self::DirEntry { key, .. } => key.rsplit_once('/').map_or(key, |(_, name)| name),
        }
    }

    /// The path the input is read from, as the user reaches it: `-` for stdin, the path
    /// as typed for a file argument, the path the walk produced for a directory's entry.
    fn path(&self) -> &'a Path {
        match *self {
            Self::Stdin => Path::new("-"),
            Self::File { typed, .. } => typed,
            Self::DirEntry { path, .. } => path,
        }
    }

    /// The name a unified-diff header shows, escaped for a terminal:
    /// [`STDIN_DISPLAY_LABEL`] for stdin, otherwise the path as typed or walked.
    fn diff_label(&self) -> Cow<'a, str> {
        match *self {
            Self::Stdin => Cow::Borrowed(STDIN_DISPLAY_LABEL),
            Self::File { typed, .. } => Cow::Owned(safe_path(typed)),
            Self::DirEntry { path, .. } => Cow::Owned(safe_path(path)),
        }
    }
}

/// Entry point for `mds lint`: run it, showing its results through the format's
/// [`ResultSink`], and return the exit code, which `main` exits with (#309).
pub(crate) fn run_lint(args: LintArgs) -> i32 {
    let quiet = args.quiet;
    match args.format {
        LintFormat::Human => lint_through(args, &mut HumanSink::new(quiet)),
        LintFormat::Json => lint_through(args, &mut JsonSink::new(quiet)),
    }
}

/// [`run_lint`] through `sink`: a lint-specific exit code, or the exit of a setup failure,
/// which the sink shows — as for an analysis failure, 3 for a resource limit (a `--vars`
/// file over the size cap, #309) and 2 for any other error.
fn lint_through(args: LintArgs, sink: &mut impl ResultSink) -> i32 {
    match do_lint(args, sink) {
        Ok(code) => code,
        Err(e) => {
            let code = e.downcast_ref::<MdsError>().map_or(2, mds_error_exit_code);
            sink.setup_failed(e);
            code
        }
    }
}

/// Inner runner — the run's exit code; every setup error propagates as `Err`, which
/// [`lint_through`] shows.
fn do_lint(args: LintArgs, sink: &mut impl ResultSink) -> Result<i32> {
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

    let resolved = match build_runtime_vars(RuntimeVarArgs {
        vars,
        set_vars,
        set_string_vars,
    }) {
        Ok(r) => r,
        Err(e) => {
            // Scope: EXACTLY this one variant, nothing wider.
            // A --set/--set-string collision is a usage error over runtime
            // VARIABLES, not an analysis failure: build/check/watch exit 1 for
            // it via `exit_code()`, so lint must not blanket-exit 2 for the
            // same mistake.  Downcast BEFORE any render call (`eprint_error`
            // consumes the report).  Shown as an analysis failure, it gives
            // --format json consumers the structured `mds::var_conflict` envelope.
            // Every OTHER setup error exits through `lint_through`: 3 for a
            // resource limit, 2 for any other.
            if let Some(mds_err @ MdsError::VarConflict { .. }) = e.downcast_ref::<MdsError>() {
                sink.analysis_failure(mds_err, None);
                return Ok(1);
            }
            return Err(e);
        }
    };
    emit_duplicate_var_warnings(&resolved, quiet);
    let runtime_vars = resolved.vars;

    let (input, _auto_detected) = resolve_input(input, "lint")?;

    // `mds lint --fix -` is a filter, whose product is the source on stdout, so
    // `--format json` refuses it — shown as an analysis failure, the error document being
    // the run's one document (#309). Stdin is not read.
    if fix && format == LintFormat::Json && input == Path::new("-") {
        let refusal = MdsError::Io {
            message: STDIN_FIX_JSON_REFUSAL.to_string(),
        };
        return Ok(analysis_failed(sink, &refusal, None));
    }

    // Stdin mode.
    if input == Path::new("-") {
        return Ok(run_lint_stdin(flags, runtime_vars, sink));
    }

    // Directory mode.
    if input.is_dir() {
        // #413: the one directory-argument check every directory-mode subcommand makes
        // (a symlink, the filesystem root, a forbidden character — all `mds::io`). A
        // refusal is an analysis failure: the JSON envelope in --format json mode, exit 2.
        return Ok(match crate::input::resolve_directory_argument(&input) {
            Ok(_) => run_lint_directory(&input, flags, runtime_vars, sink),
            Err(mds_err) => {
                sink.analysis_failure(&mds_err, None);
                2
            }
        });
    }

    // Single-file mode.
    // Check existence first, then extension (C4/F6): a non-existent path must report
    // mds::file_not_found, not mds::not_mds, regardless of the extension.
    // Shown as an analysis failure, so --format json produces the error envelope. Do NOT
    // use `?` here.
    if let Err(mds_err) = ensure_existing_mds_file(&input) {
        return Ok(analysis_failed(sink, &mds_err, None));
    }
    Ok(run_lint_file(&input, flags, runtime_vars, sink))
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
        Some(ProjectConfig {
            config: mds_config, ..
        }) => {
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
/// [`lint_input`] calls this on every input's findings, and [`run_fix_pipeline`] on every
/// residual, with the input's [`LintSource::display_label`]. For stdin that is `<stdin>`, so `diag.file` in
/// the JSON wire output reads `"<stdin>"` rather than the internal VFS key
/// `"input.mds"` (`STRING_SOURCE_MAP_LABEL`).
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
/// Error-only entries (`{"file": …, "error": …}`) bypass `to_canonical_json`
/// and therefore its own `sanitize_control_chars_wire` pass; they are instead
/// pre-sanitized by the result sink's `error_entry` — so both diagnostic and
/// error-only entry types carry identically-sanitized `file` values (ADR-008).
///
/// Forward slashes (`/`, 0x2F) are used instead of
/// the native backslash separator (`\`, 0x5C) because bytes in the range
/// 0x30–0x5B (digits `0`–`9`, uppercase letters `A`–`Z`, and `[`) sort between
/// `/` and `\`; on Windows a flat file like `sub[abc.mds` would sort BEFORE a
/// nested path `sub\d.mds` using the native separator (0x5B < 0x5C), but AFTER
/// it with the emitted forward slash (0x5B > 0x2F), reversing the array order
/// relative to the emitted key order.
///
/// # Fail-closed contract (#217)
///
/// The returned string is the directory-mode `files[].file` key and the sort key.
/// A path that is not under `root`, that yields a non-`Normal` component after
/// the strip, or that is not valid UTF-8 is an `Io` error — never a lossy
/// (U+FFFD) or absolute key.
fn relative_display(path: &Path, root: &Path) -> std::result::Result<String, MdsError> {
    let escapes = || MdsError::Io {
        message: format!(
            "path escapes lint root {}: {}",
            safe_path(root),
            safe_path(path)
        ),
    };
    let rel = path.strip_prefix(root).map_err(|_| escapes())?;
    let mut out = String::new();
    for c in rel.components() {
        // Only `Normal` can follow a successful strip of a read_dir entry; RootDir /
        // Prefix / ParentDir here means the invariant above is broken — reject rather
        // than join them into `//foo` (Unix) or `C:/\/foo` (Windows) or drop a `..`.
        let std::path::Component::Normal(name) = c else {
            return Err(escapes());
        };
        // Same wording as `read_source_file`'s non-UTF-8 rejection, which is what a
        // per-file read of this entry would have produced before the run-level check.
        // Escaped: every key is built before any entry is read, so no per-file refusal
        // of a forbidden character stands in front of this message (#390).
        let name = name.to_str().ok_or_else(|| MdsError::Io {
            message: format!("path is not valid UTF-8: {}", safe_path(path)),
        })?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(name);
    }
    Ok(out)
}

// ── Read source file ──────────────────────────────────────────────────────────

/// Read raw source of `path`: symlink-checked, size-capped and UTF-8-validated.
///
/// Shared by `mds lint` and `mds fmt`, which both need the RAW source text rather
/// than a compiled result. Returns `MdsError` (not `miette::Error`) so callers can
/// show the error as an analysis failure — the JSON envelope under `--format json` —
/// without downcasting.
pub(crate) fn read_source_file(path: &Path) -> std::result::Result<String, MdsError> {
    let canonical = NativeFs::check_symlink(path)?;
    read_canonical_source(&canonical, path)
}

/// Read the already symlink-checked `canonical` path of `path` through `NativeFs`.
///
/// R3 / CWE-209: the display root (project-root walk-up from the file's
/// directory) is anchored BEFORE `read()`, so read-error messages show a
/// project-root-relative path instead of the bare basename. A failure to anchor
/// it is reported, not swallowed (PF-004) — `mds::io`, exit 2.
fn read_canonical_source(canonical: &Path, path: &Path) -> std::result::Result<String, MdsError> {
    let path_str = canonical.to_str().ok_or_else(|| MdsError::Io {
        message: format!("path is not valid UTF-8: {}", safe_path(path)),
    })?;
    let fs = NativeFs::new();
    fs.anchor_base_dir(&effective_parent(canonical).display().to_string())?;
    fs.read(path_str)
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

// ── Fix pipeline ──────────────────────────────────────────────────────────────

/// What the `--fix` pipeline made of one input's findings (#173).
///
/// `--fix` and its preview (`--fix --check` / `--fix --diff`) read the same outcome of the
/// same pipeline, so they cannot disagree about what a fix would do. `--fix` writes the
/// source of `Fixed` and `PartiallyFixed`; the preview reads either of them as "would
/// fix", shows the would-be source in `--diff`, and takes its exit code from the residual
/// ([`InputVerdict::exit_code`]). After `Rejected` and `NothingToFix`, the findings the
/// pipeline was given stand.
enum FixPipelineOutcome {
    /// Every planned edit applied.
    Fixed {
        new_source: String,
        residual: mds::LintResult,
    },
    /// Some edits applied, some individually rejected by the per-edit reverify gate.
    ///
    /// Produced when `apply_fixes_incremental` falls back to the per-edit path and not
    /// all edits pass. The `new_source` is the partially-fixed text; `residual` carries
    /// remaining diagnostics (from the last successful per-edit reverify).
    /// `applied_count` / `total_count` are used for the `--fix` summary line.
    PartiallyFixed {
        new_source: String,
        residual: mds::LintResult,
        applied_count: usize,
        total_count: usize,
    },
    /// No edit applied: the plan was refused (e.g. an overlap), or every candidate was.
    /// Carries the human-readable reason.
    Rejected { reason: String },
    /// No fixable edit, and no overlap.
    NothingToFix,
}

/// The check every candidate source must pass before the fix pipeline accepts it, in
/// `--fix` and its preview alike.
///
/// [`verify`](Self::verify) lints each candidate with `mds::lint_str_named` under the
/// name the original was linted under ([`LintSource::lint_name`]), so a partial's
/// candidate is linted as a partial and its findings compare with the original's (#309).
/// It refuses a candidate that cannot be linted — one that no longer resolves — and, when
/// every planned edit is output-neutral
/// (`mds::fix::is_output_neutral`) and the original source compiles, one whose compiled
/// output differs from the original's. Otherwise it returns the candidate's findings, and
/// `mds::fix::apply_fixes_incremental` refuses a candidate that has more findings than the
/// original for a rule the plan does not target.
///
/// When the original does not compile (e.g. it needs runtime vars) the output check is
/// skipped; the other checks still apply, and Tier B is already suggestion-only for such
/// a source (it is not standalone). `legacy-interpolation` edits change compiled output
/// on purpose — they turn plain `{x}` text into `{{x}}` interpolation — so a plan with any
/// such edit skips the output check for every edit in it.
struct ReverifyGate<'a> {
    /// The name each candidate is linted under: the original's.
    name: &'a str,
    base_dir: &'a Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    config: &'a mds::LintConfig,
    /// The original source's compiled output when the output check applies; `None` when
    /// the plan changes output on purpose or the original does not compile.
    original_output: Option<mds::CompiledOutput>,
}

impl<'a> ReverifyGate<'a> {
    /// The gate for `plan`'s candidates of `source`, which are linted under `name` and
    /// resolve against `base_dir`.
    fn new(
        plan: &mds::fix::FixPlan,
        source: &str,
        name: &'a str,
        base_dir: &'a Path,
        runtime_vars: Option<HashMap<String, mds::Value>>,
        config: &'a mds::LintConfig,
    ) -> Self {
        let all_output_neutral = plan
            .edits
            .iter()
            .all(|e| mds::fix::is_output_neutral(&e.rule));
        let original_output =
            mds::compile_str_collecting_warnings(source, Some(base_dir), runtime_vars.clone())
                .ok()
                .map(|r| r.output)
                .filter(|_| all_output_neutral);
        Self {
            name,
            base_dir,
            runtime_vars,
            config,
            original_output,
        }
    }

    /// The candidate's findings, or the reason it is refused. `apply_fixes_incremental`
    /// calls it for the whole plan and, when that candidate is refused, for each edit on
    /// its own.
    fn verify(&self, candidate: &str) -> std::result::Result<mds::LintResult, MdsError> {
        let residual = mds::lint_str_named(
            candidate,
            Some(self.base_dir),
            self.runtime_vars.clone(),
            self.config,
            self.name,
        )?;
        if let Some(original_output) = &self.original_output {
            match mds::compile_str_collecting_warnings(
                candidate,
                Some(self.base_dir),
                self.runtime_vars.clone(),
            ) {
                Ok(compiled) if compiled.output != *original_output => {
                    return Err(MdsError::Io {
                        message: "lint --fix would change compiled output; \
                                  edit reverted to preserve template semantics"
                            .to_string(),
                    });
                }
                // Same output, or the candidate does not compile (the lint above refused
                // that already).
                _ => {}
            }
        }
        Ok(residual)
    }
}

/// Run the fix pipeline over `input`'s findings: plan the edits, pass the candidates
/// through the [`ReverifyGate`], and apply the edits it accepts to `source` in memory.
/// Nothing is written here: `--fix` writes the outcome's source, and its preview shows
/// it (see [`FixPipelineOutcome`]).
///
/// `result` is `input`'s lint result, relabelled with [`LintSource::display_label`];
/// every residual diagnostic gets the same label through [`set_diag_display_path`], in
/// place of the [`LintSource::lint_name`] the gate lints a candidate under. It becomes the
/// JSON `files[].file` key once `to_canonical_json` applies
/// `sanitize_control_chars_wire`. `base_dir` is what a candidate resolves against: the
/// file's parent, or the working directory for stdin.
fn run_fix_pipeline(
    input: &LintSource<'_>,
    result: &mds::LintResult,
    source: &str,
    base_dir: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    config: &mds::LintConfig,
) -> FixPipelineOutcome {
    let plan = mds::fix::plan_fixes_with_options(result, source, result.is_standalone);

    // An overlap clears the plan's edits, yet it is not "nothing to fix":
    // `apply_fixes_incremental` reports it as a rejection.
    if plan.edits.is_empty() && !plan.overlap_rejected {
        return FixPipelineOutcome::NothingToFix;
    }

    // Counted before the plan moves into `apply_fixes_incremental`, for the
    // "{applied} of {total}" summary of a partial fix.
    let total_edits = plan.edits.len();
    let gate = ReverifyGate::new(
        &plan,
        source,
        input.lint_name(),
        base_dir,
        runtime_vars,
        config,
    );
    let outcome =
        mds::fix::apply_fixes_incremental(source, plan, result, |candidate| gate.verify(candidate));

    match outcome {
        mds::fix::FixOutcome::Fixed {
            source: new_source,
            mut residual,
        } => {
            set_diag_display_path(&mut residual, input.display_label());
            FixPipelineOutcome::Fixed {
                new_source,
                residual,
            }
        }
        mds::fix::FixOutcome::PartiallyFixed {
            source: new_source,
            mut residual,
            rejected,
        } => {
            set_diag_display_path(&mut residual, input.display_label());
            FixPipelineOutcome::PartiallyFixed {
                new_source,
                residual,
                applied_count: total_edits - rejected.len(),
                total_count: total_edits,
            }
        }
        mds::fix::FixOutcome::Rejected { reason, .. } => FixPipelineOutcome::Rejected { reason },
        mds::fix::FixOutcome::NothingToFix => FixPipelineOutcome::NothingToFix,
        // `FixOutcome` is `#[non_exhaustive]`, so a match outside mds-core needs a
        // wildcard arm. A new variant counts as nothing to fix until it is plumbed here.
        _ => FixPipelineOutcome::NothingToFix,
    }
}

// ── One input ─────────────────────────────────────────────────────────────────

/// An input lint has read and linted, for [`lint_input`].
struct Linted<'a> {
    input: LintSource<'a>,
    /// What a fixed source resolves against: the file's parent, or the working directory
    /// for stdin.
    base_dir: &'a Path,
    config: Rc<mds::LintConfig>,
    source: SourceText<'a>,
    /// The findings, still named as `mds::lint` or `mds::lint_str_with` named them.
    result: mds::LintResult,
}

/// An input's source text, or the file to read it from once it is needed.
enum SourceText<'a> {
    Read(String),
    /// An entry of a directory under `--format json`. `mds::lint` reads the file itself,
    /// so the text is read only to fix it (see [`lint_input`]).
    Unread(&'a Path),
}

impl SourceText<'_> {
    /// The text, when it has been read.
    fn into_text(self) -> Option<String> {
        match self {
            Self::Read(text) => Some(text),
            Self::Unread(_) => None,
        }
    }

    /// The text, read now when it was not read before.
    fn read(self) -> std::result::Result<String, MdsError> {
        match self {
            Self::Read(text) => Ok(text),
            Self::Unread(path) => read_source_file(path),
        }
    }
}

/// An input's exit-code category: what it leaves behind — its findings, the residual
/// `--fix` leaves or would leave, or its failure. A directory's summary counts one per
/// file, and the directory exits with the worst.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum FileTally {
    #[default]
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

/// The tally of an input refused before it had findings — its source read or `mds::lint`:
/// resource-limited for `mds::resource_limit`, "with errors" for any other refusal.
fn failure_tally(e: &MdsError) -> FileTally {
    if matches!(e, MdsError::ResourceLimit { .. }) {
        FileTally::ResourceLimit
    } else {
        FileTally::Error
    }
}

/// What one input came to — or, merged, a whole directory.
#[derive(Clone, Copy, Default)]
struct InputVerdict {
    tally: FileTally,
    /// A `--fix` preview found something to fix.
    would_fix: bool,
    /// The findings the input is left with stopped at the diagnostic cap
    /// ([`Outcome::truncated`]).
    truncated: bool,
}

impl InputVerdict {
    /// The exit code of a run over this input alone: its tally's, raised to 1 when a
    /// preview would fix something — the `--fix --check` CI contract. A residual error the
    /// fix could not remove keeps it at 2, so a preview never reports success for a file
    /// the real `--fix` would leave failing. The residual never exceeds the findings the
    /// preview shows, because the fix pipeline refuses an edit that adds findings.
    fn exit_code(self) -> i32 {
        let code = self.tally.exit_code();
        if self.would_fix {
            code.max(1)
        } else {
            code
        }
    }

    /// Both verdicts as one: the worse tally, and a pending fix or a cap in either.
    fn merge(self, other: Self) -> Self {
        Self {
            tally: self.tally.max(other.tally),
            would_fix: self.would_fix || other.would_fix,
            truncated: self.truncated || other.truncated,
        }
    }
}

/// A directory run's summary, folded from its files' verdicts (#309): how many files came
/// to each tally — the counts its summary line shows — and the verdict of the whole run.
/// Every file adds exactly one verdict, so the counts add up to the files linted.
#[derive(Clone, Copy, Default)]
pub(crate) struct DirSummary {
    pub(crate) clean_count: usize,
    pub(crate) warn_file_count: usize,
    pub(crate) error_file_count: usize,
    pub(crate) limit_file_count: usize,
    verdict: InputVerdict,
}

impl DirSummary {
    /// The summary with one more file's verdict counted.
    fn count(mut self, verdict: InputVerdict) -> Self {
        // Exhaustive: a new tally is a compile error here, never a file left uncounted.
        match verdict.tally {
            FileTally::Clean => self.clean_count += 1,
            FileTally::WarnOnly => self.warn_file_count += 1,
            FileTally::Error => self.error_file_count += 1,
            FileTally::ResourceLimit => self.limit_file_count += 1,
        }
        self.verdict = self.verdict.merge(verdict);
        self
    }

    /// Whether the findings any file is left with stopped at the diagnostic cap: the
    /// directory document's `truncated`.
    fn truncated(&self) -> bool {
        self.verdict.truncated
    }

    /// The directory's exit code: the worst file's tally, raised to 1 when a preview would
    /// fix any file ([`InputVerdict::exit_code`]).
    fn exit_code(&self) -> i32 {
        self.verdict.exit_code()
    }
}

/// Show `error`, which stops a run over stdin or a file argument before its input is
/// linted, and return the run's exit code. `stdin_source` relabels a stdin source, as
/// [`ResultSink::analysis_failure`] describes.
fn analysis_failed(
    sink: &mut impl ResultSink,
    error: &MdsError,
    stdin_source: Option<&str>,
) -> i32 {
    sink.analysis_failure(error, stdin_source);
    mds_error_exit_code(error)
}

/// What linting one input came to, as plain data (#309): worked out without printing
/// anything by [`lint_input`] — or, for an entry of a directory that could not be linted,
/// by [`lint_dir_entry`] — and shown by [`render`].
struct FileReport<'a> {
    input: LintSource<'a>,
    /// When the input's own findings stopped at the diagnostic cap, the notice [`render`]
    /// announces it with — whatever a fix leaves; `None` under the cap.
    capped: Option<CapNotice>,
    outcome: Outcome,
}

/// The diagnostic-cap notice an input whose own findings stopped at the cap gets: whether
/// it advises re-running `--fix` (#309).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapNotice {
    /// A report or a `--fix --check` / `--fix --diff` preview, which writes nothing: the
    /// notice says the findings were suppressed, and no more.
    Plain,
    /// `--fix`, which writes its fix: a re-run lints the fixed source, past the findings
    /// this run stopped at, so the notice advises it.
    RerunFix,
}

impl CapNotice {
    /// The notice for a run with `flags`: `--fix` without `--check` or `--diff` advises
    /// re-running it.
    fn for_mode(flags: LintFlags) -> Self {
        if flags.fix && !flags.check && !flags.diff {
            Self::RerunFix
        } else {
            Self::Plain
        }
    }
}

/// The findings an input has to show, and what `--fix` made of them.
enum Outcome {
    /// Without `--fix`: the input's findings, and the text they index when it was read — an
    /// entry of a directory under `--format json` is not read to report its findings.
    Reported {
        findings: mds::LintResult,
        text: Option<String>,
    },
    /// An entry of a directory that could not be linted — or, under `--format json`, read to
    /// fix it — and the tally it counts under.
    Failed { error: MdsError, tally: FileTally },
    /// The input's analysis panicked: `mds::lint` on an entry of a directory, or the fix
    /// pipeline on any input. The panic hook reported it, and the run will exit 101
    /// (#389); the input counts under "with errors".
    Panicked,
    /// `--fix --check` / `--fix --diff`: the input's findings, the text they index, and what
    /// `--fix` would do. The findings shown stay the input's own: what is wrong now.
    Previewed {
        findings: mds::LintResult,
        text: String,
        fix: PreviewFix,
    },
    /// `--fix` on a file argument or an entry of a directory: the input's findings, the text
    /// they index — the text as it was read — and what became of its file, which carries the
    /// findings a fix leaves together with the fixed text they index.
    Rewritten {
        findings: mds::LintResult,
        text: String,
        fix: Rewrite,
    },
    /// `mds lint --fix -`, a filter: the source it emits — fixed, or as given — the findings
    /// that source is left with, and what the fix did.
    Filtered {
        findings: mds::LintResult,
        output: String,
        fix: FilterFix,
    },
}

impl Outcome {
    /// Whether the findings the input is left with stopped at the diagnostic cap — its
    /// JSON document's `truncated`, in every mode (#309): its own findings when nothing is
    /// fixed, the findings a fix leaves, or — in a preview, which shows the input's own
    /// findings — those its fix would leave. An input the output records as a failure is
    /// left with no findings there, so it is never truncated.
    fn truncated(&self) -> bool {
        match self {
            Outcome::Reported { findings, .. } | Outcome::Filtered { findings, .. } => {
                findings.truncated
            }
            Outcome::Failed { .. } | Outcome::Panicked => false,
            Outcome::Previewed { findings, fix, .. } => match fix {
                PreviewFix::Pending { residual, .. } => residual.truncated,
                PreviewFix::Refused { .. } | PreviewFix::Nothing => findings.truncated,
            },
            Outcome::Rewritten { findings, fix, .. } => match fix {
                Rewrite::Refused { .. } | Rewrite::Unchanged => findings.truncated,
                Rewrite::Written { residual, .. } => residual.findings.truncated,
                Rewrite::WriteFailed { residual, .. } => residual
                    .as_ref()
                    .is_some_and(|residual| residual.findings.truncated),
            },
        }
    }
}

/// What `--fix` would do, in a preview.
enum PreviewFix {
    /// A fix is pending: the residual it would leave, its diff under `--diff`, and whether
    /// `--check` announces it.
    Pending {
        residual: mds::LintResult,
        diff: Option<String>,
        check: bool,
    },
    /// The reverify gate refused the fix.
    Refused { reason: String },
    /// Nothing to fix.
    Nothing,
}

/// What `--fix` did to an input's file.
enum Rewrite {
    /// The reverify gate refused the fix; the findings stand.
    Refused { reason: String },
    /// Nothing to fix; the findings stand.
    Unchanged,
    /// The fixed source was written. `residual` is what it is left with; `partial` holds the
    /// applied and planned edit counts when not every edit applied.
    Written {
        residual: Residual,
        partial: Option<(usize, usize)>,
    },
    /// The fixed source could not be written. `residual` is what it would have been left
    /// with, shown before the failure in a human report. A JSON output records each input
    /// once, its findings or its failure — a directory's entry, a file argument's document
    /// — so there the failure is the record, and `residual` is `None` (#309).
    WriteFailed {
        error: MdsError,
        residual: Option<Residual>,
    },
}

/// The findings a fixed source is left with, and that source: the text their spans index
/// (#309). They are shown over it, never over the source as it was read, where a removed
/// block would move every finding below it.
struct Residual {
    findings: mds::LintResult,
    fixed: String,
}

/// What `mds lint --fix -` did to the source.
enum FilterFix {
    /// Fixed; `partial` holds the applied and planned edit counts when not every edit
    /// applied.
    Fixed { partial: Option<(usize, usize)> },
    /// The reverify gate refused the fix; the source goes out as given.
    Refused { reason: String },
    /// Nothing to fix; the source goes out as given.
    Unchanged,
}

/// Lint one input — stdin, a file argument or an entry of a directory — the one way every
/// mode shares (#173), and work out what it came to without printing anything (#309):
/// name its findings for the input, then report them, or run them through
/// [`run_fix_pipeline`] and preview `--fix` ([`preview_fix`]), apply it ([`apply_fix`]) or
/// filter stdin through it ([`fix_stdin`]). [`render`] shows the report.
///
/// Each mode loads its input in its own order before this, and acts on the verdict
/// [`render`] returns: stdin and a file argument exit with [`InputVerdict::exit_code`], a
/// directory counts it in its summary.
fn lint_input<'a>(
    linted: Linted<'a>,
    flags: LintFlags,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
) -> FileReport<'a> {
    let Linted {
        input,
        base_dir,
        config,
        source,
        mut result,
    } = linted;

    // Every finding carries the input's name — each `diag.file`, hence the JSON
    // `files[].file` key — rather than the name `mds::lint` gave it.
    set_diag_display_path(&mut result, input.display_label());
    let capped = result.truncated.then_some(CapNotice::for_mode(flags));

    if !flags.fix {
        let outcome = Outcome::Reported {
            findings: result,
            text: source.into_text(),
        };
        return FileReport {
            input,
            capped,
            outcome,
        };
    }

    // An entry of a directory under `--format json` is read only now, to fix it.
    let text = match source.read() {
        Ok(text) => text,
        Err(error) => {
            let tally = failure_tally(&error);
            let outcome = Outcome::Failed { error, tally };
            return FileReport {
                input,
                capped,
                outcome,
            };
        }
    };
    // The fix pipeline lints its candidate again. A panic in it fails this input alone:
    // a directory's other entries go on (#389).
    let fix = catch_compile(
        input.path(),
        AssertUnwindSafe(|| {
            run_fix_pipeline(
                &input,
                &result,
                &text,
                base_dir,
                runtime_vars.clone(),
                &config,
            )
        }),
    );
    let fix = match fix {
        Ok(fix) => fix,
        Err(Panicked) => {
            return FileReport {
                input,
                capped,
                outcome: Outcome::Panicked,
            }
        }
    };
    let outcome = if flags.check || flags.diff {
        preview_fix(&input, result, text, fix, flags)
    } else {
        // A file argument's rewrite is anchored at its typed parent, a directory entry's at
        // the directory argument (#160).
        match input {
            LintSource::Stdin => fix_stdin(result, text, fix),
            LintSource::File { typed, .. } => apply_fix(
                &WriteTarget::as_typed(typed.to_path_buf()),
                result,
                text,
                fix,
                flags.format,
            ),
            LintSource::DirEntry { root, path, .. } => apply_fix(
                &WriteTarget::walked_below(RootPaths::as_typed(root), path),
                result,
                text,
                fix,
                flags.format,
            ),
        }
    };
    FileReport {
        input,
        capped,
        outcome,
    }
}

/// `--fix --check` / `--fix --diff`: what `--fix` would do, with nothing written. A pending
/// fix carries its diff when `--diff` asked for one.
fn preview_fix(
    input: &LintSource<'_>,
    findings: mds::LintResult,
    text: String,
    fix: FixPipelineOutcome,
    flags: LintFlags,
) -> Outcome {
    let fix = match fix {
        FixPipelineOutcome::Fixed {
            new_source,
            residual,
        }
        | FixPipelineOutcome::PartiallyFixed {
            new_source,
            residual,
            ..
        } => PreviewFix::Pending {
            diff: flags
                .diff
                .then(|| render_unified_diff(&text, &new_source, &input.diff_label())),
            residual,
            check: flags.check,
        },
        FixPipelineOutcome::Rejected { reason } => PreviewFix::Refused { reason },
        FixPipelineOutcome::NothingToFix => PreviewFix::Nothing,
    };
    Outcome::Previewed {
        findings,
        text,
        fix,
    }
}

/// `--fix`: rewrite the input's file, `target`, with the fixed source. Stdin is a filter
/// instead ([`fix_stdin`]).
fn apply_fix(
    target: &WriteTarget,
    findings: mds::LintResult,
    text: String,
    fix: FixPipelineOutcome,
    format: LintFormat,
) -> Outcome {
    let (new_source, residual, partial) = match fix {
        FixPipelineOutcome::Fixed {
            new_source,
            residual,
        } => (new_source, residual, None),
        FixPipelineOutcome::PartiallyFixed {
            new_source,
            residual,
            applied_count,
            total_count,
        } => (new_source, residual, Some((applied_count, total_count))),
        FixPipelineOutcome::Rejected { reason } => {
            let fix = Rewrite::Refused { reason };
            return Outcome::Rewritten {
                findings,
                text,
                fix,
            };
        }
        FixPipelineOutcome::NothingToFix => {
            let fix = Rewrite::Unchanged;
            return Outcome::Rewritten {
                findings,
                text,
                fix,
            };
        }
    };
    let residual = Residual {
        findings: residual,
        fixed: new_source,
    };
    let fix = match atomic_write_file(
        target,
        &residual.fixed,
        Durability::Fsync,
        Parents::Existing,
    ) {
        Ok(()) => Rewrite::Written { residual, partial },
        Err(error) => {
            // A JSON output records each input once — a directory's entry, a file
            // argument's document — so a rewrite that failed is recorded as the failure
            // alone (#309); a human report shows the findings the fix would have left,
            // then the failure.
            let failure_alone = format == LintFormat::Json;
            Rewrite::WriteFailed {
                error,
                residual: (!failure_alone).then_some(residual),
            }
        }
    };
    Outcome::Rewritten {
        findings,
        text,
        fix,
    }
}

/// `mds lint --fix -`, a filter: the fixed source goes to stdout — the source as given
/// when nothing was fixed — and the findings it is left with to stderr, rendered against
/// it. Always human, as `--fix --format json` refuses stdin.
fn fix_stdin(findings: mds::LintResult, text: String, fix: FixPipelineOutcome) -> Outcome {
    let (output, findings, fix) = match fix {
        FixPipelineOutcome::Fixed {
            new_source,
            residual,
        } => (new_source, residual, FilterFix::Fixed { partial: None }),
        FixPipelineOutcome::PartiallyFixed {
            new_source,
            residual,
            applied_count,
            total_count,
        } => (
            new_source,
            residual,
            FilterFix::Fixed {
                partial: Some((applied_count, total_count)),
            },
        ),
        FixPipelineOutcome::Rejected { reason } => (text, findings, FilterFix::Refused { reason }),
        FixPipelineOutcome::NothingToFix => (text, findings, FilterFix::Unchanged),
    };
    Outcome::Filtered {
        findings,
        output,
        fix,
    }
}

/// Show `report` through `sink`, and return what the input came to (#309). The sink shows
/// each part in its format; the order is decided here, per mode:
///
/// - An input whose own findings stopped at the diagnostic cap announces it first, in every
///   mode — a report, a preview, a fix, a failure (#309) — in the words its
///   [`CapNotice`] picks.
/// - A preview shows its diff, `Would fix:` or `fix rejected:`, then the input's own
///   findings.
/// - A rewrite that landed shows the findings the file is left with, rendered against the
///   fixed source, then `Fixed:` — never before, so no line claims a fix the file did not
///   get. One that failed shows those findings, where the output keeps them, rendered
///   against the source it failed to write, then the failure.
/// - The stdin filter shows its status line, then its findings, rendered against the
///   source it emits, then that source.
fn render(report: FileReport<'_>, sink: &mut impl ResultSink) -> InputVerdict {
    let FileReport {
        input,
        capped,
        outcome,
    } = report;
    if let Some(notice) = capped {
        sink.cap_reached(&input, notice);
    }
    let truncated = outcome.truncated();
    let (tally, would_fix) = match outcome {
        Outcome::Reported { findings, text } => {
            sink.findings(&input, &findings, text.as_deref(), truncated);
            sink.clean(&input, &findings);
            (tally_from_result(&findings), false)
        }
        Outcome::Failed { error, tally } => {
            sink.failed(&input, error);
            (tally, false)
        }
        Outcome::Panicked => {
            sink.panicked(&input);
            (FileTally::Error, false)
        }
        Outcome::Previewed {
            findings,
            text,
            fix,
        } => {
            let verdict = match fix {
                PreviewFix::Pending {
                    residual,
                    diff,
                    check,
                } => {
                    let diff_lost = diff.is_some_and(|diff| sink.diff(&diff));
                    if check {
                        sink.would_fix(&input);
                    }
                    // A diff a failing stdout lost counts the input under "with errors", as
                    // a rewrite that fails does (#157). A run over stdin or a file exits 2
                    // either way: the failure is recorded, and the exit funnel lifts the
                    // code to 2.
                    let tally = if diff_lost {
                        FileTally::Error
                    } else {
                        tally_from_result(&residual)
                    };
                    (tally, true)
                }
                PreviewFix::Refused { reason } => {
                    sink.fix_rejected(&input, &reason);
                    (tally_from_result(&findings), false)
                }
                PreviewFix::Nothing => (tally_from_result(&findings), false),
            };
            sink.findings(&input, &findings, Some(&text), truncated);
            verdict
        }
        Outcome::Rewritten {
            findings,
            text,
            fix,
        } => match fix {
            Rewrite::Refused { reason } => {
                sink.fix_rejected(&input, &reason);
                sink.findings(&input, &findings, Some(&text), truncated);
                (tally_from_result(&findings), false)
            }
            Rewrite::Unchanged => {
                sink.findings(&input, &findings, Some(&text), truncated);
                sink.clean(&input, &findings);
                (tally_from_result(&findings), false)
            }
            Rewrite::Written { residual, partial } => {
                sink.findings(&input, &residual.findings, Some(&residual.fixed), truncated);
                sink.fixed(&input, partial);
                (tally_from_result(&residual.findings), false)
            }
            Rewrite::WriteFailed { error, residual } => {
                if let Some(residual) = &residual {
                    sink.findings(&input, &residual.findings, Some(&residual.fixed), truncated);
                }
                sink.write_failed(&input, error);
                (FileTally::Error, false)
            }
        },
        Outcome::Filtered {
            findings,
            output,
            fix,
        } => {
            match fix {
                FilterFix::Fixed { partial } => sink.fixed(&input, partial),
                FilterFix::Refused { reason } => sink.fix_rejected(&input, &reason),
                FilterFix::Unchanged => {}
            }
            sink.findings(&input, &findings, Some(&output), truncated);
            sink.fixed_source(&output);
            (tally_from_result(&findings), false)
        }
    };
    InputVerdict {
        tally,
        would_fix,
        truncated,
    }
}

// ── Stdin mode ────────────────────────────────────────────────────────────────

fn run_lint_stdin(
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    sink: &mut impl ResultSink,
) -> i32 {
    let source = match read_stdin() {
        Ok(source) => source,
        // #157: stdin over the cap is `mds::resource_limit` (exit 3), one that cannot be
        // read or is not UTF-8 `mds::io` (exit 2) — as for every other command.
        Err(e) => return analysis_failed(sink, &e, None),
    };
    // A working directory that cannot be determined — deleted while the process is in it
    // — fails closed in the words a stdin compile meets it in (#390), before `"."` names it.
    if let Err(e) = crate::output::current_dir() {
        return analysis_failed(sink, &e, Some(&source));
    }
    // The working directory, as the caller did not type it: `"."` anchors at it and is
    // what a refusal of it shows (see `read_stdin`).
    let cwd = Path::new(".");
    // mds.json load/parse failure → the JSON envelope in --format json mode.
    let config = match load_lint_config(cwd, flags.quiet) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            // Config errors (MdsError::Io) carry no embedded NamedSource, so the relabel
            // is a no-op here. Passed anyway so the envelope rule holds for EVERY stdin
            // failure path — a future error variant routed here that does carry a source
            // inherits the sentinel instead of needing a new call.
            return analysis_failed(sink, &mds_err, Some(&source));
        }
    };

    let result = match mds::lint_str_with(&source, Some(cwd), runtime_vars.clone(), &config) {
        Ok(r) => r,
        // Relabel <source> → <stdin> in the rendered failure.
        Err(e) => return analysis_failed(sink, &e, Some(&source)),
    };
    let linted = Linted {
        input: LintSource::Stdin,
        base_dir: cwd,
        config: Rc::new(config),
        source: SourceText::Read(source),
        result,
    };
    render(lint_input(linted, flags, &runtime_vars), sink).exit_code()
}

// ── Single-file mode ──────────────────────────────────────────────────────────

fn run_lint_file(
    path: &Path,
    flags: LintFlags,
    runtime_vars: Option<std::collections::HashMap<String, mds::Value>>,
    sink: &mut impl ResultSink,
) -> i32 {
    // effective_parent maps "" (bare filename) to "." — avoids PF-006.
    let base_dir = effective_parent(path);
    // mds.json load/parse failure → the JSON envelope in --format json mode.
    let config = match load_lint_config(base_dir, flags.quiet) {
        Ok(c) => c,
        Err(e) => {
            let mds_err = MdsError::Io {
                message: format!("{e}"),
            };
            return analysis_failed(sink, &mds_err, None);
        }
    };
    // File read failure (not found, symlink, I/O) → the JSON envelope in --format json mode.
    let source = match read_source_file(path) {
        Ok(s) => s,
        Err(e) => return analysis_failed(sink, &e, None),
    };

    let result = match mds::lint(path, runtime_vars.clone(), &config) {
        Ok(r) => r,
        Err(e) => return analysis_failed(sink, &e, None),
    };
    // Named only after `mds::lint` accepted the path, which refuses a path that is not
    // UTF-8 first: the name is UTF-8 here, and every refusal is `mds::lint`'s own (see
    // `LintSource::file`).
    let input = match LintSource::file(path) {
        Ok(input) => input,
        Err(e) => return analysis_failed(sink, &e, None),
    };
    let linted = Linted {
        input,
        base_dir,
        config: Rc::new(config),
        source: SourceText::Read(source),
        result,
    };
    render(lint_input(linted, flags, &runtime_vars), sink).exit_code()
}

// ── Directory mode ────────────────────────────────────────────────────────────

/// Compile-time context for directory-mode lint.
///
/// Groups the parameters resolved once at the start of a directory lint run and
/// threaded into every entry's load — keeps the per-entry functions within clippy's
/// argument limit without an `#[allow(clippy::too_many_arguments)]`
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
            Some(ProjectConfig {
                config: mds_config,
                dir: config_dir,
                ..
            }) => {
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
/// returned `MdsError::ResourceLimit`, as the read of a source over the size cap does
/// (#309) — distinct from lint findings.  A file whose own
/// output failed also counts "with errors": a `--fix` rewrite that fails, or a diff a
/// failing stdout lost (#157) — as `mds fmt <dir>` counts either one failed.
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
    sink: &mut impl ResultSink,
) -> i32 {
    const MAX_DEPTH: usize = 64;

    // Config is discovered per file (each file walks up to its nearest mds.json):
    // base_dir_cache and config_dir_cache in LintDirCtx amortise repeated loads for files
    // in the same directory and across subdirectories sharing one root mds.json.

    let walk = collect_mds_files_detailed(dir, MAX_DEPTH, None);

    // Nothing to lint emits no summary — no file was linted, so there is nothing
    // meaningful to count. Either diagnostic — under --format json the error document —
    // bypasses --quiet and exits 2 (lint's usage-error code; build/check/fmt use 1): every
    // file under default-excluded directories, and since #204 an empty tree — a silent
    // ZERO exit on an empty tree was the CI green-pass hole #204 closes.
    if walk.files.is_empty() {
        sink.nothing_to_lint(dir, &walk);
        return 2;
    }
    let files = walk.files;

    // #217: compute every display key BEFORE the sort, so a path that cannot be
    // named relative to `dir` fails the whole run instead of contributing a lossy
    // or absolute key.  Ordering matters twice over:
    //   * before the sort — the sort key IS the display key, so a lossy key that
    //     only fails later would already have been built and compared here;
    //   * before the per-file loop — nothing is linted, so no `files[]` entry can
    //     carry a key that names no file under `dir`.
    // The failure is shown as an analysis failure, so `--format json` consumers get the
    // JSON envelope they parse.  Same shape as the directory-argument refusal in `do_lint`.
    let mut keyed: Vec<(PathBuf, String)> = Vec::with_capacity(files.len());
    for p in files {
        match relative_display(&p, dir) {
            Ok(display) => keyed.push((p, display)),
            Err(e) => return analysis_failed(sink, &e, None),
        }
    }

    // F1: sort by (sanitized_display_key, raw_os_path) so that:
    // 1. Array position is consistent with the sanitized `files[].file` key
    //    emitted by `to_canonical_json` for diagnostic entries (AC-P1-10).
    // 2. An OsString secondary key breaks any ties when two distinct paths produce
    //    the same sanitized display key — rare in practice, but ensures
    //    deterministic order regardless of readdir enumeration order.
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
    keyed.sort_by_cached_key(|(p, display)| {
        (
            mds::sanitize_control_chars_wire(display).into_owned(),
            p.as_os_str().to_os_string(),
        )
    });

    let ctx = LintDirCtx {
        flags,
        runtime_vars: &runtime_vars,
        base_dir_cache: RefCell::new(HashMap::new()),
        config_dir_cache: RefCell::new(HashMap::new()),
    };

    // Each entry comes to exactly one report and one verdict, and the summary is their
    // fold: its counts add up to the entries by construction.
    sink.start_document();
    let summary = keyed
        .iter()
        .map(|(path, key)| render(lint_dir_entry(dir, path, key, &ctx), sink))
        .fold(DirSummary::default(), DirSummary::count);

    // The JSON document first, so consumers always receive it on stdout whatever the
    // exit code (issue #36); then the summary, on stderr, in both formats.
    sink.end_document(summary.truncated());
    sink.summary(&summary);

    // Exit = the worst file's tally, raised to 1 when a preview would fix any file. In a
    // preview the tallies are the residuals', so a tree whose fixes would leave
    // error-severity findings behind exits 2 even though every file "would fix".
    summary.exit_code()
}

/// Read and lint one entry of a directory, and report what it came to: [`lint_input`]'s
/// report, or the entry's failure, which the rest of the tree does not stop for.
///
/// `root` is the directory argument, `path` the file the walk found below it, `key` its
/// display key, computed once by [`run_lint_directory`] before the sort (#217).
///
/// The entry's `mds.json` loads before the entry is read, as a file argument's does, so a
/// configuration that cannot load is the entry's failure even when the file cannot be
/// read either. Human output renders every finding in its source, so it reads the source
/// next; JSON output reads it only to fix it ([`SourceText::Unread`]). Either way a read
/// refused for size counts under "resource-limited", as `mds::lint`'s own refusal does
/// ([`failure_tally`]).
fn lint_dir_entry<'a>(
    root: &'a Path,
    path: &'a Path,
    key: &'a str,
    ctx: &LintDirCtx<'_>,
) -> FileReport<'a> {
    let entry = move || LintSource::DirEntry { root, path, key };
    let failed = move |error: MdsError, tally: FileTally| FileReport {
        input: entry(),
        capped: None,
        outcome: Outcome::Failed { error, tally },
    };

    // A bare file name has the parent "", which `effective_parent` maps to ".".
    let base_dir = effective_parent(path);
    // Each file's nearest mds.json, cached per directory; one that fails to load fails
    // this entry only.
    let config = match ctx.config_for(base_dir) {
        Ok(config) => config,
        Err(e) => return failed(e, FileTally::Error),
    };

    let source = if ctx.flags.format == LintFormat::Json {
        SourceText::Unread(path)
    } else {
        match read_source_file(path) {
            Ok(text) => SourceText::Read(text),
            Err(e) => {
                let tally = failure_tally(&e);
                return failed(e, tally);
            }
        }
    };
    // A panic in the analysis fails this entry alone, and the rest of the tree goes on
    // (#389).
    let linted = catch_compile(
        path,
        AssertUnwindSafe(|| mds::lint(path, ctx.runtime_vars.clone(), &config)),
    );
    let result = match linted {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => {
            let tally = failure_tally(&e);
            return failed(e, tally);
        }
        Err(Panicked) => {
            return FileReport {
                input: entry(),
                capped: None,
                outcome: Outcome::Panicked,
            }
        }
    };
    let linted = Linted {
        input: entry(),
        base_dir,
        config,
        source,
        result,
    };
    lint_input(linted, ctx.flags, ctx.runtime_vars)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        apply_fix, fix_stdin, lint_input, render, run_fix_pipeline, set_diag_display_path,
        CapNotice, DirSummary, FileReport, FileTally, FilterFix, FixPipelineOutcome, InputVerdict,
        LintFlags, LintFormat, LintSource, Linted, Outcome, PreviewFix, Residual, ReverifyGate,
        Rewrite, SourceText,
    };
    use crate::lint_sink::{HumanSink, JsonSink, ResultSink};
    use crate::output::{safe_path, STDIN_DISPLAY_LABEL};
    use mds::{FixLineSpan, LintDiagnostic, LintResult, MdsError, SerializedSpan, Severity};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    /// ISS-13: CLI-level genuine partial overlaps are structurally impossible with
    /// the current 10 rules.  Every pair of real-rule fix spans is either disjoint
    /// (different AST blocks) or in a containment relationship (same block, different
    /// rules) — the latter is resolved by `dedup_contained_or_identical` before the
    /// overlap detector is reached.  No MDS fixture can drive `overlap_rejected=true`
    /// through the CLI binary.
    ///
    /// This unit test covers rejection surfacing at the deepest level reachable from
    /// the CLI crate: `run_fix_pipeline` with a crafted `LintResult` whose `fix_removals`
    /// produce the same genuine partial overlap used in the mds-core ISS-02 regression
    /// test (fix.rs: `a4_partial_overlap_still_rejected_after_dedup`).
    ///
    /// Math (identical to ISS-02):
    ///   source = "line0\nline1\nline2\n"
    ///   Edit A: FixLineSpan::range_inclusive(0, 6)  → ByteEdit [0,  12)
    ///   Edit B: FixLineSpan::range_inclusive(6, 12) → ByteEdit [6,  18)
    ///   A.end=12 > B.start=6, B.end=18 > A.end=12 → partial overlap → overlap_rejected.
    ///
    /// `run_fix_pipeline` must return `FixPipelineOutcome::Rejected` — the rejection must be
    /// surfaced (not silently swallowed as `NothingToFix`).  This pins PF-004: the
    /// preview path uses the same gated pipeline as the write path and is equally
    /// honest about overlap refusals.
    #[test]
    fn fix_pipeline_surfaces_rejected_on_overlap() {
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

        let input = LintSource::DirEntry {
            root: Path::new("."),
            path: Path::new("overlap.mds"),
            key: "overlap.mds",
        };
        let outcome = run_fix_pipeline(
            &input,
            &result,
            source,
            Path::new("."),
            None,
            &mds::LintConfig::default(),
        );

        assert!(
            matches!(outcome, FixPipelineOutcome::Rejected { .. }),
            "run_fix_pipeline must return Rejected for an overlap_rejected plan, not \
             NothingToFix, Fixed or PartiallyFixed (preview must be as honest as apply)"
        );
    }

    /// The residual the preview reads from `FixPipelineOutcome::Fixed` or
    /// `PartiallyFixed` must have every diagnostic's `file` field relabelled to the
    /// input's display label, as the write path's residual is.  Without the relabel the
    /// residual carries the name the reverify gate lints a candidate under: the entry's
    /// file name `custom-label.mds`, not its key `sub/custom-label.mds`.
    ///
    /// The source carries one FIXABLE finding (empty-block on the bare `@if`) and
    /// one NON-fixable finding that survives the fix (unused-variable on the
    /// unreferenced frontmatter key), so the residual is non-empty — the label
    /// assertion cannot pass vacuously (PF-013), and a non-vacuity guard asserts
    /// the expected rule survived.
    #[test]
    fn fix_pipeline_would_fix_relabels_residual_display_path() {
        let source = "---\nunused_key: value\n---\n\n@if \"a\" == \"a\":\n@end\n\nHello\n";
        let config = mds::LintConfig::default();
        let result = mds::lint_str_with(source, Some(Path::new(".")), None, &config)
            .expect("fixture source must lint");

        let input = LintSource::DirEntry {
            root: Path::new("."),
            path: Path::new("sub/custom-label.mds"),
            key: "sub/custom-label.mds",
        };
        let outcome = run_fix_pipeline(&input, &result, source, Path::new("."), None, &config);

        match outcome {
            FixPipelineOutcome::Fixed { ref residual, .. }
            | FixPipelineOutcome::PartiallyFixed { ref residual, .. } => {
                assert!(
                    !residual.diagnostics.is_empty(),
                    "non-vacuity (PF-013): the unused-variable finding must survive the fix"
                );
                assert!(
                    residual
                        .diagnostics
                        .iter()
                        .any(|d| d.rule == "unused-variable"),
                    "non-vacuity (PF-013): expected the surviving rule to be unused-variable; \
                     got: {:?}",
                    residual
                        .diagnostics
                        .iter()
                        .map(|d| d.rule.as_str())
                        .collect::<Vec<_>>()
                );
                for diag in &residual.diagnostics {
                    assert_eq!(
                        diag.file.as_deref(),
                        Some("sub/custom-label.mds"),
                        "R1: every residual diagnostic must carry the caller-supplied \
                         display label"
                    );
                }
            }
            _ => panic!(
                "run_fix_pipeline must return Fixed or PartiallyFixed for a source with a \
                 fixable empty-block finding"
            ),
        }
    }

    /// Each input's lint name is the name `mds::lint` or `mds::lint_str_with` linted it
    /// under: the string label for stdin, and the file name — the path's last component,
    /// whatever directories lead to it — for a file argument or a directory's entry.
    #[test]
    fn an_input_is_linted_under_its_file_name_and_stdin_under_the_string_label() {
        assert_eq!(LintSource::Stdin.lint_name(), mds::STRING_SOURCE_MAP_LABEL);
        let typed = Path::new("templates/_p.mds");
        let file = LintSource::File {
            typed,
            name: "_p.mds",
        };
        assert_eq!(file.lint_name(), "_p.mds");
        for key in ["_p.mds", "sub/_p.mds", "sub/deeper/_p.mds"] {
            let path = Path::new(key);
            let entry = LintSource::DirEntry {
                root: Path::new("."),
                path,
                key,
            };
            assert_eq!(entry.lint_name(), "_p.mds", "the entry keyed {key:?}");
            assert_eq!(
                path.file_name().and_then(OsStr::to_str),
                Some(entry.lint_name()),
                "the name `mds::lint` takes from the path, for the entry keyed {key:?}"
            );
        }
    }

    /// The source the reverify-gate tests share: an empty `@if` block (a fixable,
    /// output-neutral `empty-block` finding) above a line of text.
    const EMPTY_BLOCK_SOURCE: &str = "@if \"a\" == \"a\":\n@end\n\nHello\n";

    /// The gate's refusal of a candidate that would change the compiled output.
    const OUTPUT_CHANGED: &str =
        "lint --fix would change compiled output; edit reverted to preserve template semantics";

    /// `source`'s compiled output, resolved against the working directory like the
    /// tests' gates.
    fn compiled(source: &str) -> mds::CompiledOutput {
        mds::compile_str_collecting_warnings(source, Some(Path::new(".")), None)
            .expect("fixture source must compile")
            .output
    }

    /// `source`'s findings and the fix plan `run_fix_pipeline` builds from them. The
    /// source is linted as stdin is, under `mds::STRING_SOURCE_MAP_LABEL`, the name the
    /// tests' gates lint its candidates under.
    fn lint_and_plan(source: &str, config: &mds::LintConfig) -> (LintResult, mds::fix::FixPlan) {
        let result = mds::lint_str_with(source, Some(Path::new(".")), None, config)
            .expect("fixture source must lint");
        let plan = mds::fix::plan_fixes_with_options(&result, source, result.is_standalone);
        (result, plan)
    }

    /// When every planned edit is output-neutral, the gate refuses a candidate that
    /// compiles to different output, with the `mds::io` message a `fix rejected:` line
    /// quotes.
    ///
    /// No known `mds lint --fix` input reaches this refusal end to end, so the candidate
    /// is crafted. It still lints, which leaves the output check as the only check that
    /// can refuse it.
    ///
    /// Control: the plan's own fix compiles to the same output and is accepted, so the
    /// gate does not refuse every candidate.
    #[test]
    fn reverify_gate_refuses_a_candidate_that_changes_compiled_output() {
        let config = mds::LintConfig::default();
        let (_, plan) = lint_and_plan(EMPTY_BLOCK_SOURCE, &config);
        assert!(
            !plan.edits.is_empty()
                && plan
                    .edits
                    .iter()
                    .all(|e| mds::fix::is_output_neutral(&e.rule)),
            "precondition: the plan must have edits, all output-neutral; got {:?}",
            plan.edits
        );
        let gate = ReverifyGate::new(
            &plan,
            EMPTY_BLOCK_SOURCE,
            mds::STRING_SOURCE_MAP_LABEL,
            Path::new("."),
            None,
            &config,
        );

        let fixed = mds::fix::apply_plan_unchecked(EMPTY_BLOCK_SOURCE, &plan);
        assert_eq!(
            compiled(&fixed),
            compiled(EMPTY_BLOCK_SOURCE),
            "precondition: the plan's own fix must keep the compiled output"
        );
        if let Err(err) = gate.verify(&fixed) {
            panic!("control: the gate must accept an output-identical candidate; got: {err}");
        }

        let changed = EMPTY_BLOCK_SOURCE.replace("Hello", "Goodbye");
        if let Err(err) = mds::lint_str_with(&changed, Some(Path::new(".")), None, &config) {
            panic!(
                "precondition: the changed candidate must lint, so that only the output \
                 check can refuse it; got: {err}"
            );
        }
        assert_ne!(
            compiled(&changed),
            compiled(EMPTY_BLOCK_SOURCE),
            "precondition: the changed candidate must compile to different output"
        );
        let err = gate
            .verify(&changed)
            .expect_err("the gate must refuse a candidate that changes the compiled output");
        assert!(
            matches!(err, mds::MdsError::Io { .. }),
            "the refusal must be an Io error; got: {err:?}"
        );
        assert_eq!(err.to_string(), OUTPUT_CHANGED);
        assert_eq!(
            miette::Diagnostic::code(&err)
                .map(|code| code.to_string())
                .as_deref(),
            Some("mds::io")
        );
    }

    /// A plan with any edit that changes output on purpose (`legacy-interpolation`)
    /// skips the output check for every candidate: one that changes the output is
    /// accepted as long as it lints.
    ///
    /// Control: the same candidate against the plan's output-neutral edits alone is
    /// refused, so the acceptance comes from the output-changing edit.
    #[test]
    fn reverify_gate_skips_the_output_check_for_a_plan_that_changes_output() {
        let source = "@if \"a\" == \"a\":\n@end\n\nHello {name}\n";
        let config = mds::LintConfig::default();
        let neutral = |e: &mds::fix::ByteEdit| mds::fix::is_output_neutral(&e.rule);
        let (_, plan) = lint_and_plan(source, &config);
        assert!(
            plan.edits.iter().any(neutral) && !plan.edits.iter().all(neutral),
            "precondition: the plan must mix output-neutral and output-changing edits; \
             got {:?}",
            plan.edits
        );
        let changed = source.replace("Hello", "Goodbye");
        assert_ne!(
            compiled(&changed),
            compiled(source),
            "precondition: the changed candidate must compile to different output"
        );

        let gate = ReverifyGate::new(
            &plan,
            source,
            mds::STRING_SOURCE_MAP_LABEL,
            Path::new("."),
            None,
            &config,
        );
        if let Err(err) = gate.verify(&changed) {
            panic!("a plan with an output-changing edit must skip the output check; got: {err}");
        }

        let (_, mut neutral_plan) = lint_and_plan(source, &config);
        neutral_plan.edits.retain(neutral);
        assert!(
            !neutral_plan.edits.is_empty(),
            "precondition: the control plan must keep the output-neutral edit"
        );
        let neutral_gate = ReverifyGate::new(
            &neutral_plan,
            source,
            mds::STRING_SOURCE_MAP_LABEL,
            Path::new("."),
            None,
            &config,
        );
        let err = neutral_gate
            .verify(&changed)
            .expect_err("control: an all-neutral plan must refuse the changed candidate");
        assert_eq!(err.to_string(), OUTPUT_CHANGED);
    }

    /// The gate's refusal reaches the pipeline's outcome as `Rejected`, and its words
    /// reach the reason `fix rejected:` prints. The `LintResult` is crafted, as in the
    /// overlap test above: an output-neutral `empty-block` removal aimed at the `Hello`
    /// line, which no real rule plans.
    ///
    /// Control: the real findings of the same source are fixed.
    #[test]
    fn fix_pipeline_rejects_a_fix_that_would_change_compiled_output() {
        let config = mds::LintConfig::default();
        let input = LintSource::DirEntry {
            root: Path::new("."),
            path: Path::new("output.mds"),
            key: "output.mds",
        };

        let (real, _) = lint_and_plan(EMPTY_BLOCK_SOURCE, &config);
        let outcome = run_fix_pipeline(
            &input,
            &real,
            EMPTY_BLOCK_SOURCE,
            Path::new("."),
            None,
            &config,
        );
        assert!(
            matches!(outcome, FixPipelineOutcome::Fixed { .. }),
            "control: the real empty-block fix must apply"
        );

        let hello = EMPTY_BLOCK_SOURCE
            .find("Hello")
            .expect("the fixture has a Hello line");
        let crafted = LintResult::new(vec![LintDiagnostic::new(
            "empty-block",
            Severity::Warn,
            "crafted",
        )
        .with_fix_removals(vec![FixLineSpan::range_inclusive(hello, hello)])]);
        match run_fix_pipeline(
            &input,
            &crafted,
            EMPTY_BLOCK_SOURCE,
            Path::new("."),
            None,
            &config,
        ) {
            FixPipelineOutcome::Rejected { reason } => assert_eq!(
                reason,
                format!(
                    "could not verify fix — the edited source did not re-parse cleanly \
                     ({OUTPUT_CHANGED}); leaving the file unchanged"
                )
            ),
            _ => panic!(
                "run_fix_pipeline must return Rejected for a fix that would change the \
                 compiled output, not Fixed, PartiallyFixed or NothingToFix"
            ),
        }
    }

    /// `mds lint --fix --format json --quiet <dir>`, for one entry of the directory:
    /// `--quiet` keeps the status lines off the test's stderr, and the tests read the
    /// directory's JSON document.
    const DIR_JSON_FIX: LintFlags = LintFlags {
        fix: true,
        check: false,
        diff: false,
        quiet: true,
        format: LintFormat::Json,
    };

    /// A source whose fix leaves a finding: the empty `@if` block is removed, and the
    /// unreferenced frontmatter key stays an `unused-variable` warning.
    const FIX_LEAVES_A_FINDING: &str =
        "---\nunused_key: value\n---\n\n@if \"a\" == \"a\":\n@end\n\nHello\n";

    /// `FIX_LEAVES_A_FINDING`'s findings, named for `input` as a directory run names them,
    /// and what the fix pipeline makes of them: a fix that leaves a finding.
    fn fix_leaving_a_finding(input: &LintSource<'_>) -> (LintResult, FixPipelineOutcome) {
        let config = mds::LintConfig::default();
        let mut result =
            mds::lint_str_with(FIX_LEAVES_A_FINDING, Some(Path::new(".")), None, &config)
                .expect("fixture source must lint");
        set_diag_display_path(&mut result, input.display_label());
        let outcome = run_fix_pipeline(
            input,
            &result,
            FIX_LEAVES_A_FINDING,
            Path::new("."),
            None,
            &config,
        );
        match &outcome {
            FixPipelineOutcome::Fixed { residual, .. }
            | FixPipelineOutcome::PartiallyFixed { residual, .. } => assert!(
                !residual.diagnostics.is_empty(),
                "precondition: the fix must leave a finding"
            ),
            _ => panic!("precondition: the empty-block finding must be fixed"),
        }
        (result, outcome)
    }

    /// `--fix` of `input`, an entry of a directory under `--format json`, whose file is at
    /// `path`: [`apply_fix`] rewrites it, and [`render`] shows the report through a JSON sink
    /// in the middle of a directory run. The entry's verdict, and the entries the
    /// directory's document then holds.
    fn fix_in_a_json_directory(
        input: LintSource<'_>,
        path: &Path,
        result: LintResult,
        outcome: FixPipelineOutcome,
    ) -> (InputVerdict, Vec<serde_json::Value>) {
        let outcome = apply_fix(
            &crate::output::WriteTarget::as_typed(path.to_path_buf()),
            result,
            FIX_LEAVES_A_FINDING.to_string(),
            outcome,
            DIR_JSON_FIX.format,
        );
        let mut sink = JsonSink::new(DIR_JSON_FIX.quiet);
        sink.start_document();
        let report = FileReport {
            input,
            capped: None,
            outcome,
        };
        let verdict = render(report, &mut sink);
        (verdict, sink.document().to_vec())
    }

    /// A directory's JSON document records a rewritten file's remaining findings only once
    /// the rewrite has landed. When the rewrite fails, the failure is the file's one entry:
    /// findings recorded before the write would sit beside it and describe a file that was
    /// never rewritten (#309).
    ///
    /// This fix leaves a warning, so a record made before the write would show.
    ///
    /// Control: with a writable target the rewrite lands and the warning is recorded.
    #[test]
    fn directory_json_fix_records_a_failed_rewrite_as_its_only_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let writable = root.join("x.mds");
        std::fs::write(&writable, FIX_LEAVES_A_FINDING).unwrap();
        let input = LintSource::DirEntry {
            root: Path::new("."),
            path: &writable,
            key: "x.mds",
        };
        let (result, outcome) = fix_leaving_a_finding(&input);
        let (verdict, document) = fix_in_a_json_directory(input, &writable, result, outcome);
        let rewritten = std::fs::read_to_string(&writable).unwrap();
        assert!(
            !rewritten.contains("@if"),
            "control: the rewrite must land; got {rewritten:?}"
        );
        assert!(
            verdict.tally == FileTally::WarnOnly,
            "control: the file counts by the warning the fix left"
        );
        assert_eq!(
            document.len(),
            1,
            "control: one entry, the remaining warning; got {document:?}"
        );
        assert_eq!(document[0]["file"], "x.mds");
        assert_eq!(
            document[0]["diagnostics"][0]["rule"], "unused-variable",
            "control: got {document:?}"
        );

        // The target's directory is gone, so the temporary file cannot be created.
        let unwritable = root.join("gone").join("x.mds");
        let input = LintSource::DirEntry {
            root: Path::new("."),
            path: &unwritable,
            key: "x.mds",
        };
        let (result, outcome) = fix_leaving_a_finding(&input);
        let (verdict, document) = fix_in_a_json_directory(input, &unwritable, result, outcome);
        assert!(!unwritable.exists(), "precondition: nothing was written");
        assert!(
            verdict.tally == FileTally::Error,
            "a failed rewrite counts under \"with errors\""
        );
        assert_eq!(
            document.len(),
            1,
            "the failure must be the file's only entry; got {document:?}"
        );
        assert_eq!(document[0]["file"], "x.mds");
        assert_eq!(document[0]["error"]["code"], "mds::io", "got {document:?}");
        assert!(
            document[0].get("diagnostics").is_none(),
            "no findings for a file that was not rewritten; got {document:?}"
        );
    }

    /// Under `--format json` a directory's entry is read only to fix it. When that read
    /// fails, the failure is recorded as the entry and counts under "with errors". The
    /// entry holds no findings, so it does not mark the document truncated, though its
    /// lint was capped (#309).
    ///
    /// The read follows a lint that read the same file, so no CLI run reaches it without a
    /// race; the capped result is crafted.
    ///
    /// Control: the same entry, readable and with nothing to fix, adds no entry, counts
    /// clean and marks the document truncated: the findings it is left with are its own.
    #[test]
    fn directory_json_fix_records_an_entry_it_cannot_read_as_a_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let config = Rc::new(mds::LintConfig::default());
        // The entry linted, reported and shown through a JSON sink in the middle of a
        // directory run: its verdict, and the entries the directory's document then holds.
        let fix = |path: &Path, key: &str| {
            let linted = Linted {
                input: LintSource::DirEntry {
                    root: Path::new("."),
                    path,
                    key,
                },
                base_dir: &root,
                config: Rc::clone(&config),
                source: SourceText::Unread(path),
                result: LintResult::new(vec![]).truncated(),
            };
            let mut sink = JsonSink::new(DIR_JSON_FIX.quiet);
            sink.start_document();
            let verdict = render(lint_input(linted, DIR_JSON_FIX, &None), &mut sink);
            (verdict, sink.document().to_vec())
        };

        let readable = root.join("x.mds");
        std::fs::write(&readable, "Hello\n").unwrap();
        let (verdict, document) = fix(&readable, "x.mds");
        assert!(
            verdict.tally == FileTally::Clean,
            "control: nothing to fix and no findings"
        );
        assert!(
            verdict.truncated,
            "control: a capped result marks the document truncated"
        );
        assert!(
            document.is_empty(),
            "control: no findings, no entry; got {document:?}"
        );

        let missing = root.join("gone.mds");
        let expected = super::read_source_file(&missing)
            .expect_err("precondition: the file must not read")
            .serialize();
        let (verdict, document) = fix(&missing, "gone.mds");
        assert!(
            verdict.tally == FileTally::Error,
            "an entry that cannot be read to fix counts under \"with errors\""
        );
        assert!(
            !verdict.truncated,
            "an entry recorded as a failure does not mark the document truncated"
        );
        assert_eq!(
            document,
            vec![serde_json::json!({ "file": "gone.mds", "error": expected })]
        );
    }

    /// Under `--format json` a directory's entry that is over the size cap when it is read
    /// to fix is recorded as that failure, `mds::resource_limit`, and counts under
    /// "resource-limited" — not "with errors" — as it does when `mds::lint` refuses it
    /// (#309).
    ///
    /// The read follows a lint that read the same file within the cap, so no CLI run
    /// reaches it without a race; the lint result is crafted.
    ///
    /// Control: the same entry at exactly the cap is read, has nothing to fix and counts
    /// clean.
    #[test]
    fn directory_json_fix_counts_an_entry_over_the_size_cap_as_resource_limited() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let config = Rc::new(mds::LintConfig::default());
        let path = root.join("x.mds");
        let fix = |len: u64| {
            let len = usize::try_from(len).unwrap();
            std::fs::write(&path, "a".repeat(len - 1) + "\n").unwrap();
            let linted = Linted {
                input: LintSource::DirEntry {
                    root: Path::new("."),
                    path: &path,
                    key: "x.mds",
                },
                base_dir: &root,
                config: Rc::clone(&config),
                source: SourceText::Unread(&path),
                result: LintResult::new(vec![]),
            };
            let mut sink = JsonSink::new(DIR_JSON_FIX.quiet);
            sink.start_document();
            let verdict = render(lint_input(linted, DIR_JSON_FIX, &None), &mut sink);
            (verdict, sink.document().to_vec())
        };

        let (verdict, document) = fix(mds::MAX_FILE_SIZE);
        assert!(
            verdict.tally == FileTally::Clean,
            "control: read within the cap, nothing to fix"
        );
        assert!(document.is_empty(), "control: no entry; got {document:?}");

        let (verdict, document) = fix(mds::MAX_FILE_SIZE + 1);
        assert!(
            verdict.tally == FileTally::ResourceLimit,
            "an entry over the size cap counts under \"resource-limited\""
        );
        assert_eq!(
            document.len(),
            1,
            "the failure is the entry; got {document:?}"
        );
        assert_eq!(document[0]["file"], "x.mds", "got {document:?}");
        assert_eq!(
            document[0]["error"]["code"], "mds::resource_limit",
            "got {document:?}"
        );
    }

    /// A directory's summary is the fold of its files' verdicts (#309): each verdict is
    /// counted exactly once, under its tally, and the run takes the worst tally, a pending
    /// fix and a cap from any file.
    ///
    /// Controls: nothing folded counts nothing and exits 0; a pending fix alone raises a
    /// clean tree to 1.
    #[test]
    fn directory_summary_counts_each_verdict_once() {
        let verdict = |tally, would_fix, truncated| InputVerdict {
            tally,
            would_fix,
            truncated,
        };
        let counts = |s: &DirSummary| {
            (
                s.clean_count,
                s.warn_file_count,
                s.error_file_count,
                s.limit_file_count,
            )
        };

        let summary = [
            verdict(FileTally::Clean, false, false),
            verdict(FileTally::WarnOnly, true, false),
            verdict(FileTally::Error, false, true),
            verdict(FileTally::Clean, false, false),
            verdict(FileTally::ResourceLimit, false, false),
        ]
        .into_iter()
        .fold(DirSummary::default(), DirSummary::count);
        assert_eq!(counts(&summary), (2, 1, 1, 1));
        assert!(
            summary.truncated(),
            "one capped file marks the run truncated"
        );
        assert_eq!(
            summary.exit_code(),
            3,
            "the worst tally is resource-limited"
        );

        let empty = DirSummary::default();
        assert_eq!(counts(&empty), (0, 0, 0, 0), "control: nothing counted");
        assert!(!empty.truncated());
        assert_eq!(empty.exit_code(), 0, "control: nothing to fail");

        let pending = [verdict(FileTally::Clean, true, false)]
            .into_iter()
            .fold(DirSummary::default(), DirSummary::count);
        assert_eq!(counts(&pending), (1, 0, 0, 0));
        assert_eq!(
            pending.exit_code(),
            1,
            "control: a preview that would fix a clean file exits 1"
        );
    }

    /// `error`'s diagnostic code, such as `mds::io`.
    fn code_of(error: &MdsError) -> String {
        miette::Diagnostic::code(error)
            .map(|code| code.to_string())
            .unwrap_or_default()
    }

    /// A result sink that shows nothing and records each call [`render`] makes of it, in
    /// order, naming the input by its display label.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
    }

    impl Recorder {
        fn record(&mut self, call: &str, input: &LintSource<'_>) {
            self.calls.push(format!("{call} {}", input.display_label()));
        }
    }

    impl ResultSink for Recorder {
        fn quiet(&self) -> bool {
            false
        }

        fn findings(
            &mut self,
            input: &LintSource<'_>,
            findings: &LintResult,
            _: Option<&str>,
            _: bool,
        ) {
            let call = format!("findings ({})", findings.diagnostics.len());
            self.record(&call, input);
        }

        fn failed(&mut self, input: &LintSource<'_>, error: MdsError) {
            let call = format!("failed ({})", code_of(&error));
            self.record(&call, input);
        }

        fn panicked(&mut self, input: &LintSource<'_>) {
            self.record("panicked", input);
        }

        fn write_failed(&mut self, input: &LintSource<'_>, _: MdsError) {
            self.record("write failed", input);
        }

        fn clean(&mut self, input: &LintSource<'_>, _: &LintResult) {
            self.record("clean", input);
        }

        fn analysis_failure(&mut self, _: &MdsError, _: Option<&str>) {
            self.calls.push("analysis failure".to_string());
        }

        fn start_document(&mut self) {
            self.calls.push("start document".to_string());
        }

        fn end_document(&mut self, _: bool) {
            self.calls.push("end document".to_string());
        }

        fn cap_reached(&mut self, input: &LintSource<'_>, notice: CapNotice) {
            let call = match notice {
                CapNotice::Plain => "cap reached",
                CapNotice::RerunFix => "cap reached, re-run --fix",
            };
            self.record(call, input);
        }

        fn fix_rejected(&mut self, input: &LintSource<'_>, _: &str) {
            self.record("fix rejected", input);
        }

        fn would_fix(&mut self, input: &LintSource<'_>) {
            self.record("would fix", input);
        }

        fn fixed(&mut self, input: &LintSource<'_>, partial: Option<(usize, usize)>) {
            let call = format!("fixed {partial:?}");
            self.record(&call, input);
        }

        fn diff(&mut self, _: &str) -> bool {
            self.calls.push("diff".to_string());
            false
        }

        fn fixed_source(&mut self, _: &str) {
            self.calls.push("fixed source".to_string());
        }
    }

    /// A capped result announces the cap before anything else its input shows (#309) — for
    /// an entry of a directory under `--format json` whose read to fix it fails, before its
    /// failure. The late-read test above reads the directory's document, which shows the
    /// failure and the `truncated` flag but not this order; the calls `render` makes do.
    ///
    /// Under `--fix` the notice advises re-running it; in a report or a preview it does not.
    ///
    /// Controls: the same capped entry, readable with nothing to fix, announces the cap
    /// before its findings, and so do its report and its previews; an uncapped entry whose
    /// read fails announces no cap.
    #[test]
    fn a_capped_entry_that_cannot_be_read_announces_the_cap_before_its_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let config = Rc::new(mds::LintConfig::default());
        // The calls `render` makes to show the entry's report, linted as a directory run
        // under `--format json` lints it with `flags`.
        let calls_with = |flags: LintFlags, path: &Path, key: &str, result: LintResult| {
            let linted = Linted {
                input: LintSource::DirEntry {
                    root: Path::new("."),
                    path,
                    key,
                },
                base_dir: &root,
                config: Rc::clone(&config),
                source: SourceText::Unread(path),
                result,
            };
            let mut sink = Recorder::default();
            render(lint_input(linted, flags, &None), &mut sink);
            sink.calls
        };
        let calls = |path: &Path, key: &str, result| calls_with(DIR_JSON_FIX, path, key, result);

        let missing = root.join("gone.mds");
        let read_failure = code_of(
            &super::read_source_file(&missing).expect_err("precondition: the file must not read"),
        );
        assert_eq!(
            calls(&missing, "gone.mds", LintResult::new(vec![]).truncated()),
            [
                "cap reached, re-run --fix gone.mds".to_string(),
                format!("failed ({read_failure}) gone.mds"),
            ],
        );

        let readable = root.join("x.mds");
        std::fs::write(&readable, "Hello\n").unwrap();
        assert_eq!(
            calls(&readable, "x.mds", LintResult::new(vec![]).truncated()),
            [
                "cap reached, re-run --fix x.mds",
                "findings (0) x.mds",
                "clean x.mds"
            ],
            "control: a capped entry with nothing to fix announces the cap before its findings"
        );
        // Only `--fix` advises re-running it: a preview writes nothing.
        for preview in [
            LintFlags {
                check: true,
                ..DIR_JSON_FIX
            },
            LintFlags {
                diff: true,
                ..DIR_JSON_FIX
            },
        ] {
            let calls = calls_with(
                preview,
                &readable,
                "x.mds",
                LintResult::new(vec![]).truncated(),
            );
            assert_eq!(
                calls.first().map(String::as_str),
                Some("cap reached x.mds"),
                "a capped preview announces the plain cap notice first; calls: {calls:?}"
            );
        }
        let report = LintFlags {
            fix: false,
            ..DIR_JSON_FIX
        };
        assert_eq!(
            calls_with(
                report,
                &readable,
                "x.mds",
                LintResult::new(vec![]).truncated()
            ),
            ["cap reached x.mds", "findings (0) x.mds", "clean x.mds"],
            "a capped report announces the cap before its findings"
        );
        assert_eq!(
            calls(&missing, "gone.mds", LintResult::new(vec![])),
            [format!("failed ({read_failure}) gone.mds")],
            "control: an uncapped result announces no cap"
        );
    }

    /// A JSON document's `truncated` is whether the findings an input is left with stopped
    /// at the diagnostic cap (#309): its own when nothing is fixed, those a fix leaves — or,
    /// in a preview, would leave — otherwise. An input recorded as a failure is left with no
    /// findings in the output, so it is never truncated. Every outcome of an input whose own
    /// findings were capped announces the cap first, whatever its `truncated`.
    ///
    /// Controls: an uncapped report, an uncapped residual and an uncapped filter are not
    /// truncated, though the input's own findings were capped.
    #[test]
    fn truncated_is_the_residual_s_and_a_cap_is_announced_first_in_every_outcome() {
        let capped = || LintResult::new(vec![]).truncated();
        let uncapped = || LintResult::new(vec![]);
        let fixed = |findings| Residual {
            findings,
            fixed: String::new(),
        };
        let unwritable = || MdsError::Io {
            message: "unwritable".to_string(),
        };
        let previewed = |fix| Outcome::Previewed {
            findings: capped(),
            text: String::new(),
            fix,
        };
        let pending = |residual| PreviewFix::Pending {
            residual,
            diff: None,
            check: false,
        };
        let rewritten = |fix| Outcome::Rewritten {
            findings: capped(),
            text: String::new(),
            fix,
        };
        let filtered = |findings, fix| Outcome::Filtered {
            findings,
            output: String::new(),
            fix,
        };
        let refused = || "refused".to_string();
        let cases = [
            (
                "report",
                Outcome::Reported {
                    findings: capped(),
                    text: None,
                },
                true,
            ),
            (
                "report, uncapped (control)",
                Outcome::Reported {
                    findings: uncapped(),
                    text: None,
                },
                false,
            ),
            (
                "failed",
                Outcome::Failed {
                    error: unwritable(),
                    tally: FileTally::Error,
                },
                false,
            ),
            ("panicked", Outcome::Panicked, false),
            ("preview, fix clears", previewed(pending(uncapped())), false),
            (
                "preview, fix leaves a cap",
                previewed(pending(capped())),
                true,
            ),
            (
                "preview, refused",
                previewed(PreviewFix::Refused { reason: refused() }),
                true,
            ),
            ("preview, nothing", previewed(PreviewFix::Nothing), true),
            (
                "rewrite, refused",
                rewritten(Rewrite::Refused { reason: refused() }),
                true,
            ),
            ("rewrite, unchanged", rewritten(Rewrite::Unchanged), true),
            (
                "rewrite, written, uncapped (control)",
                rewritten(Rewrite::Written {
                    residual: fixed(uncapped()),
                    partial: None,
                }),
                false,
            ),
            (
                "rewrite, written",
                rewritten(Rewrite::Written {
                    residual: fixed(capped()),
                    partial: Some((1, 2)),
                }),
                true,
            ),
            (
                "rewrite, write failed, the failure is the record",
                rewritten(Rewrite::WriteFailed {
                    error: unwritable(),
                    residual: None,
                }),
                false,
            ),
            (
                "rewrite, write failed, its findings shown",
                rewritten(Rewrite::WriteFailed {
                    error: unwritable(),
                    residual: Some(fixed(capped())),
                }),
                true,
            ),
            (
                "filter, fixed, uncapped (control)",
                filtered(uncapped(), FilterFix::Fixed { partial: None }),
                false,
            ),
            (
                "filter, unchanged",
                filtered(capped(), FilterFix::Unchanged),
                true,
            ),
        ];
        let typed = Path::new("x.mds");
        let mut seen = Vec::new();
        let mut expected = Vec::new();
        for (what, outcome, truncated) in cases {
            let report = FileReport {
                input: LintSource::File {
                    typed,
                    name: "x.mds",
                },
                capped: Some(CapNotice::Plain),
                outcome,
            };
            let mut sink = Recorder::default();
            let verdict = render(report, &mut sink);
            seen.push((what, verdict.truncated, sink.calls.first().cloned()));
            expected.push((what, truncated, Some("cap reached x.mds".to_string())));
        }
        assert_eq!(seen, expected);
    }

    /// The environment variable that makes a test run as the child [`in_a_child`] starts;
    /// its value names what the child shows.
    const CHILD: &str = "MDS_LINT_TEST_CHILD";

    /// What the test `name` writes to stdout and to stderr when it runs as a child with
    /// `scenario`: this test binary again, running that one test, with [`CHILD`] set to
    /// `scenario`.
    ///
    /// The sinks print through the CLI's writer to the process's own stdout and stderr,
    /// which the test harness does not capture, so a test that reads what [`render`] shows
    /// runs it in a child and reads the child's streams. The child's stdout also holds the
    /// harness's report, which must say the one test ran and passed.
    fn in_a_child(name: &str, scenario: &OsStr) -> (String, String) {
        let binary = std::env::current_exe().expect("the test binary's path");
        let run = std::process::Command::new(binary)
            .args([name, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, scenario)
            .env("NO_COLOR", "1")
            .env_remove("FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .output()
            .expect("the child runs");
        let product = String::from_utf8(run.stdout).expect("the child's stdout is UTF-8");
        let shown = String::from_utf8(run.stderr).expect("the child's stderr is UTF-8");
        assert!(
            run.status.success() && product.contains("1 passed"),
            "the child `{name}` ({scenario:?}) must run that one test and pass; status {:?}\n\
             stdout:\n{product}\nstderr:\n{shown}",
            run.status
        );
        (product, shown)
    }

    /// A human report whose finding comes without its source text shows the finding without
    /// a snippet (#309): no panic, and neither the file's name nor any source text — with no
    /// source there is no frame header to name the file. No human run lints an input it has
    /// not read, but the report's shape allows it (an entry of a directory under
    /// `--format json` is not read to report its findings).
    ///
    /// Control: the same finding with its text is framed under the file's name, over its
    /// source, so the two needles are ones the frame shows when it can.
    #[test]
    fn a_human_finding_without_its_text_shows_no_file_name_and_no_snippet() {
        const NAME: &str =
            "lint::tests::a_human_finding_without_its_text_shows_no_file_name_and_no_snippet";
        const FILE_NAME: &str = "frame-name-sentinel";
        const SOURCE_TEXT: &str = "source-text-sentinel";
        const MESSAGE: &str = "a finding shown without its text";
        if let Some(scenario) = std::env::var_os(CHILD) {
            let text = match scenario.to_str() {
                Some("with text") => Some(format!("{SOURCE_TEXT}\n")),
                Some("without text") => None,
                _ => panic!("unknown scenario {scenario:?}"),
            };
            let key = format!("{FILE_NAME}.mds");
            let path = Path::new("walked").join(&key);
            let finding = LintDiagnostic::new("empty-block", Severity::Error, MESSAGE)
                .with_span(SerializedSpan::new(0, SOURCE_TEXT.len()))
                .with_file(key.as_str());
            let report = FileReport {
                input: LintSource::DirEntry {
                    root: Path::new("."),
                    path: &path,
                    key: &key,
                },
                capped: None,
                outcome: Outcome::Reported {
                    findings: LintResult::new(vec![finding]),
                    text,
                },
            };
            let verdict = render(report, &mut HumanSink::new(false));
            assert!(
                verdict.tally == FileTally::Error,
                "the error-severity finding counts"
            );
            return;
        }

        let (_, framed) = in_a_child(NAME, OsStr::new("with text"));
        for needle in [MESSAGE, FILE_NAME, SOURCE_TEXT] {
            assert!(
                framed.contains(needle),
                "control: a frame over its text shows {needle:?}; got:\n{framed}"
            );
        }
        let (_, bare) = in_a_child(NAME, OsStr::new("without text"));
        assert!(
            bare.contains(MESSAGE),
            "the finding still shows; got:\n{bare}"
        );
        assert!(
            !bare.contains(FILE_NAME),
            "no source, so no frame header names the file; got:\n{bare}"
        );
        assert!(!bare.contains(SOURCE_TEXT), "no snippet; got:\n{bare}");
    }

    /// The source both partial-fix tests fix.
    const PARTIAL_BEFORE: &str = "before-fix-sentinel\n";
    /// What their partial fix makes of [`PARTIAL_BEFORE`].
    const PARTIAL_AFTER: &str = "after-fix-sentinel\n";
    /// The message of the finding their partial fix leaves.
    const PARTIAL_LEFT: &str = "a finding the partial fix left";
    /// The message of a finding their partial fix removes.
    const PARTIAL_REMOVED: &str = "a finding the partial fix removed";

    /// An input's findings before a partial fix, and the fix, both named `label`: one of two
    /// edits applied, the source rewritten to [`PARTIAL_AFTER`], and one warning left, at
    /// `left_at` in that source.
    ///
    /// Built here rather than reached through a source, so the tests pin how [`render`]
    /// shows a partial fix whichever edits produce one; `cli_lint.rs` reaches one end to end
    /// and checks the counts' wording.
    fn partial_fix(label: &str, left_at: SerializedSpan) -> (LintResult, FixPipelineOutcome) {
        let left = || {
            LintDiagnostic::new("empty-block", Severity::Warn, PARTIAL_LEFT)
                .with_file(label)
                .with_span(left_at.clone())
        };
        let removed =
            LintDiagnostic::new("empty-block", Severity::Warn, PARTIAL_REMOVED).with_file(label);
        let before = LintResult::new(vec![removed, left()]);
        let fix = FixPipelineOutcome::PartiallyFixed {
            new_source: PARTIAL_AFTER.to_string(),
            residual: LintResult::new(vec![left()]),
            applied_count: 1,
            total_count: 2,
        };
        (before, fix)
    }

    /// `mds lint --fix <file>` whose fix applies in part writes the partly fixed source,
    /// shows the finding it leaves — not the one it removed — framed over the source it
    /// wrote rather than the source it read, and then, as the run's last line,
    /// `Partially fixed: <path> (1 of 2 fixes applied)` (#309).
    #[test]
    fn a_partial_fix_of_a_file_is_announced_after_the_findings_it_leaves() {
        const NAME: &str =
            "lint::tests::a_partial_fix_of_a_file_is_announced_after_the_findings_it_leaves";
        const AFTER_LINE: &str = "after-fix-sentinel";
        const BEFORE_LINE: &str = "before-fix-sentinel";
        if let Some(path) = std::env::var_os(CHILD) {
            let path = PathBuf::from(path);
            let input = LintSource::File {
                typed: &path,
                name: "partial.mds",
            };
            let left_at = SerializedSpan::new(0, AFTER_LINE.len());
            let (before, fix) = partial_fix(input.display_label(), left_at);
            let text = PARTIAL_BEFORE.to_string();
            let outcome = apply_fix(
                &crate::output::WriteTarget::as_typed(path.clone()),
                before,
                text,
                fix,
                LintFormat::Human,
            );
            let report = FileReport {
                input,
                capped: None,
                outcome,
            };
            let verdict = render(report, &mut HumanSink::new(false));
            assert!(
                verdict.tally == FileTally::WarnOnly,
                "the file counts by the warning the fix left"
            );
            return;
        }

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().canonicalize().unwrap().join("partial.mds");
        std::fs::write(&path, PARTIAL_BEFORE).unwrap();
        let (_, shown) = in_a_child(NAME, path.as_os_str());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            PARTIAL_AFTER,
            "precondition: the partly fixed source was written"
        );

        let announced = format!(
            "Partially fixed: {} (1 of 2 fixes applied)",
            safe_path(&path)
        );
        assert_eq!(
            shown.lines().last(),
            Some(announced.as_str()),
            "the last line names the file and the counts; got:\n{shown}"
        );
        assert_eq!(
            shown.matches("Partially fixed:").count(),
            1,
            "announced once; got:\n{shown}"
        );
        assert!(
            shown.contains(PARTIAL_LEFT) && shown.contains(AFTER_LINE),
            "the finding the fix left shows before the announcement, framed over the \
             written source; got:\n{shown}"
        );
        assert!(
            !shown.contains(BEFORE_LINE),
            "not framed over the source as read; got:\n{shown}"
        );
        assert!(
            !shown.contains(PARTIAL_REMOVED),
            "the finding the fix removed does not show; got:\n{shown}"
        );
    }

    /// `mds lint --fix -` whose fix applies in part announces it first —
    /// `Partially fixed: <stdin> (1 of 2 fixes applied)`, the one status line stdin shows for
    /// a fix — then the finding it leaves, framed over the source it emits rather than the
    /// source it was given, and it emits that source on stdout (#309).
    #[test]
    fn a_partial_fix_of_stdin_is_announced_before_the_findings_it_leaves() {
        const NAME: &str =
            "lint::tests::a_partial_fix_of_stdin_is_announced_before_the_findings_it_leaves";
        const AFTER_LINE: &str = "after-fix-sentinel";
        const BEFORE_LINE: &str = "before-fix-sentinel";
        if std::env::var_os(CHILD).is_some() {
            let left_at = SerializedSpan::new(0, AFTER_LINE.len());
            let (before, fix) = partial_fix(STDIN_DISPLAY_LABEL, left_at);
            let outcome = fix_stdin(before, PARTIAL_BEFORE.to_string(), fix);
            let report = FileReport {
                input: LintSource::Stdin,
                capped: None,
                outcome,
            };
            let verdict = render(report, &mut HumanSink::new(false));
            assert!(
                verdict.tally == FileTally::WarnOnly,
                "stdin counts by the warning the fix left"
            );
            return;
        }

        let (product, shown) = in_a_child(NAME, OsStr::new("stdin"));
        let announced = format!("Partially fixed: {STDIN_DISPLAY_LABEL} (1 of 2 fixes applied)");
        assert_eq!(
            shown.lines().next(),
            Some(announced.as_str()),
            "the first line names stdin and the counts; got:\n{shown}"
        );
        assert_eq!(
            shown.matches("Partially fixed:").count(),
            1,
            "announced once; got:\n{shown}"
        );
        assert!(
            shown.contains(PARTIAL_LEFT) && shown.contains(AFTER_LINE),
            "the finding the fix left follows, framed over the emitted source; got:\n{shown}"
        );
        assert!(
            !shown.contains(BEFORE_LINE),
            "not framed over the source as given; got:\n{shown}"
        );
        assert!(
            !shown.contains(PARTIAL_REMOVED),
            "the finding the fix removed does not show; got:\n{shown}"
        );
        assert!(
            product.contains(PARTIAL_AFTER),
            "the partly fixed source goes to stdout; got:\n{product}"
        );
    }

    /// #217: a path that is not under the lint root must be an `Io` error, never a
    /// key silently rebuilt out of the path's own components.
    ///
    /// The old body used `strip_prefix(root).unwrap_or(path)`, so an out-of-root
    /// path fell through to a `Normal`-component join of the FULL host path — a
    /// `files[].file` key describing a location outside the directory the user
    /// asked to lint, with the leading `/` quietly dropped.
    ///
    /// Positive control: the in-root arm must still return `Ok`, otherwise
    /// an unconditional `Err` would satisfy the rejection assertion while breaking
    /// every real path.
    #[test]
    fn relative_display_rejects_path_outside_root() {
        use super::relative_display;
        use std::path::Path;

        let root = Path::new("/lint-root");

        // Absolute out-of-root: the strip fails, and the first component of the
        // would-be fallback is `RootDir`.
        let err = relative_display(Path::new("/other/x.mds"), root)
            .expect_err("a path outside the lint root must not produce a display key");
        let message = err.to_string();
        assert!(
            message.contains("escapes lint root"),
            "out-of-root rejection must name the condition; got: {message:?}"
        );

        // RELATIVE out-of-root: every component is `Normal`, so the component
        // guard cannot see this one — only the `strip_prefix` result can reject
        // it. Without this arm a reintroduced `strip_prefix(root).unwrap_or(path)`
        // still satisfies the absolute arm above and goes unnoticed.
        let rel_root = Path::new("lint-root");
        let rel_err = relative_display(Path::new("other/x.mds"), rel_root)
            .expect_err("a relative path outside the lint root must not produce a display key");
        let rel_message = rel_err.to_string();
        assert!(
            rel_message.contains("escapes lint root"),
            "relative out-of-root rejection must name the condition; got: {rel_message:?}"
        );

        // CONTROL ARMS: in-root paths must still succeed with the unchanged key,
        // under both an absolute and a relative root.
        assert_eq!(
            relative_display(Path::new("/lint-root/a/b.mds"), root)
                .expect("an in-root path must still produce a display key"),
            "a/b.mds",
            "control: the in-root key must be byte-identical to the pre-#217 output"
        );
        assert_eq!(
            relative_display(Path::new("lint-root/a/b.mds"), rel_root)
                .expect("an in-root path under a relative root must still produce a key"),
            "a/b.mds",
            "control: the relative-root in-root key must be byte-identical too"
        );
    }

    /// #217: a directory entry whose name is not valid UTF-8 must be an `Io` error,
    /// never a lossy key.
    ///
    /// The old body ran `to_string_lossy` over each component, so an entry named
    /// with two invalid bytes became a key of two U+FFFD replacement characters —
    /// a `files[].file` value that names no file on disk and collides with every
    /// other undecodable name in the tree.
    ///
    /// The invalid bytes are built at RUNTIME from numeric values; no escape
    /// sequence or raw byte appears in this source file (Source hygiene gate).
    ///
    /// Positive control: the valid-UTF-8 arm must still return `Ok`.
    ///
    /// `#[cfg(unix)]`: builds the non-UTF-8 name with `OsStrExt` (arbitrary bytes), a
    /// Unix-only API; Windows paths are UTF-16 and have no such construction (#147).
    #[cfg(unix)]
    #[test]
    fn relative_display_rejects_non_utf8_component() {
        use super::relative_display;
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;

        let root = Path::new("/lint-root");

        // 0xFF and 0xFE are not legal UTF-8 lead bytes in any position.
        let raw: Vec<u8> = vec![0xff, 0xfe, b'.', b'm', b'd', b's'];
        let hostile = root.join(std::ffi::OsStr::from_bytes(&raw));

        let err = relative_display(&hostile, root)
            .expect_err("a non-UTF-8 entry name must not produce a display key");
        let message = err.to_string();
        assert!(
            message.contains("not valid UTF-8"),
            "non-UTF-8 rejection must name the condition; got: {message:?}"
        );

        // CONTROL ARM: a valid-UTF-8 sibling in the same root must still succeed.
        assert_eq!(
            relative_display(&root.join("ok.mds"), root)
                .expect("a valid-UTF-8 entry must still produce a display key"),
            "ok.mds",
            "control: the valid-UTF-8 key must be byte-identical to the pre-#217 output"
        );
    }

    /// #390: `path is not valid UTF-8` names the path escaped, as every path in a message
    /// is. A directory walk builds the key of every entry before any of them is read, so
    /// no per-file refusal of a forbidden character stands in front of this message for a
    /// walked name: one that is not valid UTF-8 and also holds a newline (or ESC) would
    /// otherwise put the raw byte into the error text.
    ///
    /// Both bytes and their escaped forms are built at run time from numbers, so neither
    /// a control byte nor an escape sequence appears in this file.
    ///
    /// Positive controls: the path's own text carries the raw byte, so the vector reaches
    /// the message; an ordinary name that is not UTF-8 keeps its text, U+FFFD included.
    ///
    /// `#[cfg(unix)]`: builds the name with `OsStrExt` (arbitrary bytes), as
    /// `relative_display_rejects_non_utf8_component` does.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_escaped_in_its_message() {
        use super::{read_canonical_source, relative_display};
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;

        let root = Path::new("/lint-root");
        let lossy = char::REPLACEMENT_CHARACTER;
        for control in [0x0a_u8, 0x1b] {
            let raw = [0xff, b'a', control, b'b', b'.', b'm', b'd', b's'];
            let hostile = root.join(OsStr::from_bytes(&raw));
            let raw_char = char::from(control);
            assert!(
                hostile.to_string_lossy().contains(raw_char),
                "control: the hostile path must carry the raw byte 0x{control:02X}"
            );
            let expected = format!(
                "path is not valid UTF-8: /lint-root/{lossy}a{}u{control:04X}b.mds",
                '\\'
            );
            let walked = relative_display(&hostile, root)
                .expect_err("a non-UTF-8 entry name must not produce a display key")
                .to_string();
            let read = read_canonical_source(&hostile, &hostile)
                .expect_err("a non-UTF-8 path must not be read")
                .to_string();
            for message in [walked, read] {
                assert_eq!(
                    message, expected,
                    "the path must be shown escaped, never with the raw byte 0x{control:02X}"
                );
            }
        }

        let plain = root.join(OsStr::from_bytes(&[0xff, b'.', b'm', b'd', b's']));
        assert_eq!(
            relative_display(&plain, root)
                .expect_err("a non-UTF-8 entry name must not produce a display key")
                .to_string(),
            format!("path is not valid UTF-8: /lint-root/{lossy}.mds"),
            "control: an ordinary name that is not UTF-8 keeps its text"
        );
    }

    /// #390: `path escapes lint root` names the root and the path escaped. The walk cannot
    /// produce such a path, so this guards the text, not a reachable vector. Both control
    /// characters are built at run time.
    ///
    /// Positive control: an ordinary pair keeps its text.
    #[test]
    fn a_path_outside_the_lint_root_is_escaped_in_its_message() {
        use super::relative_display;
        use std::path::Path;

        let (lf, esc) = (char::from(0x0a_u8), char::from(0x1b_u8));
        let root = format!("/lint{esc}root");
        let path = format!("/other{lf}x.mds");
        let message = relative_display(Path::new(&path), Path::new(&root))
            .expect_err("a path outside the lint root must not produce a display key")
            .to_string();
        assert_eq!(
            message,
            format!(
                "path escapes lint root /lint{}u001Broot: /other{}u000Ax.mds",
                '\\', '\\'
            ),
            "the root and the path must be shown escaped"
        );

        assert_eq!(
            relative_display(Path::new("/other/x.mds"), Path::new("/lint-root"))
                .expect_err("a path outside the lint root must not produce a display key")
                .to_string(),
            "path escapes lint root /lint-root: /other/x.mds",
            "control: an ordinary pair keeps its text"
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
    ///
    /// `#[cfg(unix)]`: on Windows `\` is a path separator, so there is no literal
    /// backslash name byte to preserve (#147).
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

        let display_subdir = relative_display(real_subdir, root).expect("in-root path");
        let display_backslash = relative_display(backslash_name, root).expect("in-root path");

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
    ///
    /// Runs on every host: the paths are only built and compared in memory, never
    /// created on disk, so a Windows file name's character rules do not apply (#147).
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

        let ctrl_raw = relative_display(ctrl_path, root).expect("in-root path");
        let normal_raw = relative_display(normal_path, root).expect("in-root path");

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

    /// Platform-independent: directory-mode sort must be byte-wise ascending over
    /// the normalised forward-slash display strings emitted by `relative_display`.
    ///
    /// Constructs paths via `PathBuf::join` (platform-agnostic component builder)
    /// so the test runs unmodified on both Unix and Windows without a `#[cfg]` guard.
    /// `relative_display` always joins with `/`, so the sort key is identical on both
    /// platforms.
    ///
    /// Critical ordering property: a flat file whose name starts with `sub[` must
    /// sort AFTER a nested file in directory `sub/`, because `/` (0x2F) < `[` (0x5B).
    /// If `relative_display` were changed to use the native separator (`\`, 0x5C on
    /// Windows), the `[` (0x5B) file would sort BEFORE the nested path (0x5B < 0x5C),
    /// reversing the intended order — and this test would fail on any platform where
    /// the temporary separator change is in effect (ADR-009 verified by changing
    /// `join("/")` to `join("\\")` in `relative_display`, confirming the test fails).
    ///
    /// The test exercises the same key function used by `run_lint_directory`'s
    /// `sort_by_cached_key` (F1): `sanitize_control_chars_wire(relative_display(p, dir))`.
    /// No filesystem access is needed — `relative_display` is a pure path computation.
    #[test]
    fn directory_sort_ascending_over_normalized_display_strings() {
        use super::relative_display;
        use std::path::PathBuf;

        // Build paths via PathBuf::join — uses native separator internally, but
        // `relative_display` always emits `/`-joined strings regardless of platform.
        let root = PathBuf::from("lint-root");
        let file_a = root.join("a.mds"); // flat: display "a.mds"
        let file_sub_a = root.join("sub").join("a.mds"); // nested: display "sub/a.mds"
        let file_sub_bracket = root.join("sub[after.mds"); // flat, [ in name: "sub[after.mds"
        let file_z = root.join("z.mds"); // flat: display "z.mds"

        // Verify display strings first (documents intent and catches platform drift).
        let display_a = relative_display(&file_a, &root).expect("in-root path");
        let display_sub_a = relative_display(&file_sub_a, &root).expect("in-root path");
        let display_sub_bracket = relative_display(&file_sub_bracket, &root).expect("in-root path");
        let display_z = relative_display(&file_z, &root).expect("in-root path");

        assert_eq!(
            display_a, "a.mds",
            "flat file must display without separator"
        );
        assert_eq!(
            display_sub_a, "sub/a.mds",
            "nested file must use forward-slash separator"
        );
        assert_eq!(
            display_sub_bracket, "sub[after.mds",
            "flat file with [ in name must display as single component"
        );
        assert_eq!(
            display_z, "z.mds",
            "flat file must display without separator"
        );

        // Sort by the same key as `run_lint_directory` (F1): sanitized display string.
        let mut paths = [
            file_z.clone(),
            file_sub_bracket.clone(),
            file_a.clone(),
            file_sub_a.clone(),
        ];
        paths.sort_by_cached_key(|p| {
            mds::sanitize_control_chars_wire(&relative_display(p, &root).expect("in-root path"))
                .into_owned()
        });

        let sorted: Vec<String> = paths
            .iter()
            .map(|p| relative_display(p, &root).expect("in-root path"))
            .collect();

        // Expected byte-wise order:
        //   "a.mds"        — 'a' (0x61)
        //   "sub/a.mds"    — 's' then '/' (0x2F) at position 3
        //   "sub[after.mds"— 's' then '[' (0x5B) at position 3  (0x2F < 0x5B)
        //   "z.mds"        — 'z' (0x7A)
        //
        // If relative_display used '\' (0x5C) instead of '/' (0x2F):
        //   "sub[after.mds" would sort BEFORE "sub\a.mds" (0x5B < 0x5C) — wrong.
        assert_eq!(
            sorted,
            vec!["a.mds", "sub/a.mds", "sub[after.mds", "z.mds"],
            "directory sort must be byte-wise ascending over normalised forward-slash \
             display strings; '/' (0x2F) must sort before '[' (0x5B) so nested paths \
             appear before same-prefix flat files with bracket-containing names"
        );
    }

    /// Windows-only: `relative_display` must normalise the native backslash separator
    /// to a forward slash in the emitted display string.
    ///
    /// This test will not execute in CI today (the Rust matrix uses `ubuntu-latest`
    /// only — adding `windows-latest` is a separate scope decision), but it documents
    /// the expected behaviour and will start running the moment the matrix is extended.
    #[cfg(windows)]
    #[test]
    fn relative_display_normalizes_windows_separator_to_forward_slash() {
        use super::relative_display;
        use std::path::Path;

        // Windows absolute path: C:\proj\sub\c.mds with root C:\proj
        let path = Path::new(r"C:\proj\sub\c.mds");
        let root = Path::new(r"C:\proj");
        let display = relative_display(path, root).expect("in-root path");

        assert_eq!(
            display, "sub/c.mds",
            "relative_display must emit forward-slash separator on Windows; \
             got {display:?} — native backslash must not appear in the wire key"
        );
    }

    /// PF-004: a failure to anchor the display root is reported, not swallowed.
    ///
    /// `read_source_file` only reaches `read_canonical_source` after
    /// `check_symlink` has canonicalized the path, so the anchor cannot fail
    /// through it without a race; the split exists so this is testable. A
    /// canonical path whose directory is missing makes the anchor fail. With the
    /// failure swallowed the read would then fail instead, as `cannot read
    /// x.mds` — the message this test tells apart.
    #[test]
    fn read_canonical_source_reports_an_anchor_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let missing = root.join("gone").join("x.mds");

        let err = super::read_canonical_source(&missing, &missing).unwrap_err();
        assert!(
            matches!(err, mds::MdsError::Io { .. }),
            "expected mds::io, got {err:?}"
        );
        assert!(
            err.to_string().contains("cannot resolve path"),
            "the anchor failure must surface, got: {err}"
        );
        assert_eq!(super::mds_error_exit_code(&err), 2);

        // Control: an existing file under an anchorable directory reads.
        let file = root.join("ok.mds");
        std::fs::write(&file, "Hello!\n").unwrap();
        assert_eq!(
            super::read_canonical_source(&file, &file).unwrap(),
            "Hello!\n"
        );
    }
}
