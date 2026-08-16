//! Integration tests for `mds lint` (issue #61).
//!
//! Coverage maps to acceptance criteria (issue #61):
//! - L-CLI-CHAN1: clean file → exit 0, diagnostics to stderr, stdout empty (human)
//! - L-CLI-CHAN2: warn-only file → exit 1, warning on stderr
//! - L-CLI-CHAN3: error-severity file → exit 2, error on stderr
//! - L-CLI-CHAN4: analysis gate failure → exit 2 (not lint exit 1)
//! - L-CLI-JSON1: --format json clean → exit 0, JSON to stdout, stderr empty
//! - L-CLI-JSON2: --format json warn → exit 1, JSON with diagnostics to stdout
//! - L-CLI-JSON3: --format json gate failure → exit 2, error envelope to stdout
//! - L-CLI-JSON4: --format json nonexistent path → exit 2, JSON error envelope stdout (AC-F-14)
//! - L-CLI-JSON5: --format json malformed mds.json → exit 2, JSON error envelope stdout (AC-F-14)
//! - L-CLI-FIX1: --fix applies auto-fixable issues in place (Tier A)
//! - L-CLI-FIX2: --fix --check exits 1 if fixes pending, never writes
//! - L-CLI-FIX3: block-spanning empty-block --fix applied; file changed, exit 0 (TEST-3)
//! - L-CLI-STDIN1: stdin (no fix) → diagnostics to stderr, stdout empty
//! - L-CLI-STDIN2: --fix stdin → fixed source to stdout, diagnostics to stderr
//! - L-CLI-VARS: --set passes runtime variables to the gate check
//! - L-CLI-QUIET1: --quiet suppresses warnings, exit 0 on clean
//! - L-CLI-DIR1: directory mode path-sorts and lints all files including partials
//! - L-CLI-RESOURCE: nesting > MAX_NESTING_DEPTH (64) → exit 3 ResourceLimit (TEST-4)
//! - L-CLI-DIR2: directory --format json files[] order is deterministic (TEST-6)
//! - I-24: unreachable-branch (always-true @if) --fix applied; file changed, exit 0
//! - I-26: shadow-variable Info severity emits diagnostic and exits 0 (Info never affects exit)
//! - AC-224-10: unknown rule name → JSON wire shape unchanged (lint continues)
//! - AC-224-11: unknown rule warning goes to stderr, not stdout
//! - AC-224-14: D2(a) asymmetry — build/check/fmt do NOT warn (see cli_build.rs)
//! - AC-224-19: directory with N files emits exactly ONE unknown-rule warning
//! - AC-224-21: stdout JSON remains valid when unknown rule name is present
//! - AC-224-22: --quiet suppresses the unknown-rule warning
//! - AC-Q06 (#216): clean directory prints exact summary line, nothing else
//! - AC-Q07 (#216): mixed-severity directory yields exact per-bucket counts
//! - AC-Q08 (#216): counters partition the file set with no drops or double-counts
//! - AC-Q09 (#216): resource-limited bucket is genuinely exercised
//! - AC-Q10 (#216): quiet warn-only lint is silent (with positive control)
//! - AC-Q11 (#216): quiet lint with error-severity findings still prints summary
//! - AC-Q12 (#216): quiet lint with resource-limit still prints summary
//! - AC-Q13 (#216): --fix --check exit path does not skip the summary
//! - AC-Q14 (#216): warn-only and --fix --check --quiet are silent (D1-a)
//! - AC-Q15 (#216): JSON directory mode keeps stdout clean; summary on stderr only
//! - AC-Q16 (#216): JSON envelope key contract is exactly {"files","truncated","version"}
//! - AC-Q18 (#216): single-file lint prints no directory summary
//! - AC-Q19 (#216): stdin lint prints no directory summary
//! - AC-Q20 (#216): bare `mds lint` cannot reach directory mode
//! - AC-Q21 (#216): empty directory prints no summary
//! - AC-Q22 (#216): all-excluded quiet lint still emits diagnostic and no summary
//! - AC-Q25 (#216): hostile filename cannot forge a summary line (with positive control)
//! - AC-Q26 (#216): help text documents quiet summary contract (machine-verified)
//! - AC-Q30 (#216): --quiet works in both argument positions for lint
//! - D4 (#216, PF-004): `fix rejected:` honours --quiet in directory (human + JSON),
//!   single-file, and stdin mode — four separate emitters, each with its own positive control
//! - D4 (#216, PF-004): diagnostic-cap notice honours --quiet in all four input modes
//!   (directory-human, directory-JSON, single-file, stdin) with paired positive controls
//!   (ADR-009/PF-013)
//! - D3-a (#216): config-load failure is counted in the "with errors" bucket (not "clean")
//! - D3-a (#216): config-load failure forces the summary under --quiet (positive control)
//! - D3-a (#216, unix): chmod-000 source-read failure counted in the "with errors" bucket
//! - D3-a (#216, unix): chmod-000 source-read failure forces summary under --quiet
//! - D4 (#216, PF-004): `Fixed:` honours --quiet in dir-mode JSON emitter (positive + negative)
//! - D4 (#216, PF-004): `Fixed:` honours --quiet in dir-mode human emitter (positive + negative)
//! - D4 (#216, PF-004): `Would fix:` honours --quiet in dir-mode JSON emitter (positive + negative)
//! - D4 (#216, PF-004, unix): write failure in dir-JSON mode must not print `Fixed:` (positive + negative)
//! - D1-a (#216): `--fix --diff --format json` diff precedes JSON envelope on stdout

mod common;
use common::{assert_no_control_chars, fixture, mds_bin};

use std::fs;
use std::path::Path;

/// Run `mds lint <path> [extra_args]`, capturing stdout + stderr separately.
fn lint_path(path: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("lint")
        .arg(path)
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap()
}

/// Run `mds lint [args]` with stdin provided as `input`.
fn lint_stdin(input: &str, extra_args: &[&str]) -> std::process::Output {
    use std::io::Write;
    let mut child = mds_bin()
        .arg("lint")
        .args(extra_args)
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Ignore BrokenPipe — the child may exit before reading stdin
    // (e.g. a usage error detected before the process reads any input).
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

// ── L-CLI-CHAN1: clean file ───────────────────────────────────────────────────

#[test]
fn clean_file_exits_0_with_empty_stdout() {
    let out = lint_path(&fixture("lint_clean.mds"), &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean file should exit 0; stderr: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "human mode must not write to stdout; got: {stdout}"
    );
}

// ── L-CLI-CHAN2: warn-only file ───────────────────────────────────────────────

#[test]
fn warn_only_file_exits_1_with_diagnostic_on_stderr() {
    let out = lint_path(&fixture("lint_warn_only.mds"), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "warn-only file should exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("unused-variable"),
        "expected unused-variable diagnostic on stderr; got: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "human mode must not write to stdout; got: {stdout}"
    );
}

// ── L-CLI-SPAN: span context in human render (Step 0, #61) ──────────────────

/// Verify that miette renders span-labeled source context (source line + caret)
/// for findings with a span. Uses lint_warn_only.mds which triggers unused-variable
/// with approx_offset pointing at the `unused_key` frontmatter line.
///
/// The rendered stderr must contain the source text from the offending line so the
/// user can see WHERE in the file the finding is.
#[test]
fn span_source_context_appears_in_human_render() {
    let out = lint_path(&fixture("lint_warn_only.mds"), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The source line text from the fixture's frontmatter — miette renders it
    // in the source context block when labels() and with_source_code() are wired.
    // The fixture's third line is: "unused_key: this key is never referenced in the body"
    assert!(
        stderr.contains("unused_key"),
        "expected span context with 'unused_key' source text in miette render; got: {stderr}"
    );
    // Miette includes the file+line reference when source is attached.
    assert!(
        stderr.contains("lint_warn_only.mds"),
        "expected filename reference in miette span render; got: {stderr}"
    );
}

// ── L-CLI-CHAN3: error-severity file ─────────────────────────────────────────

#[test]
fn error_file_exits_2_with_error_diagnostic_on_stderr() {
    let out = lint_path(&fixture("lint_error.mds"), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(2),
        "error-severity finding should exit 2; stderr: {stderr}"
    );
    assert!(
        stderr.contains("duplicate-export"),
        "expected duplicate-export error on stderr; got: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "human mode must not write to stdout; got: {stdout}"
    );
}

// ── L-CLI-CHAN4: analysis gate failure ────────────────────────────────────────

#[test]
fn analysis_gate_failure_exits_2_not_lint_codes() {
    let out = lint_path(&fixture("lint_gate_fail.mds"), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "gate failure (file not found) should exit 2; stderr: {stderr}"
    );
    // MdsError should be rendered; no lint-specific content
    assert!(
        stderr.contains("file not found") || stderr.contains("lint_nonexistent"),
        "expected file-not-found error on stderr; got: {stderr}"
    );
}

// ── L-CLI-JSON1: --format json, clean file ───────────────────────────────────

#[test]
fn json_format_clean_file_exits_0_with_json_to_stdout() {
    let out = lint_path(&fixture("lint_clean.mds"), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean file --format json should exit 0; stderr: {stderr}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert_eq!(json["version"], 1, "version must be 1");
    assert!(json["files"].is_array(), "must have files array");
    assert!(
        stderr.is_empty(),
        "JSON mode must not write to stderr when clean; got: {stderr}"
    );
}

// ── L-CLI-JSON2: --format json, warn file ────────────────────────────────────

#[test]
fn json_format_warn_file_exits_1_with_diagnostic_json() {
    let out = lint_path(&fixture("lint_warn_only.mds"), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "warn-only --format json should exit 1; stderr: {stderr}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert_eq!(json["version"], 1);
    let files = json["files"].as_array().expect("files must be array");
    assert!(!files.is_empty(), "files must be non-empty");
    let diags = files[0]["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty(), "diagnostics must be non-empty");
    assert_eq!(diags[0]["rule"], "unused-variable");
    assert_eq!(diags[0]["severity"], "warn");
    // Fixable: unused-variable is Tier C (never fixed automatically)
    assert_eq!(diags[0]["fixable"], false);
    assert!(
        stderr.is_empty(),
        "JSON mode must not write diagnostics to stderr; got: {stderr}"
    );
}

// ── L-CLI-JSON3: --format json, gate failure ─────────────────────────────────

#[test]
fn json_format_gate_failure_exits_2_with_error_envelope_to_stdout() {
    let out = lint_path(&fixture("lint_gate_fail.mds"), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(2),
        "gate failure --format json should exit 2"
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON error envelope");
    assert_eq!(json["version"], 1);
    assert!(
        json["error"].is_object(),
        "error envelope must have 'error' key"
    );
    // stdout only — human rendering must not appear on stdout
    assert!(
        !stdout.contains("help:"),
        "human rendering must not appear in JSON stdout"
    );
}

// ── L-CLI-FIX1: --fix applies auto-fixable issues ────────────────────────────

#[test]
fn fix_applies_auto_fixable_issues_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_error.mds");
    fs::copy(fixture("lint_error.mds"), &target).unwrap();

    let original = fs::read_to_string(&target).unwrap();
    assert!(
        original.contains("@export greet\n@export greet"),
        "fixture must have duplicate export"
    );

    let out = lint_path(&target, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--fix should exit 0 after fixing duplicate-export; stderr: {stderr}"
    );

    let after = fs::read_to_string(&target).unwrap();
    assert!(
        !after.contains("@export greet\n@export greet"),
        "duplicate export should be removed after --fix; got:\n{after}"
    );
    // Exactly one @export greet should remain
    assert_eq!(
        after.matches("@export greet").count(),
        1,
        "exactly one @export greet should remain; got:\n{after}"
    );
}

// ── L-CLI-FIX2: --fix --check exits 1 if fixes pending, never writes ─────────

#[test]
fn fix_check_exits_1_when_fixes_pending_and_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_error.mds");
    fs::copy(fixture("lint_error.mds"), &target).unwrap();

    let original = fs::read_to_string(&target).unwrap();

    let out = lint_path(&target, &["--fix", "--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check should exit 1 when fixes are pending; stderr: {stderr}"
    );

    // File must NOT have been modified.
    let after = fs::read_to_string(&target).unwrap();
    assert_eq!(original, after, "--fix --check must not write to the file");
}

// ── L-CLI-STDIN1: stdin (no fix) ─────────────────────────────────────────────

#[test]
fn stdin_mode_report_only_sends_diagnostics_to_stderr() {
    let source = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    let out = lint_stdin(source, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "duplicate-export from stdin should exit 2; stderr: {stderr}"
    );
    assert!(
        stderr.contains("duplicate-export"),
        "diagnostic must appear on stderr; got: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "stdin report-only mode must not write to stdout; got: {stdout}"
    );
}

// ── L-CLI-STDIN2: --fix stdin ────────────────────────────────────────────────

#[test]
fn stdin_fix_mode_writes_fixed_source_to_stdout() {
    let source = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    let out = lint_stdin(source, &["--fix"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--fix stdin should exit 0 after fixing; stderr: {stderr}"
    );
    // Fixed source goes to stdout (filter mode)
    assert!(
        stdout.contains("@export greet"),
        "fixed source must appear on stdout; got: {stdout}"
    );
    // Should have exactly one @export greet
    assert_eq!(
        stdout.matches("@export greet").count(),
        1,
        "exactly one @export should remain in fixed source; got: {stdout}"
    );
    // Diagnostics go to stderr (or are empty after fix)
    drop(stderr);
}

// ── L-CLI-VARS: --set passes runtime variables ────────────────────────────────

#[test]
fn set_var_passes_runtime_variable_to_gate_check() {
    // Without --set: gate fails (UndefinedVariable) → exit 2
    let out_no_var = lint_path(&fixture("lint_var_required.mds"), &[]);
    assert_eq!(
        out_no_var.status.code(),
        Some(2),
        "missing required_var should exit 2 (gate failure)"
    );

    // With --set required_var=foo: gate passes, finds unused_key warning → exit 1
    let out_with_var = lint_path(
        &fixture("lint_var_required.mds"),
        &["--set", "required_var=foo"],
    );
    let stderr = String::from_utf8_lossy(&out_with_var.stderr);
    assert_eq!(
        out_with_var.status.code(),
        Some(1),
        "with required_var set, should exit 1 (unused_key warning); stderr: {stderr}"
    );
    assert!(
        stderr.contains("unused-variable"),
        "unused_key should be flagged; got: {stderr}"
    );
}

// ── L-CLI-QUIET1: --quiet suppresses warnings ─────────────────────────────────

#[test]
fn quiet_flag_suppresses_warnings_on_warn_only_file() {
    let out = lint_path(&fixture("lint_warn_only.mds"), &["--quiet"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // --quiet suppresses warnings; exit code is still based on severity
    // (warn-only file still exits 1 — quiet only suppresses rendering)
    assert_eq!(
        out.status.code(),
        Some(1),
        "--quiet warn-only should still exit 1; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("unused-variable"),
        "--quiet should suppress warning diagnostic; got: {stderr}"
    );
    assert!(
        stdout.is_empty(),
        "stdout must remain empty with --quiet; got: {stdout}"
    );
}

// ── L-CLI-DIR1: directory mode ───────────────────────────────────────────────

#[test]
fn directory_mode_lints_all_files_including_partials() {
    let dir = tempfile::tempdir().unwrap();
    // Copy only the dedicated lint fixtures into a clean temp dir
    for name in &["lint_clean.mds", "lint_warn_only.mds", "_lint_partial.mds"] {
        fs::copy(fixture(name), dir.path().join(name)).unwrap();
    }

    let out = lint_path(dir.path(), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Directory has a warn-only file → exit 1
    assert_eq!(
        out.status.code(),
        Some(1),
        "directory with warn-only file should exit 1; stderr: {stderr}"
    );
    // The warning from lint_warn_only.mds should appear
    assert!(
        stderr.contains("unused-variable"),
        "unused-variable warning should appear in directory mode; got: {stderr}"
    );
}

// ── L-CLI-USAGE-ERR: --fix --format json stdin → exit 2 ─────────────────────

#[test]
fn fix_json_stdin_is_usage_error_exit_2() {
    let out = lint_stdin("Hello {{name}}!", &["--fix", "--format", "json"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "--fix --format json stdin must exit 2 (usage error)"
    );
    // Error message must go to stderr
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.is_empty(),
        "usage error message must appear on stderr"
    );
}

// ── L-CLI-JSON4: nonexistent path → JSON error envelope ─────────────────────
//
// AC-F-14: in --format json mode, every analysis-failure path — including
// "file not found" — must emit `{"version":1,"error":{...}}` to stdout, not a
// human message to stderr. exit 2 is unchanged.

#[test]
fn json_format_nonexistent_path_emits_error_envelope() {
    // Use a path that is guaranteed not to exist.
    let out = lint_path(
        Path::new("/nonexistent_mds_lint_test_12345.mds"),
        &["--format", "json"],
    );

    // Exit code must be 2 (analysis failure — file not found).
    assert_eq!(
        out.status.code(),
        Some(2),
        "--format json + nonexistent path must exit 2"
    );

    // stdout must be a parseable JSON error envelope, NOT empty.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout must be valid JSON (error envelope); parse error: {e}; stdout: {stdout}")
    });
    assert_eq!(
        parsed["version"].as_u64(),
        Some(1),
        "envelope must have version:1; got: {parsed}"
    );
    let code = parsed["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("error.code must be a string; got: {parsed}"));
    assert_eq!(
        code, "mds::file_not_found",
        "error code must be mds::file_not_found; got: {code}"
    );

    // The human error message must NOT appear on stderr (it goes to stdout in JSON mode).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("mds::file_not_found"),
        "JSON mode must not print error to stderr; got stderr: {stderr}"
    );
}

// ── C4/F6: existence-before-extension ordering for lint ──────────────────────

/// A non-existent path that also lacks the `.mds` extension must report
/// `mds::file_not_found`, NOT `mds::not_mds_file` (C4/F6).
///
/// Before this fix, `ensure_mds_extension` ran before the file was opened, so
/// `/nonexistent.txt` produced a confusing "not an .mds file" diagnostic.
/// Now `ensure_existing_mds_file` checks existence first.
#[test]
fn lint_nonexistent_non_mds_reports_file_not_found_not_extension_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing_12345.txt");

    let out = lint_path(&missing, &[]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "non-existent .txt file must exit 2; got: {:?}",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mds::file_not_found") || stderr.contains("file not found"),
        "error must be file-not-found (not extension error); got: {stderr:?}"
    );
    assert!(
        !stderr.contains("mds::not_mds_file") && !stderr.contains("not an .mds"),
        "must NOT report 'not an .mds file' for a non-existent path; got: {stderr:?}"
    );
}

/// Same as above but in `--format json` mode: the error envelope must have
/// code `mds::file_not_found`, not `mds::not_mds_file`.
#[test]
fn lint_nonexistent_non_mds_json_mode_reports_file_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing_12345.txt");

    let out = lint_path(&missing, &["--format", "json"]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "--format json + nonexistent .txt must exit 2"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("stdout must be valid JSON (error envelope); parse error: {e}; stdout: {stdout}")
    });
    let code = parsed["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("error.code must be a string; got: {parsed}"));
    assert_eq!(
        code, "mds::file_not_found",
        "JSON envelope code must be mds::file_not_found (not mds::not_mds_file); got: {code}"
    );
}

// ── L-CLI-FIX3: block-spanning --fix refusal (TEST-3) ────────────────────────
//
// Exercises the KB gotcha "block-spanning Tier A fixes are always refused":
// Block-span empty @define is now fixed correctly. The fix_removals field
// carries a FixLineSpan covering the full @define..@end block. The reverify
// gate parses the fixed source (empty file) cleanly, confirms no output delta,
// and accepts the fix. (per ADR-001 block-span discipline)
//
// Fixture: lint_block_span_empty.mds — multi-line empty @define whose body is
// a single blank line (whitespace-only Text node). The empty-block rule fires
// (Tier A, Warn). The fix removes the whole block → empty file → exit 0.

#[test]
fn fix_applied_for_block_spanning_empty_define_and_file_changed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_block_span_empty.mds");
    fs::copy(fixture("lint_block_span_empty.mds"), &target).unwrap();

    let original = fs::read_to_string(&target).unwrap();
    assert!(
        original.contains("@define empty_fn():"),
        "fixture must contain the multi-line empty @define"
    );

    let out = lint_path(&target, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The fix must succeed: block-span fix_removals removes the whole @define..@end block.
    assert!(
        !stderr.contains("fix rejected"),
        "block-spanning empty-block fix must now succeed; got stderr: {stderr}"
    );

    // No residual findings → clean exit.
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit code must be 0 after successful empty-block fix; got stderr: {stderr}"
    );

    // The file on disk must be CHANGED — the empty @define block is removed.
    let after = fs::read_to_string(&target).unwrap();
    assert_ne!(
        original, after,
        "file must be changed when --fix succeeds for empty-block"
    );
    assert!(
        !after.contains("@define empty_fn():"),
        "fixed file must not contain the removed @define; got: {after:?}"
    );
}

// ── L-CLI-RESOURCE: exit code 3 for ResourceLimit (TEST-4) ──────────────────
//
// Verifies that a template causing a ResourceLimit in the RESOLVER causes
// `mds lint` to exit 3. We trigger MAX_BLOCKS_PER_MODULE (256) by writing
// 257 uniquely-named @block declarations. The resolver's collect_block()
// increments a counter on each @block and fails with MdsError::ResourceLimit
// when count > MAX_BLOCKS_PER_MODULE (i.e. on the 257th block). The check
// gate propagates this error through mds::lint() and the CLI maps
// MdsError::ResourceLimit to exit code 3 via mds_error_exit_code.
//
// Note: deeply nested @if/@for blocks (> MAX_NESTING_DEPTH=64) are caught
// by the PARSER with MdsError::Syntax (exit 2, not 3); the facts walker's
// ResourceLimit for nesting is never reached because the parser fails first.
// The @block approach is used here because it triggers ResourceLimit in the
// resolver (inside the check gate), which is the correct path to exit code 3.

#[test]
fn too_many_block_declarations_exits_3_resource_limit() {
    // MAX_BLOCKS_PER_MODULE is 256; the 257th @block triggers ResourceLimit.
    // All blocks must have unique names (the resolver rejects duplicates).
    const BLOCK_COUNT: usize = 257;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("many_blocks.mds");

    let mut content = String::new();
    for i in 0..BLOCK_COUNT {
        content.push_str(&format!("@block block_{i}:\n@end\n"));
    }
    fs::write(&target, &content).unwrap();

    let out = lint_path(&target, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{BLOCK_COUNT} @block declarations must exit 3 (ResourceLimit, MAX_BLOCKS_PER_MODULE); \
         stderr: {stderr}"
    );
}

// ── L-CLI-DIR2: directory --format json file-order determinism (TEST-6) ──────
//
// Verifies the F1 invariant: files[] in --format json directory-mode output is
// always in sorted (path-ascending) order regardless of filesystem walk order.
// Exercises the explicit `files.sort()` in run_lint_directory, which is
// required because collect_mds_files() does NOT guarantee any walk order.
//
// Files MUST carry diagnostics: to_canonical_json() only emits file entries
// for files that have at least one diagnostic — clean files are omitted from
// the files[] array entirely. We use lint_warn_only.mds (unused-variable warn)
// so all 3 copies produce entries in the output.
//
// Files are created in REVERSE alphabetical order (c→b→a) to stress-test the
// F1 sort; an absent sort would produce non-deterministic or reverse output.
// Two consecutive runs are compared to assert cross-run determinism.

#[test]
fn directory_json_files_array_is_deterministically_sorted() {
    let dir = tempfile::tempdir().unwrap();
    // Use warn-only files so each copy appears in the files[] JSON array.
    // Create them in reverse alphabetical order to stress-test the F1 sort.
    for name in &["lint_dir_c.mds", "lint_dir_b.mds", "lint_dir_a.mds"] {
        fs::copy(fixture("lint_warn_only.mds"), dir.path().join(name)).unwrap();
    }

    // Run twice to assert cross-run determinism.
    let out1 = lint_path(dir.path(), &["--format", "json"]);
    let out2 = lint_path(dir.path(), &["--format", "json"]);
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);

    assert_eq!(
        out1.status.code(),
        Some(1),
        "directory with warn-only files must exit 1; stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Both runs must produce byte-identical output (cross-run determinism).
    assert_eq!(
        stdout1, stdout2,
        "directory mode --format json output must be identical across two consecutive runs"
    );

    let json: serde_json::Value =
        serde_json::from_str(&stdout1).expect("stdout must be valid JSON");
    let files = json["files"]
        .as_array()
        .expect("JSON output must have a files array");
    assert_eq!(
        files.len(),
        3,
        "all 3 .mds files must appear in the output; got: {files:?}"
    );

    // F1 invariant: files[] must be in sorted (path-ascending) order.
    let paths: Vec<&str> = files
        .iter()
        .map(|f| {
            f["file"]
                .as_str()
                .expect("each entry must have a file string")
        })
        .collect();
    let mut sorted_paths = paths.clone();
    sorted_paths.sort();
    assert_eq!(
        paths, sorted_paths,
        "files[] must be in sorted path order (F1 invariant); got: {paths:?}"
    );
}

// ── I-24: unreachable-branch --fix succeeds for always-true @if + @else ───────
//
// unreachable-branch is Tier A (auto-fixable per tier.rs). With block-span
// fix_removals wired, always-true @if (case A) generates two spans:
//   • Span 1: remove the @if directive line (FixLineSpan::single(b.offset)).
//   • Span 2: remove from the first unreachable branch through @end (inclusive).
// The reverify gate parses the fixed source (just the then-body unwrapped),
// confirms no output delta, and accepts the fix.
//
// Fixture: lint_unreachable_branch.mds — always-true @if "x" == "x": with a
// "hello" then-body and an @else: branch with "world". After fix: only "hello"
// remains; exit 0 (no residual findings).

#[test]
fn fix_applied_for_unreachable_branch_and_file_changed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_unreachable_branch.mds");
    fs::copy(fixture("lint_unreachable_branch.mds"), &target).unwrap();

    let original = fs::read_to_string(&target).unwrap();
    assert!(
        original.contains("@if \"x\" == \"x\":"),
        "fixture must contain the always-true @if condition"
    );
    assert!(
        original.contains("@else:"),
        "fixture must contain a later @else branch"
    );

    let out = lint_path(&target, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The fix must succeed: two-span case-A removal unwraps the then-body.
    assert!(
        !stderr.contains("fix rejected"),
        "unreachable-branch fix must now succeed; got stderr: {stderr}"
    );

    // No residual findings → clean exit.
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit code must be 0 after successful unreachable-branch fix; got stderr: {stderr}"
    );

    // File is changed — the @if/@else/@end structure is removed, then-body unwrapped.
    let after = fs::read_to_string(&target).unwrap();
    assert_ne!(original, after, "file must be changed when --fix succeeds");
    assert!(
        !after.contains("@if"),
        "fixed file must not contain the removed @if; got: {after:?}"
    );
    assert!(
        !after.contains("@else"),
        "fixed file must not contain the removed @else branch; got: {after:?}"
    );
    assert!(
        after.contains("hello"),
        "fixed file must retain the then-body content; got: {after:?}"
    );
}

// ── I-26: shadow-variable Info severity → diagnostic emitted, exit 0 ─────────
//
// shadow-variable is default-off and always Info severity. When enabled via
// mds.json, Info findings ARE rendered to stderr in human mode but NEVER
// contribute to the exit code. This test asserts BOTH properties: the finding
// appears in output AND the process exits 0 — catching regressions that either
// suppress the diagnostic or wrongly escalate Info to a non-zero exit.
//
// Fixture: loop_var_shadow.mds — @for item in items: shadows the frontmatter
// key `item` (all vars are defined; check gate passes). mds.json in the same
// temp dir enables shadow-variable at Info severity (the built-in default when
// explicitly configured as "info"). The fixture has no Warn/Error findings, so
// the only diagnostic is the Info shadow-variable — exit must be 0.

#[test]
fn shadow_variable_info_emits_diagnostic_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("loop_var_shadow.mds");
    fs::copy(fixture("loop_var_shadow.mds"), &target).unwrap();

    // Enable shadow-variable at Info severity via mds.json in the same directory.
    // The upward config search finds this mds.json for the fixture in the same dir.
    fs::write(
        dir.path().join("mds.json"),
        r#"{ "lint": { "rules": { "shadow-variable": "info" } } }"#,
    )
    .unwrap();

    let out = lint_path(&target, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // The shadow-variable diagnostic must be emitted (rule is enabled and fires).
    assert!(
        stderr.contains("shadow-variable"),
        "shadow-variable Info finding must appear in stderr; got: {stderr}"
    );

    // Info severity never contributes to exit code — must exit 0.
    assert_eq!(
        out.status.code(),
        Some(0),
        "Info-severity shadow-variable must not affect exit code; got stderr: {stderr}"
    );

    // Human mode must not write to stdout.
    assert!(
        stdout.is_empty(),
        "human mode must not write to stdout; got: {stdout}"
    );
}

// ── L-CLI-JSON5: malformed mds.json → JSON error envelope ───────────────────
//
// AC-F-14: config-load failure must also emit the JSON envelope to stdout in
// --format json mode, so that JSON/LSP consumers always get parseable output.

#[test]
fn json_format_malformed_config_emits_error_envelope() {
    let dir = tempfile::tempdir().unwrap();
    // A syntactically valid .mds file — lint must fail at the config stage, not parse stage.
    let mds_file = dir.path().join("test.mds");
    fs::write(&mds_file, "---\nfoo: bar\n---\nHello world").unwrap();
    // Malformed mds.json in the same directory — will be found by the upward config walk.
    fs::write(dir.path().join("mds.json"), "{ this is not valid json }").unwrap();

    let out = lint_path(&mds_file, &["--format", "json"]);

    // Exit code must be 2 (config-load failure is an analysis failure).
    assert_eq!(
        out.status.code(),
        Some(2),
        "--format json + malformed mds.json must exit 2; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout must be a parseable JSON error envelope.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout must be valid JSON (error envelope); parse error: {e}; \
             stdout: {stdout}; stderr: {stderr}"
        )
    });
    assert_eq!(
        parsed["version"].as_u64(),
        Some(1),
        "envelope must have version:1; got: {parsed}"
    );
    assert!(
        parsed["error"]["code"].is_string(),
        "error envelope must have a code field; got: {parsed}"
    );

    // The error details must NOT be printed to stderr in JSON mode.
    assert!(
        !stderr.contains("invalid mds.json"),
        "config error must go to stdout in JSON mode, not stderr; got stderr: {stderr}"
    );
}

// ── Phase B pin tests ─────────────────────────────────────────────────────────

// ── Test (a): Dir-mode JSON distinct paths for same-basename files ────────────
//
// Pins bug-4 fix: in directory mode with --format json, the `file` key in each
// JSON diagnostic entry must be the relative path from the lint root, NOT the
// basename. Two files with the same basename in different subdirectories must
// produce two distinct `file` keys.
//
// Pre-Phase-B behavior: mds::lint() sets diag.file to the basename only
// (path.file_name()), so all three entries would share the key "template.mds".

#[test]
fn dir_mode_json_same_basename_files_have_distinct_paths() {
    let dir = tempfile::tempdir().unwrap();
    // Create two subdirs each with a file of the same basename but a diagnostic.
    for sub in &["sub_a", "sub_b"] {
        let subdir = dir.path().join(sub);
        fs::create_dir_all(&subdir).unwrap();
        // lint_warn_only content: unused-variable warning → appears in JSON output.
        fs::copy(fixture("lint_warn_only.mds"), subdir.join("template.mds")).unwrap();
    }

    let out = lint_path(dir.path(), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // At least one file has a warning → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "dir with warn-only files must exit 1; stderr: {stderr}"
    );

    let json: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    let files = json["files"].as_array().expect("must have files[]");
    assert_eq!(
        files.len(),
        2,
        "both files must appear in JSON output; got: {files:?}"
    );

    let paths: Vec<&str> = files
        .iter()
        .map(|f| {
            f["file"]
                .as_str()
                .expect("each entry must have a file string")
        })
        .collect();

    // Each path must contain its subdirectory prefix — not just "template.mds".
    assert!(
        paths.iter().any(|p| p.contains("sub_a")),
        "sub_a path must appear in file keys; got: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("sub_b")),
        "sub_b path must appear in file keys; got: {paths:?}"
    );

    // The two entries must be distinct (not both "template.mds").
    let mut sorted = paths.clone();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        2,
        "file keys must be distinct; got: {paths:?}"
    );
}

// ── Test (b): --fix --format json dir-mode residuals keyed by relative path ──
//
// Pins that after --fix in directory mode, residual diagnostics in the JSON output
// are keyed by the relative display path, NOT by "input.mds" or the raw basename.
//
// Fixture: a file with duplicate-export (Tier A, auto-fixed) + unused-variable
// (Tier C, not auto-fixed). After --fix, the residual unused-variable diagnostic
// must appear under the relative path key, not "input.mds".

#[test]
fn dir_fix_json_residuals_keyed_by_relative_path_not_input_mds() {
    let dir = tempfile::tempdir().unwrap();
    // Put the fixture one level deep so display_path = "subdir/mixed.mds".
    let subdir = dir.path().join("subdir");
    fs::create_dir_all(&subdir).unwrap();

    // Construct a file that has both duplicate-export (fixable) and unused-variable
    // (residual after fix): reuse lint_error.mds (duplicate-export) content plus the
    // unused frontmatter key from lint_warn_only.mds.
    let mixed = "---\ngreeting: Hello\nunused_key: not referenced\n---\n\n\
                  @define greet(name):\n  Hello {{name}}!\n@end\n\n\
                  @export greet\n@export greet\n";
    let target = subdir.join("mixed.mds");
    fs::write(&target, mixed).unwrap();

    let out = lint_path(dir.path(), &["--fix", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout must be valid JSON; err: {e}; stdout: {stdout}; stderr: {stderr}")
    });

    // After fixing duplicate-export, unused-variable residual remains → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "residual unused-variable must produce exit 1; stderr: {stderr}"
    );

    let files = json["files"].as_array().expect("must have files[]");
    assert!(!files.is_empty(), "files[] must be non-empty after fix");

    // Every file key must be the relative path, not "input.mds".
    for entry in files {
        let file_key = entry["file"].as_str().unwrap_or("");
        assert!(
            !file_key.contains("input.mds"),
            "file key must NOT be 'input.mds'; got: {file_key}"
        );
        assert!(
            file_key.contains("subdir") || file_key.contains("mixed"),
            "file key must reference the actual file; got: {file_key}"
        );
    }
}

// ── Test (b2): --fix --format json single-file residuals keyed by basename ────
//
// Pins that after --fix in SINGLE-FILE mode, residual diagnostics in the JSON
// output are keyed by the file's basename, NOT by "input.mds".
//
// The plan_and_apply_fixes reverify closure calls lint_str_with, which sets
// diag.file to STRING_SOURCE_MAP_LABEL ("input.mds").  Without set_diag_display_path
// in the single-file Fixed/PartiallyFixed arms, `mds lint --fix --format json <file>`
// emitted "input.mds" instead of the real basename.  Directory mode already had the
// correct relabel (lint.rs:1176/1197); this test pins the single-file parity.
//
// Fixture: a file with duplicate-export (Tier A, auto-fixed) + unused-variable
// (Tier C, residual after fix).  After --fix, the residual must appear under
// the actual filename, not "input.mds".

#[test]
fn file_fix_json_residuals_keyed_by_filename_not_input_mds() {
    let dir = tempfile::tempdir().unwrap();
    // Use the same fixture shape as dir_fix_json_residuals_keyed_by_relative_path_not_input_mds:
    // duplicate-export (fixable) + unused-variable (residual after fix).
    let mixed = "---\ngreeting: Hello\nunused_key: not referenced\n---\n\n\
                  @define greet(name):\n  Hello {{name}}!\n@end\n\n\
                  @export greet\n@export greet\n";
    let target = dir.path().join("mixed.mds");
    fs::write(&target, mixed).unwrap();

    let out = lint_path(&target, &["--fix", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout must be valid JSON; err: {e}; stdout: {stdout}; stderr: {stderr}")
    });

    // After fixing duplicate-export, unused-variable residual remains → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "residual unused-variable must produce exit 1; stderr: {stderr}"
    );

    let files = json["files"].as_array().expect("must have files[]");
    assert!(!files.is_empty(), "files[] must be non-empty after fix");

    // Every file key must be the real basename, not "input.mds".
    for entry in files {
        let file_key = entry["file"].as_str().unwrap_or("");
        assert!(
            !file_key.contains("input.mds"),
            "file key must NOT be 'input.mds'; got: {file_key}"
        );
        assert_eq!(
            file_key, "mixed.mds",
            "file key must be the real filename basename; got: {file_key}"
        );
    }
}

// ── Test (c): --fix --check on overlap-fix fixture → "Would fix" after coalescing ──
//
// Pins bug-5 / PF-004 fix for the check path: preview_fixes returns a
// PreviewOutcome::WouldFix so --fix --check reports what would change.
//
// Fixture: lint_overlap_fix.mds — @if "x" == "x": with "hello" then-body and an
// empty @else body. Two rules fire simultaneously:
//   • unreachable-branch (Tier A, Error): always-true @if with later @else branch.
//     Generates two spans: remove @if line + remove @else..@end (inclusive).
//   • empty-block (Tier A, Warn): empty @else body.
//     Generates one span: remove @else: through @end (exclusive).
//
// The empty-block span is contained within the unreachable-branch span that covers
// @else..@end. plan_fixes_with_options sorts by (start ASC, end DESC) and calls
// dedup_contained_or_identical, which drops the shorter empty-block span. The
// resulting two unreachable-branch spans are disjoint → no overlap_rejected.
// The fix would succeed → "Would fix" printed; file unchanged (check mode).

#[test]
fn fix_check_coalesced_fix_prints_would_fix() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_overlap_fix.mds");
    fs::copy(fixture("lint_overlap_fix.mds"), &target).unwrap();
    let original = fs::read_to_string(&target).unwrap();

    let out = lint_path(&target, &["--fix", "--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // "Would fix" must appear: after containment dedup the fix is accepted.
    assert!(
        stderr.contains("Would fix"),
        "--fix --check must print 'Would fix' when coalescing resolves the overlap; \
         got stderr: {stderr}"
    );

    // "fix rejected" must NOT appear: containment dedup resolved the overlap.
    assert!(
        !stderr.contains("fix rejected"),
        "--fix --check must NOT print 'fix rejected' after coalescing; got stderr: {stderr}"
    );

    // File must be untouched — check mode never writes.
    let after = fs::read_to_string(&target).unwrap();
    assert_eq!(
        original, after,
        "--fix --check must never write to the file"
    );
}

// ── Test (d): --fix --check fixable fixture → "Would fix", exit 1, no write ──
//
// Pins the positive case: when a fix WOULD succeed, --fix --check prints "Would fix",
// exits 1, and leaves the file unchanged on disk.

#[test]
fn fix_check_fixable_prints_would_fix_and_exits_1_file_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_error.mds");
    fs::copy(fixture("lint_error.mds"), &target).unwrap();
    let original = fs::read_to_string(&target).unwrap();

    let out = lint_path(&target, &["--fix", "--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // "Would fix" must appear.
    assert!(
        stderr.contains("Would fix"),
        "--fix --check must print 'Would fix' for a fixable file; got stderr: {stderr}"
    );

    // exit 1 (fix pending).
    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check must exit 1 when a fix is pending; got stderr: {stderr}"
    );

    // File must be unchanged on disk.
    let after = fs::read_to_string(&target).unwrap();
    assert_eq!(
        original, after,
        "--fix --check must never write to the file"
    );
}

// ── Test (e): Directory --fix --check exits 1 iff any file would change ──────
//
// Pins the directory-mode any_would_fix accumulation (--fix --check exits 1 after
// processing all files when at least one would be modified, exit 0 when none).

#[test]
fn dir_fix_check_exits_1_when_any_file_fixable_exits_0_when_none() {
    // Case 1: directory with one fixable file → exit 1.
    let dir1 = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_error.mds"), dir1.path().join("a.mds")).unwrap();
    fs::copy(fixture("lint_clean.mds"), dir1.path().join("b.mds")).unwrap();

    let out1 = lint_path(dir1.path(), &["--fix", "--check"]);
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    assert_eq!(
        out1.status.code(),
        Some(1),
        "dir with fixable file must exit 1 under --fix --check; stderr: {stderr1}"
    );
    // Neither file must have been modified.
    assert_eq!(
        fs::read_to_string(dir1.path().join("a.mds")).unwrap(),
        fs::read_to_string(fixture("lint_error.mds")).unwrap(),
        "fixable file must not be written by --fix --check"
    );

    // Case 2: directory with only clean files → exit 0.
    let dir2 = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir2.path().join("c.mds")).unwrap();

    let out2 = lint_path(dir2.path(), &["--fix", "--check"]);
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert_eq!(
        out2.status.code(),
        Some(0),
        "dir with only clean files must exit 0 under --fix --check; stderr: {stderr2}"
    );
}

// ── Test (f): Overlap fixture → coalescing resolves it, fix succeeds ─────────
//
// Pins Step 4 coalescing: when multiple Tier-A rules emit overlapping spans
// (one span contained within another), dedup_contained_or_identical drops
// the smaller span and the fix proceeds with the larger (dominant) span.
//
// Fixture: lint_overlap.mds — a @define containing @if "a"=="a" with a
// @elseif "a"=="a" that is both a duplicate AND has an empty body.  Three
// rules emit edits on the @elseif line:
//   • unreachable-branch case A (always-true @if): two spans — remove @if
//     directive line + remove @elseif..@end inclusive.
//   • unreachable-branch case G (duplicate @elseif): one span —
//     remove @elseif..@end exclusive (contained within case-A span 2).
//   • empty-block ⑥ (empty @elseif body): one span — same as case-G
//     span (contained/identical).
// After sort (start ASC, end DESC) and containment dedup, only two spans
// survive: [remove-@if-line, remove-@elseif..@end-inclusive].
// No overlap remains → fix is applied → file changed → exit 0.

#[test]
fn fix_overlap_coalesced_and_applied() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_overlap.mds");
    fs::copy(fixture("lint_overlap.mds"), &target).unwrap();
    let original = fs::read_to_string(&target).unwrap();

    let out = lint_path(&target, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Fix must succeed: coalescing resolves the contained-span overlap.
    assert!(
        !stderr.contains("fix rejected"),
        "--fix on coalesced overlap fixture must NOT print 'fix rejected'; got stderr: {stderr}"
    );

    // File must be changed.
    let after = fs::read_to_string(&target).unwrap();
    assert_ne!(
        original, after,
        "fix must change the file when coalescing resolves the overlap"
    );

    // The @if and @elseif blocks are gone; the then-body and outer @end remain.
    assert!(
        !after.contains("@if"),
        "fixed file must not contain the removed @if; got:\n{after}"
    );
    assert!(
        !after.contains("@elseif"),
        "fixed file must not contain the removed @elseif; got:\n{after}"
    );

    // No residual diagnostics → exit 0.
    assert_eq!(
        out.status.code(),
        Some(0),
        "no residual diagnostics after fix; got stderr: {stderr}"
    );
}

// ── Test (g): PartiallyFixed end-to-end: applied count in summary ─────────────
//
// Pins the PartiallyFixed outcome: when some edits pass the reverify gate and
// some fail, the CLI writes the partially-fixed file and emits a
// "{applied} of {total} fixes applied" summary.
//
// Fixture: lint_partial_fix.mds — a multi-line empty @define (Tier A, empty-block)
// with a matching @export, plus a duplicate @export (Tier A, duplicate-export).
//
// The empty-block fix removes @define empty_fn():..@end. This leaves @export empty_fn
// in the file with no corresponding @define. The MDS compiler rejects this
// ("cannot export 'empty_fn': not defined in this module"), so the reverify gate
// refuses the empty-block fix fail-closed. The duplicate-export fix (remove second
// @export greet) passes independently.
//
// Expected behaviour (per-edit fallback, right-to-left):
//   1. duplicate-export (higher offset) → applied, reverify passes → Fixed.
//   2. empty-block (lower offset) → reverify fails (orphaned @export) → rejected.
// File written with one @export greet remaining; empty @define still present.
// Stderr: "1 of 2 fixes applied" (or "Partially fixed: … (1 of 2 fixes applied)").

#[test]
fn partially_fixed_end_to_end_count_in_summary() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_partial_fix.mds");
    fs::copy(fixture("lint_partial_fix.mds"), &target).unwrap();

    let out = lint_path(&target, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Stderr must contain the count summary.
    assert!(
        stderr.contains("1 of 2"),
        "stderr must contain '1 of 2 fixes applied' summary; got: {stderr}"
    );

    let after = fs::read_to_string(&target).unwrap();

    // The duplicate export must have been removed (duplicate-export fix was applied).
    assert_eq!(
        after.matches("@export greet").count(),
        1,
        "file must have exactly one @export greet after partial fix; got:\n{after}"
    );

    // The empty @define must still be present (empty-block fix refused: orphaned @export).
    assert!(
        after.contains("@define empty_fn():"),
        "empty @define must still be present after partial fix; got:\n{after}"
    );

    // Residual diagnostics remain (empty-block Warn) → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "residual empty-block Warn must produce exit 1; got stderr: {stderr}"
    );
}

// ── Test (h): Stdin lint with diagnostic includes code frame ─────────────────
//
// Pins bug-19 fix: when lint runs in stdin (report-only) mode and emits a
// human diagnostic, the named source "input.mds" + source text must be attached
// so miette renders the annotated source context (code frame with caret underline).
//
// Pre-Phase-B behavior: named_source was None for stdin report-only mode, so no
// source context was rendered — diagnostics lacked the code frame entirely.

#[test]
fn stdin_lint_diagnostic_includes_code_frame() {
    let source = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    let out = lint_stdin(source, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // "duplicate-export" diagnostic must appear.
    assert!(
        stderr.contains("duplicate-export"),
        "diagnostic must appear in stdin report-only mode; got: {stderr}"
    );

    // "<stdin>" must appear: AD-211-1 relabels the span header from the internal
    // STRING_SOURCE_MAP_LABEL ("input.mds") to the uniform CLI sentinel.
    assert!(
        stderr.contains("<stdin>"),
        "stdin mode must show '<stdin>' in the code frame; got: {stderr}"
    );

    // PF-013 negative half: the internal VFS key must NOT appear in the human output.
    // Symmetric with the JSON sibling (stdin_json_wire_file_key_is_stdin_sentinel) which
    // also asserts absence of "input.mds" alongside the presence assertion for "<stdin>".
    assert!(
        !stderr.contains("input.mds"),
        "stdin human output must not expose the internal VFS key 'input.mds'; got: {stderr}"
    );

    // At least one token from the source must appear in the code frame context.
    // miette renders the offending line; "@export greet" is on that line.
    assert!(
        stderr.contains("@export"),
        "code frame must include the offending source line '@export greet'; got: {stderr}"
    );
}

// ── AC-P1-01: stdin JSON wire `files[].file` emits "<stdin>" ─────────────────
//
// Pins issue #211: every stdin lint path must emit `"<stdin>"` in the JSON
// `files[].file` key, not the internal VFS sentinel `"input.mds"`.
//
// Covers: run_lint_stdin report-only mode with --format json.

#[test]
fn stdin_json_wire_file_key_is_stdin_sentinel() {
    // Source with a known lint finding that needs no file imports so lint_str_with
    // succeeds and emits a regular diagnostic JSON (not an analysis-failure envelope).
    // duplicate-export fires without any resolver look-ups.
    let source = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    let out = lint_stdin(source, &["--format", "json"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be valid JSON");

    let files = v["files"].as_array().expect("JSON must have 'files' array");
    assert!(
        !files.is_empty(),
        "stdin JSON must have at least one file group (duplicate-export fires); got: {stdout}"
    );
    for entry in files {
        let file_key = entry["file"]
            .as_str()
            .expect("each file entry must have a 'file' string");
        assert_eq!(
            file_key, "<stdin>",
            "AC-P1-01: JSON files[].file must be '<stdin>' for stdin input, not '{file_key}'"
        );
    }
    // AC-P1-01/AC-P1-27, negative half: the internal VFS key must not leak anywhere
    // in the document, not just in the key this test read.
    assert!(
        !stdout.contains("input.mds"),
        "AC-P1-27: 'input.mds' must not appear anywhere in CLI stdout for a stdin \
         lint; got: {stdout}"
    );
}

// ── AC-P1-08/#202: JSON wire diagnostics sorted by byte offset ───────────────
//
// Pins issue #202: within a file group, diagnostics must appear in ascending
// byte-offset order regardless of the order the rules were applied in.

/// Fixture whose OFFSET order is the reverse of its RULE-EXECUTION order.
///
/// `run_rules` (crates/mds-core/src/lint/mod.rs) dispatches `duplicate_export`
/// fifth and `legacy_interpolation` tenth — so without a sort, `duplicate-export`
/// (the LATER offset) is emitted first. `{name}` on line 2 is a legacy
/// single-brace interpolation at a low offset; the repeated `@export greet` at the
/// end is a duplicate export at a high offset.
///
/// A fixture whose diagnostics are already ascending cannot detect the sort being
/// removed — the assertion would hold either way.
const OUT_OF_ORDER_FIXTURE: &str =
    "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n";

fn stdin_json_diagnostics(source: &str) -> Vec<serde_json::Value> {
    let out = lint_stdin(source, &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be JSON: {e}\n{stdout}"));
    let files = v["files"].as_array().expect("JSON must have 'files' array");
    assert_eq!(
        files.len(),
        1,
        "stdin lint must emit exactly one file group"
    );
    files[0]["diagnostics"]
        .as_array()
        .expect("must have 'diagnostics'")
        .clone()
}

#[test]
fn stdin_json_diagnostics_sorted_by_offset() {
    let diags = stdin_json_diagnostics(OUT_OF_ORDER_FIXTURE);

    let rules: Vec<&str> = diags.iter().filter_map(|d| d["rule"].as_str()).collect();
    let offsets: Vec<i64> = diags
        .iter()
        .filter_map(|d| d["span"]["offset"].as_i64())
        .collect();

    // Non-vacuity guard: this assertion is meaningless on a single diagnostic, and
    // meaningless if the two land on the same offset.
    assert_eq!(
        offsets.len(),
        diags.len(),
        "every diagnostic in this fixture must carry a span; got: {diags:?}"
    );
    assert!(
        offsets.len() >= 2 && offsets[0] != offsets[offsets.len() - 1],
        "AC-P1-08 needs at least two diagnostics at DISTINCT offsets to be a real \
         check; got rules {rules:?} at offsets {offsets:?}"
    );

    let mut sorted = offsets.clone();
    sorted.sort_unstable();
    assert_eq!(
        offsets, sorted,
        "AC-P1-08: diagnostics must be in ascending byte-offset order; \
         got rules {rules:?} at offsets {offsets:?}"
    );

    // The positive control: `duplicate_export` runs BEFORE `legacy_interpolation`
    // in run_rules but fires at the LATER offset, so it must appear LATER in the
    // array. Delete the sort in LintResultBuilder::build and this flips.
    let legacy = rules
        .iter()
        .position(|r| *r == "legacy-interpolation")
        .expect("fixture must produce a legacy-interpolation diagnostic");
    let dup = rules
        .iter()
        .position(|r| *r == "duplicate-export")
        .expect("fixture must produce a duplicate-export diagnostic");
    assert!(
        legacy < dup,
        "AC-P1-08: emitted order must follow byte offset, not rule-dispatch order \
         (duplicate_export is dispatched first but fires later in the file); \
         got rules {rules:?} at offsets {offsets:?}"
    );
}

/// AC-P1-09: the human renderer must present diagnostics in the same order as the
/// JSON renderer — proving the sort lives on `LintResult.diagnostics` and not in
/// one renderer (PF-007: a per-surface assertion could not show this).
#[test]
fn stdin_human_and_json_diagnostic_order_match() {
    let json_rules: Vec<String> = stdin_json_diagnostics(OUT_OF_ORDER_FIXTURE)
        .iter()
        .filter_map(|d| d["rule"].as_str().map(str::to_string))
        .collect();
    // Non-vacuity guard: two empty sequences compare equal and prove nothing.
    assert!(
        json_rules.len() >= 2,
        "AC-P1-09 needs at least two diagnostics to compare an ORDER; got {json_rules:?}"
    );

    let out = lint_stdin(OUT_OF_ORDER_FIXTURE, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Rule names appear in the miette `code` line of each rendered diagnostic.
    let human_rules: Vec<String> = stderr
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            json_rules
                .iter()
                .find(|r| t == format!("mds::lint::{r}") || t == **r)
                .cloned()
        })
        .collect();

    assert_eq!(
        human_rules, json_rules,
        "AC-P1-09: human and JSON surfaces must agree on diagnostic order.\n\
         json: {json_rules:?}\nhuman: {human_rules:?}\nstderr:\n{stderr}"
    );
}

/// AC-P1-11: repeated lints of identical input are byte-identical.
#[test]
fn stdin_json_output_is_byte_identical_across_runs() {
    let first = lint_stdin(OUT_OF_ORDER_FIXTURE, &["--format", "json"]).stdout;
    for run in 2..=5 {
        let next = lint_stdin(OUT_OF_ORDER_FIXTURE, &["--format", "json"]).stdout;
        assert_eq!(
            first,
            next,
            "AC-P1-11: run {run} differed from run 1.\nrun1: {}\nrun{run}: {}",
            String::from_utf8_lossy(&first),
            String::from_utf8_lossy(&next)
        );
    }
}

// ── AC-P1-07 / AD-211-5: the analysis-failure envelope labels stdin `<stdin>` ──

/// Pull the source identity out of a miette code-frame header, e.g. the
/// `<stdin>` in `[<stdin>:1:1]`.
///
/// Returning `Option` and requiring the caller to unwrap keeps this from passing
/// vacuously: a rendering that emitted no frame at all yields `None` and fails,
/// rather than silently satisfying a "does not contain `<source>`" assertion
/// (PF-013).
fn frame_source_identity(rendered: &str) -> Option<String> {
    let start = rendered.find('[')?;
    let rest = &rendered[start + 1..];
    let end = rest.find(']')?;
    let inner = &rest[..end];
    // `<label>:LINE:COL` — strip the trailing two numeric fields.
    let mut parts: Vec<&str> = inner.rsplitn(3, ':').collect();
    parts.reverse();
    (parts.len() == 3 && parts[1].chars().all(|c| c.is_ascii_digit())).then(|| parts[0].to_string())
}

#[test]
fn stdin_analysis_failure_labels_source_as_stdin() {
    // A source that fails the check gate: `@if` without a condition is a hard
    // syntax error, so lint_str_with returns Err and takes the
    // emit_analysis_failure_json_or_stderr path.
    let source = "@if\nbroken\n";

    // Human channel: the miette frame header must name <stdin>.
    let human = lint_stdin(source, &[]);
    let stderr = String::from_utf8_lossy(&human.stderr);
    let identity = frame_source_identity(&stderr).unwrap_or_else(|| {
        panic!("expected a miette code frame with a `[label:line:col]` header; got:\n{stderr}")
    });
    assert_eq!(
        identity, "<stdin>",
        "AC-P1-07: the analysis-failure frame must name '<stdin>'; got '{identity}'.\n{stderr}"
    );
    assert_eq!(
        human.status.code(),
        Some(2),
        "an analysis failure must still exit 2; stderr:\n{stderr}"
    );

    // JSON channel: the analysis-failure envelope is `{"version":1,"error":{...}}`.
    //
    // The `error` object carries `code`, `message`, `help`, and `span` — there is
    // NO top-level `file` key and NO `file` key inside `error`.  A JSON consumer
    // MUST NOT look for `files[].file` in this envelope; it is only present on
    // the success envelope (`{"version":1,"files":[...],"truncated":...}`).
    // This contract is documented in the CHANGELOG "Lint JSON wire contract" block
    // (AC-P1-07 / AD-211-5) and pinned here so it cannot regress silently.
    let json = lint_stdin(source, &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&json.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("analysis-failure JSON must parse");
    assert!(
        v["error"].is_object(),
        "JSON analysis failure must use the error envelope; got: {stdout}"
    );
    // AC-P1-07 / AD-211-5: the error envelope has no `file` key at the top level —
    // a stdin source identity never appears here.  Asserting null explicitly so a
    // regression that adds `"file": "<stdin>"` to this envelope is caught.
    assert!(
        v.get("file").is_none() || v["file"].is_null(),
        "AC-P1-07: analysis-failure JSON envelope must have NO top-level 'file' key; \
         got: {stdout}"
    );
    // Also confirm `files` (the success envelope's array) is absent — both keys
    // would indicate a confused merge of the two envelope shapes.
    assert!(
        v.get("files").is_none() || v["files"].is_null(),
        "AC-P1-07: analysis-failure JSON envelope must have NO 'files' array; got: {stdout}"
    );
    let message = v["error"]["message"]
        .as_str()
        .expect("error.message must be a string");

    // Non-vacuity guard: the message must be non-empty so the absence checks below
    // are not trivially satisfied by an empty string.
    assert!(
        !message.is_empty(),
        "AC-P1-07: error.message must not be empty"
    );

    // ADR-009 / PF-013 positive controls: apply the SAME extraction pipeline
    // (`serde_json::from_str` → `v["error"]["message"].as_str()` → `.contains()`)
    // to a SYNTHETIC JSON document that embeds each forbidden value.
    //
    // **Why synthetic rather than a binary built at 113f472?**  Spawning a
    // historical CLI binary inside a cargo test is impractical — it requires
    // a pre-built artifact committed to the repo or a build-time download,
    // neither of which is feasible here.  A synthetic envelope is the
    // next-best option: it validates that the serde path (`v["error"]["message"]
    // .as_str()`) can detect the forbidden values when they ARE present,
    // which is exactly the failure mode a regression would introduce (the key
    // moves or the value is `null`).  The tradeoff is that the control cannot
    // prove the live CLI would emit the forbidden value if the fix regressed.
    //
    // **Near-vacuous JSON-channel absence assertions:** analysis (2026-08-14)
    // shows that no `MdsError` Display template interpolates `ctx.file_str` (the
    // resolved file path) into the rendered message, so `"<source>"` structurally
    // cannot reach `error.message` through any current code path.  The absence
    // checks are therefore largely structural rather than behavioral; the
    // non-vacuous coverage rests on the human-channel `frame_source_identity`
    // assertion below, which is genuinely sound because it exercises the miette
    // rendering path end-to-end.
    //
    // A tautology over a string literal — e.g. `"<source>".contains("<source>")` —
    // proves only that `str::contains` is implemented; it never exercises the serde
    // deserialization step, so it would still pass even if `v["error"]["message"]`
    // silently returned `None` (e.g. because `MdsError::serialize` moved the value
    // to a different key).  These controls catch that narrower regression.
    {
        let ctrl_stdout = r#"{"version":1,"error":{"code":"parse_error","message":"<source>:1:1","help":null,"span":null}}"#;
        let ctrl_v: serde_json::Value = serde_json::from_str(ctrl_stdout).unwrap();
        let ctrl_msg = ctrl_v["error"]["message"]
            .as_str()
            .expect("control: synthetic envelope must yield a message string");
        assert!(
            ctrl_msg.contains("<source>"),
            "control (ADR-009/PF-013): extraction path v[\"error\"][\"message\"].as_str() \
             detects '<source>' in error.message — if this fails the real check below is broken"
        );
        assert!(
            ctrl_stdout.contains("<source>"),
            "control (ADR-009/PF-013): raw stdout scan detects '<source>' — \
             if this fails the stdout scan below is broken"
        );
    }
    {
        let ctrl_stdout = r#"{"version":1,"error":{"code":"io_error","message":"failed to read input.mds","help":null,"span":null}}"#;
        let ctrl_v: serde_json::Value = serde_json::from_str(ctrl_stdout).unwrap();
        let ctrl_msg = ctrl_v["error"]["message"]
            .as_str()
            .expect("control: synthetic envelope must yield a message string");
        assert!(
            ctrl_msg.contains("input.mds"),
            "control (ADR-009/PF-013): extraction path v[\"error\"][\"message\"].as_str() \
             detects 'input.mds' in error.message — if this fails the real check below is broken"
        );
        assert!(
            ctrl_stdout.contains("input.mds"),
            "control (ADR-009/PF-013): raw stdout scan detects 'input.mds' — \
             if this fails the stdout scan below is broken"
        );
    }

    assert!(
        !message.contains("<source>"),
        "AC-P1-07: error.message must not embed '<source>'; got: {message}"
    );
    assert!(
        !message.contains("input.mds"),
        "AC-P1-07: error.message must not embed 'input.mds'; got: {message}"
    );
    assert!(
        !stdout.contains("<source>"),
        "AC-P1-27: '<source>' must not appear in stdout; got: {stdout}"
    );
    assert!(
        !stdout.contains("input.mds"),
        "AC-P1-27: 'input.mds' must not appear in stdout; got: {stdout}"
    );
}

/// AC-P1-04: `lint`, `check`, `build`, and `fmt` all name a stdin source with the
/// SAME sentinel per the §0a ruling.
///
/// This test has three parts because the subcommands have different output shapes:
///
/// **Error-frame part** (`lint`, `check`, `build`): a broken source triggers a
/// miette code frame and `frame_source_identity` extracts the label.  `mds build -`
/// reaches the check gate at `build.rs` (`run_check` → `check_stdin`), not the
/// compile-to-content stdin site; the assertion still holds — it is the same
/// AD-211-5 relabel that applies on that path.
///
/// **Status-line part** (`fmt --check`): a valid but reformattable source causes
/// `mds fmt --check -` to print `"Would reformat: <stdin>"` — a different output
/// shape, covered separately after the loop.
///
/// **Success-line part** (`check`): a valid source causes `mds check -` to print
/// `"OK: <stdin>"`.  This is the third distinct output shape: the
/// `output::STDIN_DISPLAY_LABEL` constant at `main.rs` is shared with
/// `fmt.rs`; a regression pointing the constant at the wrong value must be
/// caught here, not by the print-discipline allowlist (which pins the sanitizer
/// exemption, not the emitted text).
///
/// Before the relabels landed, `mds check -` and `mds build -` rendered
/// `[<source>:1:1]` and `mds lint -` rendered `[input.mds:1:1]`.  `mds fmt -`
/// already used `<stdin>` (AD-211-3 extended the same constant).
#[test]
fn stdin_source_identity_is_uniform_across_subcommands() {
    // Part 1 — error path: a parse-failing source forces a miette frame.
    // All three subcommands below reach their respective analysis-failure paths,
    // each of which applies the AD-211-5 / AD-211-3 <stdin> relabel.
    let broken = "@if\nbroken\n";
    for subcommand in ["lint", "check", "build"] {
        let out = run_mds_stdin(subcommand, broken);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let identity = frame_source_identity(&stderr).unwrap_or_else(|| {
            panic!("`mds {subcommand} -` must render a code frame; got:\n{stderr}")
        });
        assert_eq!(
            identity, "<stdin>",
            "AC-P1-04: `mds {subcommand} -` must name the source '<stdin>'; \
             got '{identity}'.\n{stderr}"
        );
        assert!(
            !stderr.contains("<source>"),
            "AC-P1-04: `mds {subcommand} -` must not emit '<source>'; got:\n{stderr}"
        );
    }

    // Part 2 — fmt status line: `mds fmt --check -` emits "Would reformat: <stdin>"
    // when a source needs reformatting (not a code frame).  A directive with
    // trailing whitespace triggers the formatter's R4 strip rule.
    {
        use std::io::Write;
        // Three trailing spaces after the @define directive are stripped by the
        // formatter (R4: directive trailing-whitespace strip), so --check reports
        // that a reformat would occur.
        let fmt_source = "@define greet(name):   \nHello {{name}}!\n@end\n";
        let mut child = mds_bin()
            .arg("fmt")
            .arg("--check")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(fmt_source.as_bytes());
        let out = child.wait_with_output().unwrap();
        let fmt_stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            fmt_stderr.contains("Would reformat: <stdin>"),
            "AC-P1-04: `mds fmt --check -` must emit 'Would reformat: <stdin>'; \
             got:\n{fmt_stderr}"
        );
        assert!(
            !fmt_stderr.contains("input.mds") && !fmt_stderr.contains("<source>"),
            "AC-P1-04: `mds fmt --check -` must not emit old source labels; \
             got:\n{fmt_stderr}"
        );
    }

    // Part 3 — check success line: `mds check -` emits "OK: <stdin>" when stdin
    // passes validation (AD-211-3 / AC-P1-04).  This pins the `main.rs`
    // `output::STDIN_DISPLAY_LABEL` usage at the `OK:` status line; a regression
    // that swapped the constant back to a bare literal would be caught here.
    {
        // Plain valid MDS source — no syntax errors, no undefined variables.
        let valid_source = "Hello!\n";
        let out = run_mds_stdin("check", valid_source);
        let check_stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "AC-P1-04: `mds check -` must succeed for valid stdin; \
             got:\n{check_stderr}"
        );
        assert!(
            check_stderr.contains("OK: <stdin>"),
            "AC-P1-04: `mds check -` must emit 'OK: <stdin>' on success; \
             got:\n{check_stderr}"
        );
        assert!(
            !check_stderr.contains("input.mds") && !check_stderr.contains("<source>"),
            "AC-P1-04: `mds check -` must not emit old source labels; \
             got:\n{check_stderr}"
        );
    }
}

// ── PF-012 guard: imported-file errors must not be relabelled as <stdin> ───────

/// Pins the conditional guard inside `relabel_stdin_error` (output.rs).
///
/// The guard — `e.is_string_source()` — prevents relabelling errors whose
/// embedded source belongs to an IMPORTED file.  When
/// stdin imports a file that fails to parse (broken lib.mds), the `MdsError`
/// carries the imported file's real path as its source name.  Because that path
/// is not the sentinel `"<source>"`, the guard is false and `StdinRelabeledError`
/// is constructed with `source: None`, which makes `source_code()` delegate to
/// the inner error — preserving lib.mds's path in the miette code frame.
///
/// **Why the existing tests do not cover this path:** every prior stdin test
/// passes an import-free broken source (`@if\nbroken\n`) so the error always
/// originates in the stdin source itself and always carries `"<source>"` — the
/// guard is always true and the mutation of removing it does not change the
/// observable output.
///
/// **Proven necessary by mutation:** removing the guard (making the relabel
/// unconditional) lets all 681 prior tests pass while silently mis-anchoring
/// the miette caret.  The relabelled report uses stdin text as source content
/// but the span is lib.mds's byte offset — exactly the PF-012
/// in-bounds-but-wrong-source class the guard was added to prevent.
///
/// PF-013 positive control: asserts that "lib.mds" IS in the frame identity
/// before asserting that "<stdin>" is NOT, so neither assertion can pass
/// vacuously.
#[test]
fn stdin_import_error_frame_names_imported_file_not_stdin() {
    use std::io::Write;

    // A syntactically broken imported file: @if without a condition is a hard
    // parse error.  The resolver embeds lib.mds's canonical path in the error's
    // NamedSource (MdsError::Syntax { src: Some(NamedSource::new(key, …)) }),
    // so source_name() returns the real path, not "<source>", and the guard is
    // false.  attach_import_span passes MdsError::Syntax through unchanged, so
    // the source identity is preserved end-to-end.
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib.mds");
    fs::write(&lib, "@if\n").unwrap();

    // The stdin source itself is syntactically valid (@import is well-formed).
    // The analysis failure originates entirely inside lib.mds, not in stdin.
    let stdin_src = "@import { x } from \"./lib.mds\"\n";

    for subcommand in ["lint", "check"] {
        let mut child = mds_bin()
            .arg(subcommand)
            .arg("-")
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Ignore BrokenPipe — process may exit before reading stdin.
        let _ = child.stdin.take().unwrap().write_all(stdin_src.as_bytes());
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);

        // Non-vacuity: a miette code frame must be rendered at all.
        let identity = frame_source_identity(&stderr).unwrap_or_else(|| {
            panic!(
                "PF-012: `mds {subcommand} -` (stdin importing broken lib.mds) must \
                 render a miette code frame; got:\n{stderr}"
            )
        });

        // PF-013 positive control: the frame must identify lib.mds so the
        // absence check below cannot pass vacuously on an empty or wrong identity.
        assert!(
            identity.contains("lib.mds"),
            "PF-012: `mds {subcommand} -` frame must name 'lib.mds', not '<stdin>'; \
             got '{identity}'.\n{stderr}"
        );

        // Guard assertion: imported-file errors must NOT be relabelled to <stdin>.
        // If the guard in relabel_stdin_error is removed, the frame shows
        // "<stdin>" instead of the real lib.mds path — this assertion catches that.
        assert_ne!(
            identity, "<stdin>",
            "PF-012: `mds {subcommand} -` must not relabel an imported-file error \
             as '<stdin>'; the relabel_stdin_error guard is broken.\n{stderr}"
        );
    }
}

// ── AC-P1-10: `files[]` array ordering is part of the wire contract ──────────

/// Directory mode path-sorts `files[]` by byte-wise (lexicographic) string order
/// on the relative display path; single-file and stdin emit exactly one entry.
/// Both halves are frozen here because a published contract has to pin the order
/// of the outer array too, not only the diagnostics inside each entry.
///
/// **Non-vacuity (PF-013):** the fixture intentionally includes `api-utils.mds`
/// (flat) alongside `api/x.mds` (in a subdirectory).  Under `Path::Ord`
/// (component-wise), `api/x.mds` sorts before `api-utils.mds` because the first
/// component `"api"` is shorter than `"api-utils.mds"`.  Under byte-wise string
/// order, `api-utils.mds` sorts before `api/x.mds` because `'-'` (0x2D) < `'/'`
/// (0x2F).  The oracle (`sorted.sort_unstable()` on strings) expects byte-wise
/// order, so a regression that reverts to `Path::Ord` produces a different
/// sequence and the assertion fails.
#[test]
fn json_files_array_is_path_sorted_in_directory_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("api")).unwrap();
    // Written in an order that is neither the expected string-sorted order nor
    // the Path::Ord order, so passing cannot be an accident of enumeration order.
    let dupe = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    for rel in ["b.mds", "api/x.mds", "api-utils.mds"] {
        fs::write(dir.path().join(rel), dupe).unwrap();
    }

    let out = lint_path(dir.path(), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    let files: Vec<&str> = v["files"]
        .as_array()
        .expect("JSON must have 'files' array")
        .iter()
        .filter_map(|f| f["file"].as_str())
        .collect();

    assert_eq!(
        files.len(),
        3,
        "all three files must report findings; got: {files:?}"
    );
    // Oracle: byte-wise string sort. Expected: ["api-utils.mds", "api/x.mds", "b.mds"].
    // Under Path::Ord (component-wise) the order would be ["api/x.mds", "api-utils.mds",
    // "b.mds"], so a Path::Ord regression would fail this assertion (PF-013 positive
    // control).
    let mut sorted = files.clone();
    sorted.sort_unstable();
    assert_eq!(
        files, sorted,
        "AC-P1-10: files[] must be in ascending byte-wise path order; got: {files:?}"
    );

    // Single-file mode: exactly one entry.
    let single = lint_path(&dir.path().join("b.mds"), &["--format", "json"]);
    let single_stdout = String::from_utf8_lossy(&single.stdout);
    let sv: serde_json::Value = serde_json::from_str(&single_stdout).unwrap();
    assert_eq!(
        sv["files"].as_array().map(Vec::len),
        Some(1),
        "AC-P1-10: single-file mode must emit exactly one files[] entry; got: {single_stdout}"
    );
}

// ── AC-P1-20: WIRE sanitization of files[].file survives the relabel ─────────

/// POSITIVE CONTROL (PF-013 / ADR-009). This PR rewrites `diag.file` wholesale in
/// stdin mode, and `files[].file` is the exact key that #176 / CWE-150 escaping
/// protects. An assertion that merely finds `<stdin>` proves nothing about the
/// escape contract — `<stdin>` contains no character in the hostile class, so it
/// is a no-op for it.
///
/// So: lint a directory containing a file whose PATH carries a control byte, and
/// assert the emitted key carries the escaped `\uXXXX` literal and no raw byte.
/// The control arm runs the SAME extraction over the raw path and shows it does
/// detect the raw byte when one is present — without it, "no raw byte found"
/// would be indistinguishable from "the check matched nothing".
///
/// The byte is constructed at runtime from a numeric escape and never typed as a
/// literal into this file (PF-018 — the editing tooling decodes such escapes into
/// real bytes in tracked source, and the `Source hygiene` CI job rejects them).
#[cfg(unix)]
#[test]
fn directory_json_file_key_escapes_control_bytes_in_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    const BEL: u8 = 0x07;
    let dir = tempfile::tempdir().unwrap();
    let mut raw = b"be".to_vec();
    raw.push(BEL);
    raw.extend_from_slice(b"l.mds");
    let name = OsString::from_vec(raw.clone());
    let target = dir.path().join(&name);

    let dupe = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    if fs::write(&target, dupe).is_err() {
        // Both CI filesystems (ext4 / APFS) accept 0x07 in filenames, so
        // this branch is unreachable in CI.  Panic rather than silently
        // returning — cargo nextest captures stdout/stderr and only surfaces
        // them on failure, so an eprintln!/return here would be invisible
        // and would never reveal a genuine skip.  Windows has no
        // `OsStringExt` analogue, so there is no #[cfg(windows)] sibling
        // test; the Windows coverage gap is tracked at #147/#148.
        panic!(
            "directory_json_file_key_escapes_control_bytes_in_paths: \
             control-byte filename rejected by filesystem — unexpected on this platform \
             (PF-013 / ADR-009)"
        );
    }

    let out = lint_path(dir.path(), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    /// The extraction under test: does `s` contain a raw C0 control byte?
    fn has_raw_control(s: &str) -> bool {
        s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
    }

    // CONTROL ARM — the same predicate, applied to the unsanitized path, must
    // return true. If this fails, the assertion below is incapable of failing.
    let raw_path = String::from_utf8_lossy(&raw).into_owned();
    assert!(
        has_raw_control(&raw_path),
        "control arm: the predicate must detect the raw byte when it is present, \
         otherwise the assertion below proves nothing (PF-013)"
    );

    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout must be valid JSON");
    let file_key = v["files"]
        .as_array()
        .and_then(|f| f.first())
        .and_then(|f| f["file"].as_str())
        .unwrap_or_else(|| panic!("expected one files[] entry; got: {stdout}"));

    assert!(
        !has_raw_control(file_key),
        "AC-P1-20: files[].file must not carry a raw control byte; got: {file_key:?}"
    );
    assert!(
        file_key.contains("\\u0007"),
        "AC-P1-20: files[].file must carry the escaped literal form of the control \
         byte; got: {file_key:?}"
    );
}

/// Run `mds <subcommand> -` with `input` on stdin.
fn run_mds_stdin(subcommand: &str, input: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = mds_bin()
        .arg(subcommand)
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

// ── AC-P1-03: fix-preview status lines use the bracketed sentinel ────────────

/// The bare `stdin` convention (`Would fix: stdin`) is gone: every fix-path
/// message names the source with the same bracketed sentinel as the diagnostics.
#[test]
fn stdin_fix_preview_messages_use_bracketed_sentinel() {
    // legacy-interpolation is Tier A and auto-fixable on a standalone source.
    // The interpolation sits inside a `@define` body so `name` is a parameter in
    // scope — otherwise the fix-safety gate rejects the rewrite ("undefined
    // variable") and no status line is printed at all.
    let source = "@define greet(name):\n  Hello {name}!\n@end\n";

    // (a) --fix --check → "Would fix: <stdin>", exit 1.
    let out = lint_stdin(source, &["--fix", "--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Would fix: <stdin>"),
        "AC-P1-03: expected 'Would fix: <stdin>'; got:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check must exit 1 when a fix would apply; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Would fix: stdin"),
        "AC-P1-03: the bare `stdin` convention must be gone; got:\n{stderr}"
    );

    // (b) --fix --diff → the unified-diff header names <stdin>.
    let out = lint_stdin(source, &["--fix", "--diff"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("<stdin>"),
        "AC-P1-03: the diff header must reference '<stdin>'; got:\n{stdout}"
    );
    // The diff header lines are `--- <name>` / `+++ <name>`; neither may carry the
    // unbracketed token.
    for line in stdout
        .lines()
        .filter(|l| l.starts_with("---") || l.starts_with("+++"))
    {
        assert!(
            !line.split_whitespace().any(|w| w == "stdin"),
            "AC-P1-03: diff header must not use bare `stdin`; got: {line}"
        );
    }
}

// ── AC-P1-26: zero-diagnostic stdin JSON scope-out ───────────────────────────
//
// When stdin produces no diagnostics the JSON envelope correctly emits
// `files: []` — there is no file entry at all, so no `file` key to check.
// This is intentional and consistent with how directory-mode lint omits
// zero-diagnostic files.  Asserting "files[0].file == <stdin>" would be
// undefined for a clean source; the correct assertion is that `files` is empty
// and that no VFS key or source-identity sentinel leaks into the document.

/// AC-P1-26 zero-diagnostic carve-out: clean stdin produces `files:[]`
/// (no file entry), `truncated:false`, `version:1`.  The `<stdin>` sentinel
/// appears in `files[0].file` only when at least one diagnostic is emitted.
/// Per AC-P1-06 the binding surfaces also produce `files:[]` for clean
/// string-source input, making the JSON identical across CLI and bindings
/// in the zero-diagnostic case.  See CHANGELOG for the documented wire contract.
#[test]
fn stdin_json_clean_source_emits_empty_files_array() {
    let out = lint_stdin("Hello World!\n", &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("clean stdin JSON must parse");
    assert_eq!(
        v["files"],
        serde_json::json!([]),
        "AC-P1-26: clean stdin must emit files:[] (no file entry); got: {stdout}"
    );
    assert_eq!(v["truncated"], false, "truncated must be false");
    assert_eq!(v["version"], 1, "version must be 1");
    // No VFS key or source sentinel may leak even for zero-diagnostic output.
    assert!(
        !stdout.contains("input.mds"),
        "AC-P1-26: 'input.mds' must not appear in clean-stdin JSON; got: {stdout}"
    );
    assert!(
        !stdout.contains("<stdin>"),
        "AC-P1-26: '<stdin>' must not appear in clean-stdin JSON (files is empty); \
         got: {stdout}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean source must exit 0; stdout: {stdout}"
    );
}

// ── AC-P1-03(c): Partially fixed: <stdin> ────────────────────────────────────
//
// The partial-fix write path must emit "Partially fixed: <stdin>" (not
// "Partially fixed: stdin") when some fixes are applied but others are
// rejected by the reverify gate.
//
// Fixture (inline): empty @define with orphaned @export (empty-block fix
// rejected — removing the define leaves an unreferenced @export) plus a
// duplicate @export (applied successfully).  This is the same scenario as
// the file-mode partial-fix fixture; the stdin variant confirms the sentinel
// is STDIN_DISPLAY_LABEL, not a file path (AD-211-2).

/// AC-P1-03(c): the "Partially fixed:" status line uses the bracketed sentinel.
#[test]
fn stdin_partial_fix_message_uses_bracketed_sentinel() {
    // Source: empty @define (empty-block, Tier A) + duplicate export.
    // The empty-block fix is rejected (orphaned @export after removal);
    // the duplicate-export fix succeeds.  1 of 2 fixes applied.
    let source = "\
@define empty_fn():

@end

@export empty_fn

@define greet(name):
  Hello {{name}}!
@end

@export greet
@export greet
";
    let out = lint_stdin(source, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Partially fixed: <stdin>"),
        "AC-P1-03: 'Partially fixed: <stdin>' must appear in stderr; got:\n{stderr}"
    );
    // Must not leak bare `stdin` without angle brackets.
    assert!(
        !stderr.contains("Partially fixed: stdin\n")
            && !stderr.starts_with("Partially fixed: stdin "),
        "AC-P1-03: must not use bare 'stdin' in partial-fix message; got:\n{stderr}"
    );
}

// ── Test (i): Auto-detect hint names the invoking subcommand ─────────────────
//
// Pins bugs 22/23: auto_detect_mds_file and resolve_input now take a `subcommand:
// &str` parameter.  When multiple .mds files are present in the current directory
// and no file argument is given, the error hint must include the specific subcommand
// name (e.g. "mds lint <file>"), NOT a generic "mds build <file>".
//
// Tests two subcommands (lint, fmt) to cover separate call sites.

#[test]
fn auto_detect_hint_names_subcommand_lint_and_fmt() {
    // ── lint subcommand ──────────────────────────────────────────────────────
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_clean.mds"), dir.path().join("a.mds")).unwrap();
        fs::copy(fixture("lint_warn_only.mds"), dir.path().join("b.mds")).unwrap();

        let out = mds_bin()
            .arg("lint")
            .current_dir(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("mds lint"),
            "auto-detect hint for 'lint' must contain 'mds lint'; got: {stderr}"
        );
        // Must NOT say "mds build" or "mds fmt" (wrong subcommand).
        assert!(
            !stderr.contains("mds build"),
            "hint must not name a different subcommand; got: {stderr}"
        );
    }

    // ── fmt subcommand ───────────────────────────────────────────────────────
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_clean.mds"), dir.path().join("a.mds")).unwrap();
        fs::copy(fixture("lint_warn_only.mds"), dir.path().join("b.mds")).unwrap();

        let out = mds_bin()
            .arg("fmt")
            .current_dir(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("mds fmt"),
            "auto-detect hint for 'fmt' must contain 'mds fmt'; got: {stderr}"
        );
        assert!(
            !stderr.contains("mds build"),
            "fmt hint must not name 'mds build'; got: {stderr}"
        );
    }
}

// ── resolve-w2 regression tests ──────────────────────────────────────────────

// ── resolve-w2 #36: dir --fix --check --format json emits JSON before exit ───
//
// Regression: the `any_would_fix` early `std::process::exit(1)` in
// `run_lint_directory` was sequenced BEFORE the JSON envelope emit block, so
// stdout was empty on exit. `JSON.parse("")` throws.
//
// Fix: emit the JSON envelope BEFORE the `any_would_fix` exit so that consumers
// always receive parseable output regardless of the exit code (AC-F-14 / ADR-004).

#[test]
fn dir_fix_check_json_emits_parseable_json_before_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    // One fixable file — triggers any_would_fix = true.
    fs::copy(fixture("lint_error.mds"), dir.path().join("fixable.mds")).unwrap();

    let out = lint_path(dir.path(), &["--fix", "--check", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // --fix --check exits 1 when fixes are pending.
    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check must exit 1 when fixes are pending; stderr: {stderr}"
    );

    // stdout must be parseable JSON — not empty.
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "--fix --check --format json must emit parseable JSON before exiting; \
             parse error: {e}; stdout: '{stdout}'"
        )
    });
    assert_eq!(
        json["version"], 1,
        "envelope must have version:1; got: {json}"
    );
    assert!(
        json["files"].is_array(),
        "envelope must have files[]; got: {json}"
    );
}

// ── resolve-w2 #59: stdin --fix --check never writes fixed source ─────────────
//
// Regression: `run_lint_stdin` destructured `LintFlags` with `..`, silently
// dropping `check` and `diff`. `mds lint - --fix --check` APPLIED FIXES AND WROTE
// THE RESULT TO STDOUT instead of exiting 1 without mutating anything.
// `--check` must never mutate (avoids PF-004).

#[test]
fn stdin_fix_check_exits_1_and_writes_nothing_to_stdout() {
    // A fixable source — duplicate-export, Tier A.
    let source = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    let out = lint_stdin(source, &["--fix", "--check"]);

    // Must exit 1: fix is pending, --check signals "would change".
    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check stdin must exit 1 when fixes are pending; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout must be EMPTY — --check never writes.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.is_empty(),
        "--fix --check stdin must not write the fixed source to stdout; got: {stdout}"
    );
}

// ── resolve-w2 #43: dir-mode and single-file-mode agree on --quiet for PartiallyFixed
//
// Regression: `lint_one_file_accumulating` destructured `LintFlags` without binding
// `quiet`, so `mds lint dir/ --fix --format json --quiet` emitted "partial fix:"
// lines to stderr that the single-file equivalent suppressed. Three different message
// texts across four call sites was the root cause. Refs: issue #173.

#[test]
fn dir_and_single_file_agree_on_quiet_for_partially_fixed() {
    // Case A: single-file mode --fix --quiet must suppress "Partially fixed" on stderr.
    {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("partial.mds");
        fs::copy(fixture("lint_partial_fix.mds"), &target).unwrap();

        let out = lint_path(&target, &["--fix", "--quiet"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Partially fixed") && !stderr.to_lowercase().contains("partial fix"),
            "single-file --fix --quiet must suppress 'Partially fixed'; got: {stderr}"
        );
    }

    // Case B: directory-mode --fix --format json --quiet must ALSO suppress
    // "Partially fixed" — this is the realized defect from #43.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            fixture("lint_partial_fix.mds"),
            dir.path().join("partial.mds"),
        )
        .unwrap();

        let out = lint_path(dir.path(), &["--fix", "--format", "json", "--quiet"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Partially fixed") && !stderr.to_lowercase().contains("partial fix"),
            "dir-mode --fix --format json --quiet must suppress 'Partially fixed'; got: {stderr}"
        );

        // Stdout must still be parseable JSON.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("dir-mode --quiet must still emit valid JSON; err: {e}; stdout: {stdout}")
        });
    }

    // Case C (positive): without --quiet, both modes print the unified message.
    // Uses fresh copies so the previous partial-fix writes don't interfere.
    {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("partial.mds");
        fs::copy(fixture("lint_partial_fix.mds"), &target).unwrap();

        let out = lint_path(&target, &["--fix"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Partially fixed"),
            "single-file --fix without --quiet must print 'Partially fixed'; got: {stderr}"
        );
    }
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            fixture("lint_partial_fix.mds"),
            dir.path().join("partial.mds"),
        )
        .unwrap();

        let out = lint_path(dir.path(), &["--fix", "--format", "json"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Partially fixed"),
            "dir-mode --fix --format json without --quiet must print 'Partially fixed'; got: {stderr}"
        );
    }
}

// ── Bare-filename regression (PF-006) ────────────────────────────────────────

/// `mds lint --fix` on a bare filename must apply fixes in place.
///
/// Regression for PF-006: `path.parent()` on a bare filename returns `Some("")`.
/// `NativeFs::check_symlink("")` failed because `"".file_name()` returns `None`,
/// making `atomic_write_file` reject every fix edit with an Io error — silently
/// turning `--fix` into a no-op for any file passed as a bare name.
///
/// Uses `.current_dir(tempdir)` with a bare argument.
#[test]
fn lint_fix_bare_filename_applies_fix() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dup.mds");
    fs::copy(fixture("lint_error.mds"), &target).unwrap();

    let original = fs::read_to_string(&target).unwrap();
    assert!(
        original.contains("@export greet\n@export greet"),
        "fixture must have duplicate export"
    );

    let out = mds_bin()
        .arg("lint")
        .arg("--fix")
        .arg("dup.mds") // bare filename — the only form that triggered the bug
        .current_dir(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--fix on bare filename should exit 0 after fixing; stderr: {stderr}"
    );

    let after = fs::read_to_string(&target).unwrap();
    assert_ne!(after, original, "--fix must have rewritten the file");
    assert_eq!(
        after.matches("@export greet").count(),
        1,
        "exactly one @export greet should remain after --fix; got:\n{after}"
    );
}

// ── ESC injection regression (issue #176 / ESC-INJECTION) ─────────────────────

/// Regression gate: a .mds file containing a raw ESC byte (U+001B) that reaches
/// `MdsError::Syntax` must not emit raw ESC bytes to stderr — single-file mode.
///
/// Background: `MdsError::Syntax` embeds user-controlled source fragments via
/// miette's NamedSource.  Before the fix, those fragments printed with raw ESC
/// bytes intact, enabling terminal escape injection when linting untrusted repos.
/// The fix sanitizes at the CLI render boundary in `emit_analysis_failure_json_or_stderr`.
#[test]
fn lint_esc_byte_in_syntax_error_is_sanitized_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("esc_test.mds");
    // Raw ESC byte (0x1B) on the same line as the syntax error so miette renders it
    // as part of the source context.  Unclosed @define → guaranteed syntax error.
    fs::write(&file, b"@define \x1bfoo:\nhello\n").unwrap();

    let out = lint_path(&file, &[]);
    // Must exit 2 (analysis/gate failure, not lint-severity exit 1).
    assert_eq!(
        out.status.code(),
        Some(2),
        "syntax error should exit 2; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Raw ESC byte (0x1B) must not appear anywhere in stderr output.
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must be sanitized before writing to stderr; \
         got (hex): {:02x?}",
        &out.stderr[..out.stderr.len().min(512)]
    );
}

// ── T-5..T-9: complete ESC-injection hardening for lint output (issue #176) ───
//
// The vector used by T-5..T-8 is a .mds file whose template body line contains a
// raw ESC byte (U+001B) as part of a `legacy-interpolation` finding.  The line
// "Hello \x1b{name} world" contains both the friendly word "Hello" (guard against
// vacuous passes) and the raw ESC byte; `legacy-interpolation` fires because `{name}`
// matches the old single-brace syntax.
//
// C1 note (T-8): raw 0x80–0x9F bytes are invalid UTF-8 so they are rejected by the
// MDS lexer before any lint rule runs.  The C1 representative used here is U+0085
// (NEL, UTF-8 0xC2 0x85), which is valid UTF-8 and is neutralized by
// `neutralize_source_for_render` before miette renders the source frame.
//
// Neutralization model (PF-014): control bytes in source are substituted at the INPUT
// boundary before miette renders, not post-processed over the rendered frame.
// - C0/DEL (1-byte) → '?' (1 byte, byte-length-preserving)
// - C1 (2-byte UTF-8 0x80–0x9F) → U+00A0 NBSP (2 bytes, byte-length-preserving)
// `sanitize_control_chars` (\\uXXXX expansion) is used only for message and help text.
//
// All tests use `NO_COLOR=1` so ANSI colour codes in miette output don't interfere
// with the raw-byte search.

/// T-5 [AC-F1]: single-file mode — human-format lint on a file whose diagnostic
/// source line contains a raw ESC byte must produce no raw 0x1B byte on stderr.
#[test]
fn lint_single_file_esc_in_diagnostic_frame_is_sanitized() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("esc_lint.mds");
    // "Hello \x1b{name} world" — legacy-interpolation fires on `{name}`;
    // the raw ESC byte on the same line reaches the source frame.
    fs::write(&file, b"Hello \x1b{name} world\n").unwrap();

    let out = lint_path(&file, &[]);

    let stderr_str = String::from_utf8_lossy(&out.stderr);
    // Exit 1 — legacy-interpolation is Warn severity.
    assert_eq!(
        out.status.code(),
        Some(1),
        "legacy-interpolation is warn-only; expected exit 1; stderr: {stderr_str}"
    );
    // No raw control bytes on stderr (char-based check — catches ESC, DEL, and C1).
    assert_no_control_chars(&stderr_str, "T-5 stderr");
    // Under input-neutralization (PF-014): ESC → '?' in the source frame.
    // "Hello" must be visible, confirming source context is non-vacuous.
    assert!(
        stderr_str.contains("Hello"),
        "source context word 'Hello' must be visible in output; got: {stderr_str:?}"
    );
    // Stdout must be empty (human mode only writes to stderr).
    assert!(
        out.stdout.is_empty(),
        "human mode must not write to stdout; got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// T-6 [AC-F1]: directory mode — same vector via dir walk.
#[test]
fn lint_directory_esc_in_diagnostic_frame_is_sanitized() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("esc_lint.mds"), b"Hello \x1b{name} world\n").unwrap();

    let out = lint_path(dir.path(), &[]);

    let stderr_str = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "directory mode: expected exit 1; stderr: {stderr_str}"
    );
    // No raw control bytes on stderr (char-based check).
    assert_no_control_chars(&stderr_str, "T-6 stderr");
    // Under input-neutralization (PF-014): ESC → '?' in the source frame.
    assert!(
        stderr_str.contains("Hello"),
        "directory mode: 'Hello' must be visible in output; got: {stderr_str:?}"
    );
    // Stdout must be empty (human mode).
    assert!(
        out.stdout.is_empty(),
        "directory mode: stdout must be empty; got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// T-7 [AC-F1]: stdin mode — same vector via the BrokenPipe-safe `lint_stdin` helper.
#[test]
fn lint_stdin_esc_in_diagnostic_frame_is_sanitized() {
    let input = "Hello \x1b{name} world\n";
    let out = lint_stdin(input, &[]);

    let stderr_str = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdin mode: expected exit 1; stderr: {stderr_str}"
    );
    // No raw control bytes on stderr (char-based check).
    assert_no_control_chars(&stderr_str, "T-7 stderr");
    // Under input-neutralization (PF-014): ESC → '?' in the source frame.
    assert!(
        stderr_str.contains("Hello"),
        "stdin mode: 'Hello' must be visible in output; got: {stderr_str:?}"
    );
}

/// T-8 [AC-F1]: DEL (U+007F) and C1 NEL (U+0085, UTF-8 0xC2 0x85) must both be
/// sanitized in the rendered source frame.
#[test]
fn lint_del_and_c1_in_diagnostic_frame_is_sanitized() {
    let dir = tempfile::tempdir().unwrap();
    // U+007F = DEL; U+0085 = C1 NEL (valid UTF-8, 0xC2 0x85).
    // `{name}` trips legacy-interpolation so we get a diagnostic and a source frame.
    let content = "Hello\u{007F}and\u{0085}{name}world\n";
    fs::write(dir.path().join("del_c1_lint.mds"), content.as_bytes()).unwrap();

    let out = lint_path(dir.path(), &[]);

    let stderr_str = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "del/c1 test: expected exit 1; stderr: {stderr_str}"
    );
    // DEL must be neutralized: 0x7F → '?' (1-byte → 1-byte, PF-014).
    assert!(
        !stderr_str.contains('\u{007F}'),
        "raw DEL must not appear on stderr; got: {stderr_str:?}"
    );
    // C1 NEL must be neutralized: U+0085 → U+00A0 NBSP (2-byte → 2-byte, PF-014).
    assert!(
        !stderr_str.contains('\u{0085}'),
        "raw C1 NEL must not appear on stderr; got: {stderr_str:?}"
    );
    assert!(
        stderr_str.contains('\u{00A0}'),
        "neutralized C1 NEL must appear as NBSP (U+00A0) in source frame; got: {stderr_str:?}"
    );
    // Source context guard: "Hello" confirms output is non-vacuous.
    assert!(
        stderr_str.contains("Hello"),
        "source context word 'Hello' must be visible; got: {stderr_str:?}"
    );
}

/// T-9 [AC-C3]: `mds lint --format json` on a source whose `duplicate-import`
/// diagnostic message embeds a raw C1 control character (U+0085 NEL) must emit
/// valid JSON with no raw control bytes anywhere — in particular the embedded
/// path must be escaped to its 6-character JSON escape (backslash, u, 0, 0, 8, 5).
///
/// ## Why this vector?
///
/// The original T-9 used a YAML frontmatter key containing a raw ESC byte
/// (`"a\x1Bb": 1`).  `serde_yaml_ng` rejects raw ESC/DEL bytes inside
/// double-quoted YAML keys, so the test never reached the lint code path at all
/// — the YAML parser rejected the input before any sanitizer ran, making every
/// assertion vacuous (PF-013 shape).
///
/// U+0085 (NEL, C1 NEL) is a C1 control character whose UTF-8 encoding
/// (0xC2 0x85) **passes** `serde_yaml_ng` YAML parsing.  We exploit a
/// different route: the `duplicate-import` rule fires when the same module is
/// imported twice and embeds the raw import path in its message.  A module
/// whose file *name* contains U+0085 therefore injects that byte into the
/// diagnostic message.  When `to_canonical_json` serializes the result, it
/// must sanitize U+0085 into its 6-character ASCII JSON escape; if that
/// sanitization is removed the raw 0xC2 0x85 bytes appear in the JSON wire.
///
/// ## Failure mode (regression guard)
///
/// Removing the `sanitize_control_chars` call in `to_canonical_json` causes:
///   - Gate 1 still passes (the JSON is syntactically valid)
///   - Gate 2 FAILS: `assert_no_control_chars` finds U+0085 (a C1 char) in
///     the JSON wire output
///   - Gate 3 FAILS: the per-message check finds U+0085 in the diagnostic message
///   - The positive assertion FAILS: the 6-character escape is not present when
///     raw bytes leak
#[test]
fn lint_json_hostile_source_output_contains_no_raw_control_bytes() {
    let dir = tempfile::tempdir().unwrap();

    // Helper module whose file name contains U+0085 (NEL, C1).  This character
    // survives serde_yaml_ng YAML parsing (unlike ESC/DEL which are rejected).
    let nel_name = "fo\u{0085}o.mds";
    let nel_module = dir.path().join(nel_name);
    fs::write(&nel_module, "hi\n").unwrap();

    // main.mds imports the NEL-named module twice → duplicate-import fires.
    // The diagnostic message embeds the raw import path (including U+0085).
    let import_line = format!("@import \"./{nel_name}\"\n");
    let main_content = format!("{import_line}{import_line}");
    let main_file = dir.path().join("main.mds");
    fs::write(&main_file, main_content.as_bytes()).unwrap();

    let out = lint_path(&main_file, &["--format", "json"]);

    let stdout_str = String::from_utf8_lossy(&out.stdout);

    // Gate 1 (fail-closed): stdout must be valid JSON with version 1.
    let json: serde_json::Value = serde_json::from_str(&stdout_str).unwrap_or_else(|e| {
        panic!(
            "T-9: lint --format json must emit valid JSON; parse error: {e}; \
             stdout: {stdout_str:?}"
        )
    });
    assert_eq!(json["version"], 1, "T-9: JSON version must be 1");

    // Gate 2 (fail-closed, char-based): the entire JSON wire must contain no raw
    // C0 (excl. \t \n), DEL, or C1 control codepoints.
    // Uses `assert_no_control_chars` which iterates over Unicode codepoints, not
    // raw bytes, to avoid false-positives on continuation bytes of non-C1 chars.
    assert_no_control_chars(&stdout_str, "T-9 JSON wire output");

    // Gate 3 (fail-closed): assert diagnostics ARE present and the rule fired.
    let files = json["files"]
        .as_array()
        .unwrap_or_else(|| panic!("T-9: JSON must have 'files' array; got: {json}"));
    assert!(
        !files.is_empty(),
        "T-9: files array must be non-empty (duplicate-import should fire); got: {json}"
    );
    let all_diags: Vec<&serde_json::Value> = files
        .iter()
        .flat_map(|f| {
            f["diagnostics"].as_array().unwrap_or_else(|| {
                panic!(
                    "T-9: every file entry must have a 'diagnostics' array; \
                                           got: {f}"
                )
            })
        })
        .collect();
    assert!(
        !all_diags.is_empty(),
        "T-9: must have at least one diagnostic; got: {files:?}"
    );
    let has_dup_import = all_diags.iter().any(|d| d["rule"] == "duplicate-import");
    assert!(
        has_dup_import,
        "T-9: duplicate-import must be among the diagnostics; got rules: {:?}",
        all_diags.iter().map(|d| &d["rule"]).collect::<Vec<_>>()
    );
    // Per-message sanitization check (char-based).
    for diag in &all_diags {
        let msg = diag["message"]
            .as_str()
            .unwrap_or_else(|| panic!("T-9: diagnostic must have a string 'message'; got: {diag}"));
        assert_no_control_chars(msg, "T-9 diagnostic message");
    }

    // Positive assertion (non-vacuous, PF-013): the sanitized escape for U+0085
    // must appear in at least one message.  If sanitization is removed the raw
    // U+0085 character leaks and this assertion fails because the 6-char literal
    // is absent while the raw codepoint (caught by Gate 2/3) is present.
    //
    // After JSON deserialisation by serde_json the string value is the
    // 6-character sequence: backslash, u, 0, 0, 8, 5.
    let has_sanitized_nel = all_diags.iter().any(|d| {
        d["message"]
            .as_str()
            .map(|m| m.contains("\\u0085"))
            .unwrap_or(false)
    });
    assert!(
        has_sanitized_nel,
        "T-9: sanitized \\u0085 literal must appear in at least one diagnostic message; \
         got messages: {:?}",
        all_diags.iter().map(|d| &d["message"]).collect::<Vec<_>>()
    );
}

/// T-9b [AC-C3]: the Trojan Source vector (CVE-2021-42574) end-to-end through
/// `mds lint --format json`.
///
/// U+202E RIGHT-TO-LEFT OVERRIDE is not a C0/DEL/C1 control character, so before
/// the escape class was widened it passed through the wire untouched. A single RLO
/// inside a filename makes every downstream renderer (terminal, IDE, code-review UI)
/// display the remainder of the line in reverse — a benign-looking diagnostic can be
/// made to read as something entirely different from its bytes.
///
/// Reachability: the same `duplicate-import` route T-9 uses. The rule embeds the raw
/// import path in its message, and the path is a plain string (not YAML), so U+202E
/// reaches the lint engine intact.
///
/// Non-vacuity (PF-013): asserts the `files` array is non-empty, that
/// `duplicate-import` actually fired, AND the POSITIVE assertion that the escaped
/// `\\u202E` literal is present — all three fail if the widened class is reverted.
#[test]
fn lint_json_bidi_override_in_import_path_is_escaped() {
    let dir = tempfile::tempdir().unwrap();

    // "fo<RLO>gnp.mds" renders as "fopng.mds" in a bidi-aware terminal.
    let rlo_name = "fo\u{202E}gnp.mds";
    fs::write(dir.path().join(rlo_name), "hi\n").unwrap();

    let import_line = format!("@import \"./{rlo_name}\"\n");
    let main_file = dir.path().join("main.mds");
    fs::write(&main_file, format!("{import_line}{import_line}").as_bytes()).unwrap();

    let out = lint_path(&main_file, &["--format", "json"]);
    let stdout_str = String::from_utf8_lossy(&out.stdout);

    // Gate 1 (fail-closed): valid JSON, version 1.
    let json: serde_json::Value = serde_json::from_str(&stdout_str).unwrap_or_else(|e| {
        panic!("T-9b: lint --format json must emit valid JSON; parse error: {e}; stdout: {stdout_str:?}")
    });
    assert_eq!(json["version"], 1, "T-9b: JSON version must be 1");

    // Gate 2 (fail-closed): no raw hostile codepoint anywhere in the wire output.
    assert_no_control_chars(&stdout_str, "T-9b JSON wire output");

    // Gate 3 (non-vacuity): the rule actually fired.
    let files = json["files"]
        .as_array()
        .unwrap_or_else(|| panic!("T-9b: JSON must have 'files' array; got: {json}"));
    assert!(
        !files.is_empty(),
        "T-9b: files array must be non-empty (duplicate-import should fire); got: {json}"
    );
    let all_diags: Vec<&serde_json::Value> = files
        .iter()
        .flat_map(|f| {
            f["diagnostics"]
                .as_array()
                .unwrap_or_else(|| panic!("T-9b: every file entry needs 'diagnostics'; got: {f}"))
        })
        .collect();
    assert!(
        all_diags.iter().any(|d| d["rule"] == "duplicate-import"),
        "T-9b: duplicate-import must be among the diagnostics; got rules: {:?}",
        all_diags.iter().map(|d| &d["rule"]).collect::<Vec<_>>()
    );

    // Positive assertion (PF-013): the escaped form must be present.
    let has_escaped_rlo = all_diags
        .iter()
        .any(|d| d["message"].as_str().is_some_and(|m| m.contains("\\u202E")));
    assert!(
        has_escaped_rlo,
        "T-9b: escaped \\u202E literal must appear in at least one diagnostic message; \
         got messages: {:?}",
        all_diags.iter().map(|d| &d["message"]).collect::<Vec<_>>()
    );
}

/// T-9c [AC-C3]: wire-mode newline escaping end-to-end — the log/YAML-key forging
/// vector.
///
/// A raw newline inside a diagnostic message lets an attacker forge what reads as a
/// second, independent finding in any consumer that prints or line-splits the JSON
/// string value. On the wire that newline (U+000A) must arrive as its 6-char escape
/// literal.
///
/// ## Why this vector?
///
/// The obvious route — a POSIX filename containing a newline, imported twice — is
/// **not reachable**: the lexer rejects a newline inside an `@import "..."` path with
/// `syntax error: unclosed quote in path` before any lint rule runs, which would make
/// every assertion below vacuous (PF-013 shape).
///
/// A YAML **double-quoted frontmatter key** does reach it: `serde_yaml_ng` decodes the
/// `\n` escape into a real newline, and `unused-variable` embeds the decoded key
/// verbatim in its message. The forged payload mimics a real diagnostic header, so a
/// naive line-oriented consumer would render it as a genuine second finding.
#[test]
fn lint_json_newline_in_frontmatter_key_is_escaped_on_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("forge.mds");

    // The `\n` sequences below are YAML escapes inside a double-quoted key — after
    // YAML parsing the key contains two REAL newline characters.
    fs::write(
        &file,
        b"---\n\"a\\nerror[mds::forged]: FAKE\\nb\": 1\n---\nHello\n",
    )
    .unwrap();

    let out = lint_path(&file, &["--format", "json"]);
    let stdout_str = String::from_utf8_lossy(&out.stdout);

    let json: serde_json::Value = serde_json::from_str(&stdout_str).unwrap_or_else(|e| {
        panic!("T-9c: lint --format json must emit valid JSON; parse error: {e}; stdout: {stdout_str:?}")
    });
    assert_eq!(json["version"], 1, "T-9c: JSON version must be 1");

    let files = json["files"]
        .as_array()
        .unwrap_or_else(|| panic!("T-9c: JSON must have 'files' array; got: {json}"));
    assert!(
        !files.is_empty(),
        "T-9c: files array must be non-empty; got: {json}"
    );
    let all_diags: Vec<&serde_json::Value> = files
        .iter()
        .flat_map(|f| {
            f["diagnostics"]
                .as_array()
                .unwrap_or_else(|| panic!("T-9c: every file entry needs 'diagnostics'; got: {f}"))
        })
        .collect();

    // Non-vacuity: the vector really did reach the lint engine.
    assert!(
        all_diags.iter().any(|d| d["rule"] == "unused-variable"),
        "T-9c: unused-variable must fire; got rules: {:?}",
        all_diags.iter().map(|d| &d["rule"]).collect::<Vec<_>>()
    );

    // No parsed message may contain a raw newline …
    for diag in &all_diags {
        let msg = diag["message"].as_str().unwrap_or_else(|| {
            panic!("T-9c: diagnostic must have a string 'message'; got: {diag}")
        });
        assert!(
            !msg.contains('\n'),
            "T-9c: raw newline must not survive into a wire message; got: {msg:?}"
        );
    }

    // … and the escaped form must be present (positive, non-vacuous).
    assert!(
        all_diags
            .iter()
            .any(|d| d["message"].as_str().is_some_and(|m| m.contains("\\u000A"))),
        "T-9c: escaped \\u000A literal must appear in at least one diagnostic message; \
         got messages: {:?}",
        all_diags.iter().map(|d| &d["message"]).collect::<Vec<_>>()
    );

    // The forged payload text itself is preserved verbatim (only the newline changed),
    // proving the guard escapes rather than strips.
    assert!(
        all_diags.iter().any(|d| d["message"]
            .as_str()
            .is_some_and(|m| m.contains("error[mds::forged]"))),
        "T-9c: message body must be preserved verbatim; got messages: {:?}",
        all_diags.iter().map(|d| &d["message"]).collect::<Vec<_>>()
    );
}

// ── atomic_write_file: mode preservation and error-message coverage ──────────

/// Regression gate: `mds lint --fix <file>` must preserve the original Unix file
/// mode after applying auto-fixes via `atomic_write_file`.
///
/// `tempfile::Builder` defaults to mode 0600; without the permission-restoration
/// step a 0644 source file turns owner-only after the rename.  The masking of
/// file-type bits (`mode & 0o7777`) ensures `Permissions::from_mode` receives
/// only the permission bits.  This test locks in that guarantee for the lint path
/// now that `atomic_write_file` lives in `output.rs` and is shared with `fmt`.
#[cfg(unix)]
#[test]
fn lint_fix_preserves_mode_0644() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("perm_lint.mds");
    // Write content with a single Tier A auto-fixable issue (duplicate-export)
    // and no residual warning, so --fix exits 0 (clean after fix).
    fs::write(
        &target,
        "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n",
    )
    .unwrap();
    // Set 0644 explicitly before invoking --fix.
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();

    let out = lint_path(&target, &["--fix"]);
    // Must succeed (duplicate-export is Tier A; residual after fix is clean).
    assert!(
        out.status.success(),
        "lint --fix should succeed on duplicate-export fixture; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o644,
        "lint --fix must preserve file mode 0644 after atomic write; got 0{mode:o}"
    );
}

/// Regression gate: when `atomic_write_file` fails (e.g. directory not writable),
/// the error message emitted to stderr MUST include the target filename so the
/// user can diagnose which file caused the failure.
///
/// This gates that all error paths in `atomic_write_file` carry `path.display()`.
/// Previously only the `persist()` error carried the path; all other paths
/// (temp-file creation, permission set, write, fsync) emitted generic messages.
/// Fixed by step 9.1 (all 5 non-persist errors now include `path.display()`).
#[cfg(unix)]
#[test]
fn lint_write_failure_includes_filename_in_stderr() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("write_fail.mds");
    // A single Tier A auto-fixable issue so lint will attempt to write the file.
    fs::write(
        &target,
        "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n",
    )
    .unwrap();

    // Make the parent directory read-only so temp-file creation fails.
    // This triggers the "cannot create temp file for {path}" error path.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

    let out = lint_path(&target, &["--fix"]);

    // Restore writability so tempdir cleanup can succeed.
    let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755));

    // The write must have failed (non-zero exit).
    assert_ne!(
        out.status.code(),
        Some(0),
        "lint --fix must fail when the parent dir is read-only"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    // The filename "write_fail.mds" must appear in the error message.
    assert!(
        stderr.contains("write_fail.mds"),
        "error stderr must contain the target filename; got: {stderr}"
    );
}

/// Regression gate (single-file mode): when `atomic_write_file` fails,
/// stderr must NOT contain "Fixed: <file>" — the success label must only
/// appear after a successful write, never before.
///
/// Previously `lint.rs` emitted `eprintln!("Fixed: ...")` BEFORE calling
/// `atomic_write_file`, so a failed write printed "Fixed: …/e1.mds" followed
/// immediately by "error writing …/e1.mds" — actively lying about the
/// outcome.  `fmt.rs:284` already does this correctly (write first, label on
/// `Ok(())`); this test locks in parity for both lint modes.
#[cfg(unix)]
#[test]
fn lint_fix_write_failure_does_not_print_fixed_label_single_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("no_fixed_label.mds");
    // Tier A auto-fixable content.
    fs::write(
        &target,
        "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n",
    )
    .unwrap();

    // Make the parent directory read-only so atomic_write_file fails.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

    let out = lint_path(&target, &["--fix"]);

    // Restore writability before any assertions (ensures tempdir cleanup succeeds
    // even if the test panics).
    let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755));

    let stderr = String::from_utf8_lossy(&out.stderr);

    // The write must have failed.
    assert_ne!(
        out.status.code(),
        Some(0),
        "lint --fix must fail when the parent dir is read-only; stderr: {stderr}"
    );

    // "Fixed:" must NOT appear — emitting it before the write would be a lie.
    assert!(
        !stderr.contains("Fixed:"),
        "stderr must not contain 'Fixed:' when the write failed; got: {stderr}"
    );
}

/// Regression gate (directory mode): when `atomic_write_file` fails,
/// stderr must NOT contain "Fixed: <file>" — mirrors the single-file check
/// above for the `lint_one_file_human` code path (lint.rs:1227).
#[cfg(unix)]
#[test]
fn lint_fix_write_failure_does_not_print_fixed_label_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("files");
    fs::create_dir(&inner).unwrap();

    let target = inner.join("no_fixed_label_dir.mds");
    fs::write(
        &target,
        "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n",
    )
    .unwrap();

    // Make the inner directory read-only so temp-file creation fails on write.
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o555)).unwrap();

    // Run lint --fix on the directory (directory mode routes through lint_one_file_human).
    let out = lint_path(&inner, &["--fix"]);

    let _ = fs::set_permissions(&inner, fs::Permissions::from_mode(0o755));

    let stderr = String::from_utf8_lossy(&out.stderr);

    // The write must have failed (directory mode tallies the error and may still
    // exit non-zero, but "Fixed:" must not appear for the failed file).
    assert!(
        !stderr.contains("Fixed:"),
        "stderr must not contain 'Fixed:' when the directory-mode write failed; got: {stderr}"
    );
}

/// Regression gate (directory JSON mode): when `atomic_write_file` fails in
/// `lint_one_file_accumulating`, stderr must NOT contain "Fixed: <file>" — mirrors
/// the human-mode check above for the `lint_one_file_human` code path.
///
/// Code ordering is correct: `lint_one_file_accumulating` returns `FileTally::Error`
/// at lint.rs:1535 before reaching the `eprintln!("Fixed: …")` at lint.rs:1539.
/// This test provides the coverage that the code ordering is verified.
///
/// Positive control (PF-013/ADR-009): a writable directory run confirms "Fixed:"
/// DOES appear so the absence assertion below cannot be vacuous.
#[cfg(unix)]
#[test]
fn lint_fix_write_failure_json_dir_does_not_print_fixed_label() {
    use std::os::unix::fs::PermissionsExt as _;

    // ── Positive control (PF-013): in a writable directory, "Fixed:" must appear ──
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_error.mds"), dir.path().join("fixable.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix", "--format", "json"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Fixed:"),
            "positive control: dir --fix --format json must emit 'Fixed:' when the write \
             succeeds (otherwise the absence assertion below is vacuous; ADR-009); \
             got stderr: {stderr:?}"
        );
        // PR1 contract: stdout carries ONLY the JSON document.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "stdout must be valid JSON in dir --fix --format json; err: {e}; stdout: {stdout}"
            )
        });
    }

    // ── Negative (failure gate): in a read-only directory, "Fixed:" must be absent ──
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("files");
    fs::create_dir(&inner).unwrap();

    let target = inner.join("no_fixed_label_json.mds");
    fs::copy(fixture("lint_error.mds"), &target).unwrap();

    // Make the inner directory read-only so temp-file creation fails on write.
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o555)).unwrap();

    // Run lint --fix --format json on the directory (routes through lint_one_file_accumulating).
    let out = lint_path(&inner, &["--fix", "--format", "json"]);

    let _ = fs::set_permissions(&inner, fs::Permissions::from_mode(0o755));

    let stderr = String::from_utf8_lossy(&out.stderr);

    // "Fixed:" must NOT appear — the early return at lint.rs:1535 must prevent it.
    assert!(
        !stderr.contains("Fixed:"),
        "stderr must not contain 'Fixed:' when the JSON-dir write failed \
         (lint_one_file_accumulating, lint.rs:1535 guard); got: {stderr:?}"
    );
}

/// Regression gate: `mds lint <dir>` (directory mode) must not emit raw ESC bytes to
/// stderr when a source file embeds a raw ESC byte (U+001B) in content that reaches
/// `MdsError::Syntax`.
///
/// Directory mode routes through `lint_one_file_human`, which previously called
/// `eprintln!("{:?}", miette::Report::from(e.clone()))` directly without sanitization.
/// That path is now guarded by `crate::output::eprint_error` (avoids PF-004 parallel-path
/// gap — the sibling that slipped past rounds 1 and 2).
#[test]
fn lint_directory_esc_byte_in_syntax_error_is_sanitized_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    // Raw ESC byte (0x1B) in a .mds file that has a syntax error (unclosed @define).
    // The ESC is on the error line so miette renders it inside the source context frame.
    fs::write(dir.path().join("esc_dir.mds"), b"@define \x1bfoo:\nhello\n").unwrap();

    let out = lint_path(dir.path(), &[]);
    // Must exit non-zero (syntax error aborts lint analysis).
    assert_ne!(
        out.status.code(),
        Some(0),
        "lint dir with syntax error should exit non-zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Raw ESC byte (0x1B) must not appear anywhere in stderr.
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must be sanitized before writing to stderr (directory mode); \
         got (hex): {:02x?}",
        &out.stderr[..out.stderr.len().min(512)]
    );
    // Stdout must also be clean (JSON path not taken in human mode).
    assert!(
        !out.stdout.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must not appear in stdout; \
         got (hex): {:02x?}",
        &out.stdout[..out.stdout.len().min(512)]
    );
}

// ── A6: per-file config discovery ────────────────────────────────────────────

/// A6: per-file config discovery — nested mds.json overrides parent for files
/// in its subtree.
///
/// Layout:
///   root/
///     mds.json          {"lint": {"rules": {"shadow-variable": "off"}}}
///     top.mds           (shadow-variable template — no diagnostic at root)
///     sub/
///       mds.json        {"lint": {"rules": {"shadow-variable": "error"}}}
///       sub.mds         (same shadow-variable template — error in sub)
///
/// Without A6 (single root config for all): both files use root config
/// (shadow-variable=off) → exit 0.
/// With A6 (per-file config): sub.mds uses sub config (shadow-variable=error)
/// → exit 2.
#[test]
fn lint_dir_nested_config_per_file_discovery() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Root config: shadow-variable=off (suppress diagnostic).
    fs::write(
        root.join("mds.json"),
        r#"{"lint":{"rules":{"shadow-variable":"off"}}}"#,
    )
    .unwrap();

    // Template that triggers shadow-variable: `item` declared in frontmatter,
    // reused as the @for loop variable.
    let shadow_template = "---\nitem: outer\nitems: [a, b]\n---\n\
                           Before: {item}\n@for item in items:\n- {item}\n@end\nAfter: {item}\n";
    fs::write(root.join("top.mds"), shadow_template).unwrap();

    // Subdirectory with its own config: shadow-variable=error.
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("mds.json"),
        r#"{"lint":{"rules":{"shadow-variable":"error"}}}"#,
    )
    .unwrap();
    fs::write(sub.join("sub.mds"), shadow_template).unwrap();

    let out = lint_path(root, &[]);

    // sub.mds must trigger shadow-variable at error severity → exit 2.
    assert_eq!(
        out.status.code(),
        Some(2),
        "lint dir with nested config (shadow-variable=error in sub/) must exit 2; \
         stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );

    // top.mds has shadow-variable=off — its presence in output must not carry
    // an error-severity diagnostic, i.e. the error must originate from sub.mds.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sub.mds") || stderr.contains("sub"),
        "stderr must mention sub.mds as the source of the error; got: {stderr}"
    );
}

/// A6: per-file config discovery — a malformed mds.json in a subdirectory
/// produces a per-file error for files in that subtree, but files outside the
/// subtree continue linting normally.
///
/// Layout:
///   root/
///     clean.mds         (no issues — no root mds.json, defaults apply)
///     bad/
///       mds.json        (invalid JSON — parse failure)
///       bad.mds         (clean template, but config load fails)
///
/// Expected: exit 2 (per-file config error for bad.mds); clean.mds is
/// unaffected (its FileTally is Clean).
#[test]
fn lint_dir_nested_malformed_config_per_file_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Clean file in root — no mds.json present, defaults apply.
    fs::write(
        root.join("clean.mds"),
        "---\ngreeting: hi\n---\n{{greeting}} world!\n",
    )
    .unwrap();

    // Subdirectory with a malformed mds.json.
    let bad = root.join("bad");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("mds.json"), "{ INVALID JSON").unwrap();
    fs::write(
        bad.join("bad.mds"),
        "---\ngreeting: hi\n---\n{{greeting}} world!\n",
    )
    .unwrap();

    let out = lint_path(root, &[]);

    // Malformed config in bad/ must cause exit 2.
    assert_eq!(
        out.status.code(),
        Some(2),
        "lint dir with malformed nested mds.json must exit 2; \
         stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );

    // The error must be reported — stderr or stdout should mention config or mds.json.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("mds.json") || combined.contains("config") || combined.contains("bad"),
        "output must reference the malformed config source; got: {combined}"
    );
}

// ── AC-224-10/11/21/22/19: unknown rule-name warn-and-continue ───────────────
//
// An unknown rule name in `mds.json` must:
//   AC-224-10: not alter the JSON wire shape on stdout (lint continues as normal)
//   AC-224-11: emit the warning on stderr (never stdout)
//   AC-224-21: preserve valid JSON on stdout in --format json mode
//   AC-224-22: be suppressed by --quiet
//   AC-224-19: emit exactly ONE warning per mds lint invocation, not one per file

/// Write a `mds.json` with an unknown rule name into `dir`.
fn write_unknown_rule_config(dir: &std::path::Path) {
    let config = serde_json::json!({
        "lint": {
            "rules": {
                "no-such-rule-xyzzy": "warn"
            }
        }
    });
    std::fs::write(
        dir.join("mds.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
}

/// AC-224-21 / AC-224-11: warning goes to stderr; stdout JSON is unaffected.
///
/// In `--format json` mode the stdout JSON must be valid and not contain the
/// warning text; the warning must appear on stderr instead.
#[test]
fn unknown_rule_warning_to_stderr_not_stdout_in_json_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("clean.mds"), "Hello!\n").unwrap();
    write_unknown_rule_config(dir.path());

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .arg("--format")
        .arg("json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // AC-224-21: stdout must still be valid JSON (the warning must NOT land there).
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "AC-224-21: stdout must be valid JSON even when unknown rules are present; \
             parse error: {e}; stdout: {stdout}"
        )
    });
    assert_eq!(
        parsed["version"].as_u64(),
        Some(1),
        "AC-224-21: JSON must have version:1; got: {parsed}"
    );
    assert!(
        !stdout.contains("unknown"),
        "AC-224-21: 'unknown' warning text must not appear in stdout JSON; got: {stdout}"
    );

    // AC-224-11: the warning must appear on stderr and name the rule.
    // Both substrings required (&&, not ||) so neither half alone can satisfy the
    // positive control — aligns with the stronger form in
    // `unknown_rule_json_stdout_contains_no_warning_text` (avoids PF-013 asymmetry).
    assert!(
        stderr.contains("unknown lint rule") && stderr.contains("no-such-rule-xyzzy"),
        "AC-224-11: the unknown-rule warning must go to stderr and name the rule; \
         got stderr: {stderr}"
    );
}

/// AC-224-10: the JSON wire shape is unchanged when lint continues past an unknown rule.
///
/// The `files` array and `truncated` / `version` fields must be present with their
/// normal shape. No extra top-level error key must appear.
#[test]
fn unknown_rule_json_wire_shape_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();
    write_unknown_rule_config(dir.path());

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .arg("--format")
        .arg("json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");

    // AC-224-10: standard fields are present; no error envelope.
    assert!(
        parsed.get("files").is_some(),
        "AC-224-10: 'files' key must be present in the JSON output; got: {parsed}"
    );
    assert!(
        parsed.get("truncated").is_some(),
        "AC-224-10: 'truncated' key must be present in the JSON output; got: {parsed}"
    );
    assert!(
        parsed.get("error").is_none(),
        "AC-224-10: no 'error' key must appear for a lint-continues result; got: {parsed}"
    );

    // Exit code must be 0 (clean file, no real diagnostics).
    assert_eq!(
        out.status.code(),
        Some(0),
        "AC-224-10: exit code must be 0 when the only unknown element is a rule name; \
         got stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// AC-224-22: `--quiet` suppresses the unknown-rule warning on stderr.
#[test]
fn unknown_rule_warning_suppressed_by_quiet() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();
    write_unknown_rule_config(dir.path());

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .arg("--quiet")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);

    // AC-224-22: --quiet must suppress the unknown-rule warning.
    assert!(
        !stderr.contains("unknown lint rule") && !stderr.contains("no-such-rule-xyzzy"),
        "AC-224-22: --quiet must suppress the unknown-rule warning; got stderr: {stderr}"
    );

    // Exit code is still 0 (clean file).
    assert_eq!(
        out.status.code(),
        Some(0),
        "AC-224-22: exit code must be 0; got stderr: {stderr}"
    );
}

/// AC-224-19: a directory tree whose files span multiple subdirectories emits exactly
/// ONE unknown-rule warning, not one per file or one per subdirectory.
///
/// A flat-directory test (all files in one dir) would be vacuous because the
/// base_dir cache already coalesces them — the caching claim under review is about
/// SUBDIRECTORIES that all resolve to the same root `mds.json`. This test uses a
/// nested tree (root + subdirs a/, b/, c/d/) so that each unique base_dir produces a
/// distinct cache lookup. Without the config_dir-keyed deduplication in
/// `LintDirCtx::config_for`, each distinct base_dir would re-load the config and
/// re-emit the warning, yielding N warnings for N subdirs (the AC-224-19 bug).
#[test]
fn unknown_rule_one_warning_per_invocation_not_per_file() {
    let dir = tempfile::tempdir().unwrap();
    // Files in different subdirectories — each has a distinct base_dir. All are
    // governed by the single root mds.json, exercising the config_dir deduplication.
    for subdir in ["a", "b", "c", "c/d"] {
        let sub = dir.path().join(subdir);
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("file.mds"), "Hello!\n").unwrap();
    }
    // One file at the root level so the base_dir == config_dir case is also covered.
    std::fs::write(dir.path().join("root.mds"), "Hello!\n").unwrap();
    // Single mds.json at the root governs every file in the tree.
    write_unknown_rule_config(dir.path());

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);

    // AC-224-19: the warning must appear at least once (positive control, PF-013).
    assert!(
        stderr.contains("unknown lint rule") && stderr.contains("no-such-rule-xyzzy"),
        "AC-224-19: warning must appear on stderr; got stderr: {stderr}"
    );

    // AC-224-19: exactly one warning per invocation, regardless of how many
    // subdirectories contributed distinct base_dir lookups. A warning_count > 1 here
    // means config_for is keying on base_dir rather than config_dir and emitting a
    // duplicate for each subdir that resolves to the same root mds.json.
    let warning_count = stderr
        .lines()
        .filter(|l| l.contains("unknown lint rule"))
        .count();
    assert_eq!(
        warning_count, 1,
        "AC-224-19: warning must appear exactly once per distinct config directory \
         (got {warning_count} — one per subdir instead of one per config); \
         got stderr:\n{stderr}"
    );
}

/// AC-224-2 (plural branch): when two or more rule names are unknown the plural form
/// of the warning is emitted and the names appear in lexicographic sorted order.
///
/// Test-plan entry 2: CLI config `{"zzz-bad":"warn","aaa-bad":"error"}` yields
/// exactly one warning line naming both unknown rules with `aaa-bad` before `zzz-bad`.
///
/// Non-vacuity (PF-013/ADR-009): the test also confirms the positive control —
/// `recognised rules are` appears — and the negative control — a known rule in the
/// same config does NOT trigger the plural path for itself.
#[test]
fn unknown_rule_plural_sorted_warning() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();
    // zzz-bad and aaa-bad are both unknown; unused-variable is real so it does NOT
    // count toward the unknown list.  The inserted order is zzz first to confirm
    // lexicographic sorting, not insertion order.
    write_rules_config(
        dir.path(),
        serde_json::json!({
            "zzz-bad":        "warn",
            "aaa-bad":        "error",
            "unused-variable": "off"
        }),
    );

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);

    // AC-224-2: both unknown names are reported.
    assert!(
        stderr.contains("aaa-bad"),
        "AC-224-2: 'aaa-bad' must appear in the plural warning; got stderr: {stderr}"
    );
    assert!(
        stderr.contains("zzz-bad"),
        "AC-224-2: 'zzz-bad' must appear in the plural warning; got stderr: {stderr}"
    );
    // AC-224-2: plural form is used (not singular).
    assert!(
        stderr.contains("unknown lint rules"),
        "AC-224-2: plural form must be used for two unknown names; got stderr: {stderr}"
    );
    // AC-224-2: aaa-bad sorts before zzz-bad.
    let aaa_pos = stderr
        .find("aaa-bad")
        .expect("aaa-bad must appear in stderr");
    let zzz_pos = stderr
        .find("zzz-bad")
        .expect("zzz-bad must appear in stderr");
    assert!(
        aaa_pos < zzz_pos,
        "AC-224-2: 'aaa-bad' must appear before 'zzz-bad' (lexicographic order); \
         got stderr: {stderr}"
    );
    // Positive control: recognised-rules list is present (AC-224-2 completeness).
    assert!(
        stderr.contains("recognised rules are"),
        "recognised-rules list must be included in the plural warning; got stderr: {stderr}"
    );
    // Negative control: unused-variable is a known rule and must NOT appear
    // QUOTED (as an unknown name) in the warning — it does appear unquoted in
    // the "recognised rules are: …" part, which is expected.  The unknown-names
    // list uses single-quote delimiters ('NAME'), so checking for the quoted
    // form distinguishes the two positions.
    let warning_line = stderr
        .lines()
        .find(|l| l.contains("unknown lint rules"))
        .unwrap_or_else(|| panic!("no plural unknown-rule warning line; got stderr: {stderr}"));
    assert!(
        !warning_line.contains("'unused-variable'"),
        "a recognised rule name must not appear quoted in the unknown-names list; \
         got warning line: {warning_line}"
    );
}

/// AC-224-2 / AC-224-3 golden (singular and plural): pins the EXACT rendered warning
/// text for both the one-offender and two-offender paths on the CLI.
///
/// Test-plan entries 2 and 3 require exact rendering, not just `contains` checks, so
/// that future edits to the message format fail loudly here rather than silently
/// diverging from the CHANGELOG and spec.md documentation.
///
/// PF-007: these goldens cover the CLI surface only; the binding surfaces are pinned
/// by their own per-surface tests.
#[test]
fn unknown_rule_cli_exact_golden() {
    // We cannot import mds:: directly from an integration test; hard-code the
    // current ten names in alphabetical order (the same value KNOWN_LINT_RULES
    // holds, verified by AC-224-7).  If the registry grows this golden must be
    // updated alongside the registry — the test will fail loudly.
    #[rustfmt::skip]
    let recognised = concat!(
        "duplicate-export, duplicate-import, empty-block, legacy-interpolation, ",
        "redundant-else, shadow-variable, unreachable-branch, unused-function, ",
        "unused-import, unused-variable"
    );

    // ── Singular golden ────────────────────────────────────────────────────────
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();
        write_rules_config(
            dir.path(),
            serde_json::json!({ "no-such-rule-xyzzy": "warn" }),
        );

        let out = mds_bin()
            .arg("lint")
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected_singular = format!(
            "warning: in mds.json: unknown lint rule 'no-such-rule-xyzzy'; \
             recognised rules are: {recognised}; ignoring"
        );
        let warning_line = stderr
            .lines()
            .find(|l| l.contains("unknown lint rule"))
            .unwrap_or_else(|| {
                panic!("no singular unknown-rule warning line; got stderr: {stderr}")
            });
        assert_eq!(
            warning_line.trim(),
            expected_singular,
            "AC-224-2/AC-224-3 singular golden mismatch"
        );
    }

    // ── Plural golden ──────────────────────────────────────────────────────────
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();
        // Insert zzz-bad before aaa-bad to confirm lexicographic sorting, not
        // insertion order.
        write_rules_config(
            dir.path(),
            serde_json::json!({ "zzz-bad": "warn", "aaa-bad": "error" }),
        );

        let out = mds_bin()
            .arg("lint")
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected_plural = format!(
            "warning: in mds.json: unknown lint rules: 'aaa-bad', 'zzz-bad'; \
             recognised rules are: {recognised}; ignoring"
        );
        let warning_line = stderr
            .lines()
            .find(|l| l.contains("unknown lint rules"))
            .unwrap_or_else(|| panic!("no plural unknown-rule warning line; got stderr: {stderr}"));
        assert_eq!(
            warning_line.trim(),
            expected_plural,
            "AC-224-2/AC-224-3 plural golden mismatch"
        );
    }
}

/// AC-224-2 / AC-224-3 golden for the SINGLE-FILE target path.
///
/// `unknown_rule_cli_exact_golden` exercises `mds lint <dir>`, which goes through
/// `LintDirCtx::config_for`. This test exercises `mds lint <file>`, which goes
/// through `load_lint_config` — a distinct code path. By pinning the EXACT warning
/// string for both paths we ensure that a future edit to either emitter fails
/// loudly here rather than diverging silently (the duplication that existed before
/// the `emit_unknown_rule_warning` extraction was introduced in this PR).
///
/// PF-007: covers the CLI single-file surface only.
#[test]
fn unknown_rule_cli_exact_golden_single_file() {
    #[rustfmt::skip]
    let recognised = concat!(
        "duplicate-export, duplicate-import, empty-block, legacy-interpolation, ",
        "redundant-else, shadow-variable, unreachable-branch, unused-function, ",
        "unused-import, unused-variable"
    );

    // ── Singular golden (single-file path) ────────────────────────────────────
    {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mds");
        std::fs::write(&file, "Hello!\n").unwrap();
        write_rules_config(
            dir.path(),
            serde_json::json!({ "no-such-rule-xyzzy": "warn" }),
        );

        let out = mds_bin()
            .arg("lint")
            .arg(&file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected_singular = format!(
            "warning: in mds.json: unknown lint rule 'no-such-rule-xyzzy'; \
             recognised rules are: {recognised}; ignoring"
        );
        let warning_line = stderr
            .lines()
            .find(|l| l.contains("unknown lint rule"))
            .unwrap_or_else(|| {
                panic!(
                    "AC-224-2/AC-224-3 single-file: no singular unknown-rule warning; \
                     got stderr: {stderr}"
                )
            });
        assert_eq!(
            warning_line.trim(),
            expected_singular,
            "AC-224-2/AC-224-3 single-file singular golden mismatch"
        );
    }

    // ── Plural golden (single-file path) ──────────────────────────────────────
    {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.mds");
        std::fs::write(&file, "Hello!\n").unwrap();
        write_rules_config(
            dir.path(),
            serde_json::json!({ "zzz-bad": "warn", "aaa-bad": "error" }),
        );

        let out = mds_bin()
            .arg("lint")
            .arg(&file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected_plural = format!(
            "warning: in mds.json: unknown lint rules: 'aaa-bad', 'zzz-bad'; \
             recognised rules are: {recognised}; ignoring"
        );
        let warning_line = stderr
            .lines()
            .find(|l| l.contains("unknown lint rules"))
            .unwrap_or_else(|| {
                panic!(
                    "AC-224-2/AC-224-3 single-file: no plural unknown-rule warning; \
                     got stderr: {stderr}"
                )
            });
        assert_eq!(
            warning_line.trim(),
            expected_plural,
            "AC-224-2/AC-224-3 single-file plural golden mismatch"
        );
    }
}

/// Populate `dir` with a fixed three-file tree — one clean, two with real findings —
/// so the JSON envelope under test carries actual `files[]` entries rather than an
/// empty array (an all-clean tree would make the comparison below near-vacuous).
fn write_mixed_lint_tree(dir: &std::path::Path) {
    std::fs::write(dir.join("clean.mds"), "Hello!\n").unwrap();
    std::fs::write(
        dir.join("unused.mds"),
        "---\nunused_key: value\n---\nHello!\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("dup.mds"),
        "---\nanother_unused: v\n---\nHi there!\n",
    )
    .unwrap();
}

/// Write an `mds.json` naming `rules` verbatim.
fn write_rules_config(dir: &std::path::Path, rules: serde_json::Value) {
    let config = serde_json::json!({ "lint": { "rules": rules } });
    std::fs::write(
        dir.join("mds.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
}

/// AC-224-10 (NEGATIVE, envelope frozen): an unknown rule name changes the `--format json`
/// stdout by **zero bytes**.
///
/// The two runs differ only by the presence of `no-such-rule-xyzzy` in `mds.json`. Their
/// stdout buffers are compared byte-for-byte, and the top-level key set is compared as an
/// EXACT set (not `contains`), so a new top-level key or a `files[]` entry lacking
/// `diagnostics` fails. Exit codes must match too.
///
/// Non-vacuity (PF-013 / ADR-009): the same run asserts the warning IS on stderr and that
/// the envelope actually carries `files[]` entries — a byte-identical comparison of two
/// empty or two error envelopes would prove nothing.
#[test]
fn unknown_rule_json_stdout_is_byte_identical_to_run_without_it() {
    let with_dir = tempfile::tempdir().unwrap();
    write_mixed_lint_tree(with_dir.path());
    write_rules_config(
        with_dir.path(),
        serde_json::json!({ "unused-variable": "warn", "no-such-rule-xyzzy": "warn" }),
    );

    let without_dir = tempfile::tempdir().unwrap();
    write_mixed_lint_tree(without_dir.path());
    write_rules_config(
        without_dir.path(),
        serde_json::json!({ "unused-variable": "warn" }),
    );

    let run = |dir: &std::path::Path| {
        mds_bin()
            .arg("lint")
            .arg(dir)
            .arg("--format")
            .arg("json")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap()
    };
    let with = run(with_dir.path());
    let without = run(without_dir.path());

    // Non-vacuity: the warning fired in the first run and not in the second.
    let with_stderr = String::from_utf8_lossy(&with.stderr);
    assert!(
        with_stderr.contains("unknown lint rule") && with_stderr.contains("no-such-rule-xyzzy"),
        "non-vacuity: the unknown-rule warning must fire on stderr; got: {with_stderr}"
    );
    let without_stderr = String::from_utf8_lossy(&without.stderr);
    assert!(
        !without_stderr.contains("unknown lint rule"),
        "control run must not warn; got: {without_stderr}"
    );

    // AC-224-10: stdout is byte-identical, and the exit code does not move.
    assert_eq!(
        with.stdout,
        without.stdout,
        "AC-224-10: stdout must be byte-identical with and without the unknown rule name;\n\
         with:    {}\nwithout: {}",
        String::from_utf8_lossy(&with.stdout),
        String::from_utf8_lossy(&without.stdout)
    );
    assert_eq!(
        with.status.code(),
        without.status.code(),
        "AC-224-13: the exit code must not move because of an unknown rule name"
    );

    // AC-224-10: exact top-level key set — not `contains`.
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&with.stdout).trim())
            .expect("stdout must be valid JSON");
    let obj = parsed.as_object().expect("envelope must be a JSON object");
    let mut top_keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    top_keys.sort_unstable();
    assert_eq!(
        top_keys,
        ["files", "truncated", "version"],
        "AC-224-10: the top-level key set must be exactly {{files, truncated, version}}; \
         got: {parsed}"
    );
    assert_eq!(parsed["version"].as_u64(), Some(1));

    // Non-vacuity: the envelope really does carry file entries, and each has the
    // `diagnostics` key with no `error` key.
    let files = parsed["files"].as_array().expect("files must be an array");
    assert!(
        !files.is_empty(),
        "non-vacuity: the fixture tree must produce at least one files[] entry; got: {parsed}"
    );
    for entry in files {
        let e = entry.as_object().expect("files[] entry must be an object");
        assert!(
            e.contains_key("diagnostics"),
            "AC-224-10: every files[] entry must carry a diagnostics key; got: {entry}"
        );
        assert!(
            !e.contains_key("error"),
            "AC-224-10: no files[] entry may carry an error key; got: {entry}"
        );
    }
}

/// AC-224-21: `--format json` stdout carries no warning text at all — including the
/// literal substring `warning`, which the bare `!stdout.contains("unknown")` check above
/// would not catch. Paired positive control: the warning IS on stderr in the same run.
#[test]
fn unknown_rule_json_stdout_contains_no_warning_text() {
    let dir = tempfile::tempdir().unwrap();
    write_mixed_lint_tree(dir.path());
    write_rules_config(
        dir.path(),
        serde_json::json!({ "no-such-rule-xyzzy": "warn" }),
    );

    let out = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .arg("--format")
        .arg("json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Positive control first: without this, every negative below is vacuous.
    assert!(
        stderr.contains("no-such-rule-xyzzy") && stderr.contains("recognised rules are"),
        "positive control: the warning must be on stderr; got: {stderr}"
    );

    for needle in [
        "no-such-rule-xyzzy",
        "recognised rules are",
        "warning",
        "ignoring",
    ] {
        assert!(
            !stdout.contains(needle),
            "AC-224-21: stdout must not contain {needle:?}; got: {stdout}"
        );
    }
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .expect("AC-224-21: stdout must parse as a single JSON document");
}

/// AC-224-21 (single-file target): `--format json` stdout is clean when the unknown-
/// rule warning fires.
///
/// `run_lint_file` is a separate emitter from `run_lint_directory`; this test covers
/// it independently (AC-224-21 gap identified in the code review). Paired positive
/// control (PF-013 / ADR-009): the warning IS on stderr, so a clean-stdout assertion
/// cannot pass vacuously on a run where the warning never fired.
#[test]
fn unknown_rule_json_stdout_clean_single_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clean.mds");
    std::fs::write(&file, "Hello!\n").unwrap();
    write_unknown_rule_config(dir.path());

    let out = mds_bin()
        .arg("lint")
        .arg(&file)
        .arg("--format")
        .arg("json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Positive control: warning fired on stderr.
    assert!(
        stderr.contains("unknown lint rule") && stderr.contains("no-such-rule-xyzzy"),
        "AC-224-21 (file): warning must fire on stderr; got: {stderr}"
    );

    // AC-224-21: stdout must be valid JSON and contain no warning text.
    serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|e| {
        panic!("AC-224-21 (file): stdout must be valid JSON; error: {e}; got: {stdout}")
    });
    for needle in [
        "no-such-rule-xyzzy",
        "recognised rules are",
        "warning",
        "ignoring",
    ] {
        assert!(
            !stdout.contains(needle),
            "AC-224-21 (file): stdout must not contain {needle:?}; got: {stdout}"
        );
    }
}

/// AC-224-21 (stdin target): `--format json` stdout is clean when the unknown-rule
/// warning fires.
///
/// `run_lint_stdin` is a separate emitter from `run_lint_directory` and
/// `run_lint_file`; this test covers it independently (AC-224-21 gap). `current_dir`
/// is set to a tempdir containing `mds.json` so `run_lint_stdin` picks up the unknown
/// rule. Paired positive control (PF-013 / ADR-009).
#[test]
fn unknown_rule_json_stdout_clean_stdin() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    write_unknown_rule_config(dir.path());

    let mut child = mds_bin()
        .arg("lint")
        .arg("-")
        .arg("--format")
        .arg("json")
        .current_dir(dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Ignore BrokenPipe — child may exit before reading all of stdin.
    let _ = child.stdin.take().unwrap().write_all(b"Hello!\n");
    let out = child.wait_with_output().unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Positive control: warning fired on stderr.
    assert!(
        stderr.contains("unknown lint rule") && stderr.contains("no-such-rule-xyzzy"),
        "AC-224-21 (stdin): warning must fire on stderr; got: {stderr}"
    );

    // AC-224-21: stdout must be valid JSON and contain no warning text.
    serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|e| {
        panic!("AC-224-21 (stdin): stdout must be valid JSON; error: {e}; got: {stdout}")
    });
    for needle in [
        "no-such-rule-xyzzy",
        "recognised rules are",
        "warning",
        "ignoring",
    ] {
        assert!(
            !stdout.contains(needle),
            "AC-224-21 (stdin): stdout must not contain {needle:?}; got: {stdout}"
        );
    }
}

/// AC-224-22: the pre-subcommand global form `mds --quiet lint <dir>` suppresses the
/// warning exactly like `mds lint --quiet <dir>`, and neither moves the exit code.
///
/// Paired positive control (PF-013): the same tree without `--quiet` DOES warn.
#[test]
fn unknown_rule_warning_suppressed_by_global_quiet_form() {
    let dir = tempfile::tempdir().unwrap();
    write_mixed_lint_tree(dir.path());
    write_rules_config(
        dir.path(),
        serde_json::json!({ "no-such-rule-xyzzy": "warn" }),
    );

    let loud = mds_bin()
        .arg("lint")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&loud.stderr).contains("unknown lint rule"),
        "positive control: the non-quiet run must warn"
    );

    for args in [vec!["lint", "--quiet"], vec!["--quiet", "lint"]] {
        let mut cmd = mds_bin();
        for a in &args {
            cmd.arg(a);
        }
        let out = cmd
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("unknown lint rule") && !stderr.contains("no-such-rule-xyzzy"),
            "AC-224-22: `mds {}` must suppress the unknown-rule warning; got: {stderr}",
            args.join(" ")
        );
        assert_eq!(
            out.status.code(),
            loud.status.code(),
            "AC-224-22: --quiet must not move the exit code (form: {})",
            args.join(" ")
        );
    }
}

/// AC-224-22 (single-file target): `--quiet` suppresses the unknown-rule warning when
/// targeting a single file.
///
/// `load_lint_config` (the `run_lint_file` / `run_lint_stdin` code path) has a quiet gate
/// inside `emit_unknown_rule_warning`; this test covers the single-file path independently
/// from the directory target covered by `unknown_rule_warning_suppressed_by_quiet`, which
/// exercises the `config_for` emitter instead.
///
/// Paired positive control (PF-013 / ADR-009): the same invocation WITHOUT `--quiet` DOES
/// emit the warning, so the absence assertion cannot pass vacuously.
#[test]
fn unknown_rule_warning_suppressed_by_quiet_single_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clean.mds");
    std::fs::write(&file, "Hello!\n").unwrap();
    write_unknown_rule_config(dir.path());

    // Positive control: without --quiet the warning fires on the single-file path.
    let loud = mds_bin()
        .arg("lint")
        .arg(&file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&loud.stderr).contains("unknown lint rule"),
        "positive control: single-file mode without --quiet must warn; got: {}",
        String::from_utf8_lossy(&loud.stderr)
    );

    // AC-224-22: --quiet must suppress the warning.
    let out = mds_bin()
        .arg("lint")
        .arg(&file)
        .arg("--quiet")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown lint rule") && !stderr.contains("no-such-rule-xyzzy"),
        "AC-224-22 (single-file): --quiet must suppress the unknown-rule warning; got: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        loud.status.code(),
        "AC-224-22 (single-file): --quiet must not move the exit code; got stderr: {stderr}"
    );
}

/// AC-224-22 (stdin target): `--quiet` suppresses the unknown-rule warning when reading
/// from stdin.
///
/// `load_lint_config` (the `run_lint_stdin` code path) has a quiet gate inside
/// `emit_unknown_rule_warning`; this test covers the stdin path independently from the
/// directory target covered by `unknown_rule_warning_suppressed_by_quiet`, which exercises
/// the `config_for` emitter instead.
///
/// `current_dir` is set to a tempdir containing `mds.json` so `run_lint_stdin` picks up
/// the unknown rule.  Paired positive control (PF-013 / ADR-009): the same invocation
/// WITHOUT `--quiet` DOES emit the warning.
#[test]
fn unknown_rule_warning_suppressed_by_quiet_stdin() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    write_unknown_rule_config(dir.path());

    // Positive control: without --quiet the warning fires on the stdin path.
    let mut loud_child = mds_bin()
        .arg("lint")
        .arg("-")
        .current_dir(dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Ignore BrokenPipe -- child may exit before reading all of stdin.
    let _ = loud_child.stdin.take().unwrap().write_all(b"Hello!\n");
    let loud = loud_child.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&loud.stderr).contains("unknown lint rule"),
        "positive control: stdin mode without --quiet must warn; got: {}",
        String::from_utf8_lossy(&loud.stderr)
    );

    // AC-224-22: --quiet must suppress the warning on the stdin path.
    let mut child = mds_bin()
        .arg("lint")
        .arg("-")
        .arg("--quiet")
        .current_dir(dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Ignore BrokenPipe -- child may exit before reading all of stdin.
    let _ = child.stdin.take().unwrap().write_all(b"Hello!\n");
    let out = child.wait_with_output().unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown lint rule") && !stderr.contains("no-such-rule-xyzzy"),
        "AC-224-22 (stdin): --quiet must suppress the unknown-rule warning; got: {stderr}"
    );
    assert_eq!(
        out.status.code(),
        loud.status.code(),
        "AC-224-22 (stdin): --quiet must not move the exit code; got stderr: {stderr}"
    );
}

/// AC-224-12 (NEGATIVE / fix behaviour unchanged): an unknown rule name changes nothing
/// about `--fix`, `--fix --check` or `--fix --diff`.
///
/// Two identical trees are built; only one names `no-such-rule-xyzzy` alongside the same
/// valid rules. For each of the three invocations the exit code, the post-run file bytes
/// and the stdout are compared between the trees.
///
/// Non-vacuity (PF-013): the run asserts a fix was ACTUALLY applied (the file bytes moved)
/// and that the warning fired in the unknown-rule tree — a parity assertion between two
/// trees where nothing happened would prove nothing.
#[test]
fn unknown_rule_does_not_change_fix_behaviour() {
    /// Build a tree with one auto-fixable (Tier A) duplicate-export violation.
    fn build_tree(rules: serde_json::Value) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("lint_error.mds");
        fs::copy(fixture("lint_error.mds"), &target).unwrap();
        write_rules_config(dir.path(), rules);
        (dir, target)
    }

    let valid_rules = serde_json::json!({ "duplicate-export": "error" });
    let mut with_rules = valid_rules.clone();
    with_rules["no-such-rule-xyzzy"] = serde_json::json!("warn");

    // ── --fix --check: must not write, must report pending fixes ────────────
    let (with_dir, with_target) = build_tree(with_rules.clone());
    let (without_dir, without_target) = build_tree(valid_rules.clone());
    let with_before = fs::read(&with_target).unwrap();

    let with_check = lint_path(with_dir.path(), &["--fix", "--check"]);
    let without_check = lint_path(without_dir.path(), &["--fix", "--check"]);
    assert!(
        String::from_utf8_lossy(&with_check.stderr).contains("unknown lint rule"),
        "non-vacuity: the unknown-rule tree must warn"
    );
    assert_eq!(
        with_check.status.code(),
        without_check.status.code(),
        "AC-224-12: --fix --check exit code must not move"
    );
    assert_eq!(
        fs::read(&with_target).unwrap(),
        with_before,
        "AC-224-12: --fix --check must not write"
    );

    // ── --fix --diff: identical diff output, still no write ─────────────────
    //
    // The `---`/`+++` headers name the absolute file path, which necessarily differs
    // between the two tempdirs. Normalise each tree's own root to a placeholder so the
    // comparison is about the diff CONTENT — the only thing an unknown rule name could
    // plausibly change — rather than about tempdir naming.
    let with_diff = lint_path(with_dir.path(), &["--fix", "--diff"]);
    let without_diff = lint_path(without_dir.path(), &["--fix", "--diff"]);
    let normalize = |bytes: &[u8], root: &std::path::Path| {
        String::from_utf8_lossy(bytes).replace(root.to_string_lossy().as_ref(), "<DIR>")
    };
    let with_diff_text = normalize(&with_diff.stdout, with_dir.path());
    let without_diff_text = normalize(&without_diff.stdout, without_dir.path());
    assert!(
        with_diff_text.contains("@export greet"),
        "non-vacuity: --fix --diff must actually render a hunk; got: {with_diff_text}"
    );
    assert_eq!(
        with_diff_text, without_diff_text,
        "AC-224-12: --fix --diff output must be identical once the tempdir root is normalised"
    );
    assert_eq!(
        with_diff.status.code(),
        without_diff.status.code(),
        "AC-224-12: --fix --diff exit code must not move"
    );
    assert_eq!(
        fs::read(&with_target).unwrap(),
        with_before,
        "AC-224-12: --fix --diff must not write"
    );

    // ── --fix: identical result bytes and exit code, and a fix really happened ──
    let with_fix = lint_path(with_dir.path(), &["--fix"]);
    let without_fix = lint_path(without_dir.path(), &["--fix"]);
    assert_eq!(
        with_fix.status.code(),
        without_fix.status.code(),
        "AC-224-12: --fix exit code must not move"
    );
    let with_after = fs::read(&with_target).unwrap();
    let without_after = fs::read(&without_target).unwrap();
    assert_eq!(
        with_after, without_after,
        "AC-224-12: the fixed file bytes must be identical with and without the unknown rule"
    );
    assert_ne!(
        with_after, with_before,
        "non-vacuity: a fix must actually have been applied, or the parity above is empty"
    );

    // No stray artefacts left behind in either tree.
    for dir in [with_dir.path(), without_dir.path()] {
        for entry in fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            assert!(
                name == "lint_error.mds" || name == "mds.json",
                "AC-224-12: --fix must leave no temp or backup artefact; found {name}"
            );
        }
    }
}

/// AC-224-11 (HUMAN format): stdout stays empty, and the warning is the ONLY delta on
/// stderr — every file is still linted and reported exactly as it was.
///
/// The JSON arm is pinned by `unknown_rule_json_stdout_is_byte_identical_to_run_without_it`.
/// Human format routes diagnostics through a completely different renderer
/// (`render_result_human` → `eprint_error` → miette, on stderr) and writes nothing to
/// stdout, so a JSON-only assertion proves nothing here — this arm has to be asserted
/// separately (PF-007 reasoning applied within one surface: two output formats are two
/// renderers).
///
/// The SAME tempdir is reused for both runs, with only `mds.json` rewritten between
/// them, so the display paths are byte-identical and the stderr comparison is about
/// content rather than tempdir naming.
///
/// Non-vacuity (PF-013 / ADR-009): the run asserts the warning IS present in the first
/// run and ABSENT in the second, and that the control run's stderr actually carries
/// per-file diagnostics — comparing two empty buffers would prove nothing.
#[test]
fn unknown_rule_human_stdout_empty_and_per_file_output_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    write_mixed_lint_tree(dir.path());

    let run = || {
        mds_bin()
            .arg("lint")
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap()
    };

    // Run 1: config names an unknown rule alongside a valid one.
    write_rules_config(
        dir.path(),
        serde_json::json!({ "unused-variable": "warn", "no-such-rule-xyzzy": "warn" }),
    );
    let with = run();
    // Run 2: identical tree and identical paths, unknown entry deleted.
    write_rules_config(dir.path(), serde_json::json!({ "unused-variable": "warn" }));
    let without = run();

    let with_stderr = String::from_utf8_lossy(&with.stderr);
    let without_stderr = String::from_utf8_lossy(&without.stderr);

    // Positive control: the warning fired in run 1 and not in run 2.
    assert!(
        with_stderr.contains("unknown lint rule") && with_stderr.contains("no-such-rule-xyzzy"),
        "non-vacuity: the unknown-rule warning must fire on stderr in human mode; \
         got: {with_stderr}"
    );
    assert!(
        !without_stderr.contains("unknown lint rule"),
        "control run must not warn; got: {without_stderr}"
    );
    // Non-vacuity: the control run really does carry per-file lint output, so the
    // equality assertion below is comparing real diagnostics, not two empty buffers.
    assert!(
        without_stderr.contains("unused-variable"),
        "non-vacuity: the fixture tree must produce per-file diagnostics on stderr; \
         got: {without_stderr}"
    );

    // AC-224-11: human format writes NOTHING to stdout, with or without the unknown rule.
    assert!(
        with.stdout.is_empty(),
        "AC-224-11: human-format stdout must be empty on the unknown-rule path; got: {}",
        String::from_utf8_lossy(&with.stdout)
    );
    assert!(
        without.stdout.is_empty(),
        "AC-224-11: human-format stdout must be empty in the control run; got: {}",
        String::from_utf8_lossy(&without.stdout)
    );

    // AC-224-11: the added warning line is the ONLY difference on stderr — no file is
    // skipped and no per-file result changes.
    let with_minus_warning: String = with_stderr
        .lines()
        .filter(|l| !l.contains("unknown lint rule"))
        .map(|l| format!("{l}\n"))
        .collect();
    let without_normalized: String = without_stderr.lines().map(|l| format!("{l}\n")).collect();
    assert_eq!(
        with_minus_warning, without_normalized,
        "AC-224-11: apart from the added warning line, human-mode stderr must be \
         identical with and without the unknown rule name;\nwith:\n{with_stderr}\n\
         without:\n{without_stderr}"
    );

    // AC-224-13: the exit code does not move because of the unknown rule name.
    assert_eq!(
        with.status.code(),
        without.status.code(),
        "AC-224-13: the exit code must not move because of an unknown rule name in human mode"
    );
}

/// AC-224-10 / AC-224-19 (nested config): the `mds.json` lives in a SUBDIRECTORY and the
/// lint root itself has none.
///
/// This is a distinct `LintDirCtx::config_for` branch from the other unknown-rule tests:
/// the root's `base_dir` resolves to `None` (default config, cached under `base_dir` only)
/// while the subdirectory resolves to `Some((config, config_dir))`. Both branches are
/// exercised in one invocation, so a regression that emitted the warning from the `None`
/// arm, or that failed to reach the `Some` arm at all, is caught here.
///
/// Non-vacuity (PF-013 / ADR-009): the run asserts the warning fires exactly once AND
/// that both files were actually linted (the root file is reported too), so neither the
/// count nor the JSON comparison can pass because nothing ran.
#[test]
fn unknown_rule_nested_config_warns_once_and_envelope_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    // Root file: governed by NO config (the lint root has no mds.json).
    std::fs::write(
        dir.path().join("root.mds"),
        "---\nroot_unused: v\n---\nHi!\n",
    )
    .unwrap();
    // Subdirectory file: governed by sub/mds.json, which names the unknown rule.
    std::fs::write(sub.join("nested.mds"), "---\nsub_unused: v\n---\nHo!\n").unwrap();

    let run = || {
        mds_bin()
            .arg("lint")
            .arg(dir.path())
            .arg("--format")
            .arg("json")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap()
    };

    write_rules_config(
        &sub,
        serde_json::json!({ "unused-variable": "warn", "no-such-rule-xyzzy": "warn" }),
    );
    let with = run();
    write_rules_config(&sub, serde_json::json!({ "unused-variable": "warn" }));
    let without = run();

    let with_stderr = String::from_utf8_lossy(&with.stderr);

    // AC-224-19: exactly one warning, emitted from the subdirectory's config only.
    let warning_count = with_stderr
        .lines()
        .filter(|l| l.contains("unknown lint rule"))
        .count();
    assert_eq!(
        warning_count, 1,
        "AC-224-19 (nested): the subdirectory config must warn exactly once; got stderr:\n\
         {with_stderr}"
    );
    assert!(
        with_stderr.contains("no-such-rule-xyzzy"),
        "non-vacuity: the warning must name the unknown rule; got: {with_stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&without.stderr).contains("unknown lint rule"),
        "control run must not warn; got: {}",
        String::from_utf8_lossy(&without.stderr)
    );

    // AC-224-10: the stdout envelope is byte-identical across the two runs.
    assert_eq!(
        with.stdout,
        without.stdout,
        "AC-224-10 (nested): stdout must be byte-identical with and without the unknown \
         rule name;\nwith:    {}\nwithout: {}",
        String::from_utf8_lossy(&with.stdout),
        String::from_utf8_lossy(&without.stdout)
    );
    assert_eq!(
        with.status.code(),
        without.status.code(),
        "AC-224-13 (nested): the exit code must not move"
    );

    // Non-vacuity: BOTH files were linted — the root file (default config, no warning)
    // and the nested file (subdir config). A byte-identical comparison of two envelopes
    // that never reached the nested directory would prove nothing.
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&with.stdout).trim())
            .expect("stdout must be valid JSON");
    let files: Vec<String> = parsed["files"]
        .as_array()
        .expect("files must be an array")
        .iter()
        .map(|f| f["file"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        files.iter().any(|f| f == "root.mds"),
        "non-vacuity: the root file must still be linted; got files: {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "sub/nested.mds"),
        "non-vacuity: the nested file must still be linted; got files: {files:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// #216 — CLI quiet-mode and directory-summary contract
// ═══════════════════════════════════════════════════════════════════════════════

// ── AC-Q06: clean directory prints exactly one summary line ──────────────────
//
// Verified: directory mode emits NO per-file `Clean:` line (those live at
// lint.rs::835 and ::881, single-file/stdin only).  After this change, the
// summary IS the only stderr line for a clean directory run, so we can assert
// exact string equality rather than substring containment (ADR-009).

#[test]
fn lint_directory_prints_summary_when_clean() {
    let dir = tempfile::tempdir().unwrap();
    for name in &["a.mds", "b.mds", "c.mds"] {
        fs::copy(fixture("lint_clean.mds"), dir.path().join(name)).unwrap();
    }

    let out = lint_path(dir.path(), &[]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "clean directory must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.is_empty(),
        "stdout must be empty in human mode; got: {stdout:?}"
    );

    // Exact equality — proved correct because dir mode never emits per-file Clean: lines.
    let stderr_trimmed = String::from_utf8_lossy(&out.stderr).trim().to_string();
    assert_eq!(
        stderr_trimmed, "3 clean, 0 with warnings, 0 with errors, 0 resource-limited",
        "AC-Q06: trimmed stderr must equal the exact summary string"
    );
}

// ── AC-Q07: mixed-severity tree yields exact per-bucket counts ────────────────
// AC-Q08: the four counters partition the file set (no drops, no double-counts).

#[test]
fn lint_directory_summary_counts_each_severity_bucket() {
    let dir = tempfile::tempdir().unwrap();
    // 2 clean, 1 warn-only (unused-variable), 1 error (duplicate-export)
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean1.mds")).unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean2.mds")).unwrap();
    fs::copy(fixture("lint_warn_only.mds"), dir.path().join("warn.mds")).unwrap();
    fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

    let out = lint_path(dir.path(), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(2),
        "directory with an error file must exit 2; stderr: {stderr}"
    );
    assert!(
        stderr.contains("2 clean, 1 with warnings, 1 with errors, 0 resource-limited"),
        "AC-Q07: summary must contain '2 clean, 1 with warnings, 1 with errors, 0 resource-limited'; \
         got: {stderr:?}"
    );

    // AC-Q08: parse the four integers and assert they partition the 4-file set.
    let counts = parse_summary_counts(&stderr);
    assert_eq!(
        counts,
        [2, 1, 1, 0],
        "AC-Q08: counters must partition the 4-file set (clean+warn+error+limit == 4)"
    );
}

/// Parse [clean, warn, error, limit] from a directory summary line.
/// Returns [0,0,0,0] on parse failure (test will then fail on the equality check).
fn parse_summary_counts(stderr: &str) -> [usize; 4] {
    // Pattern: "N clean, N with warnings, N with errors, N resource-limited"
    for line in stderr.lines() {
        if let Some(rest) = line.strip_suffix(" resource-limited") {
            let parts: Vec<&str> = rest.split(", ").collect();
            if parts.len() == 4 {
                let clean = parts[0]
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok());
                let warn = parts[1]
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok());
                let error = parts[2]
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok());
                let limit = parts[3]
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok());
                if let (Some(c), Some(w), Some(e), Some(l)) = (clean, warn, error, limit) {
                    return [c, w, e, l];
                }
            }
        }
    }
    [0, 0, 0, 0]
}

/// Build content that triggers `MdsError::ResourceLimit` (257 `@block` declarations;
/// `MAX_BLOCKS_PER_MODULE` is 256).  Shared by AC-Q09 and AC-Q12.
fn make_resource_limit_content() -> String {
    (0..257)
        .map(|i| format!("@block block_{i}:\n@end\n"))
        .collect()
}

// ── AC-Q09: resource-limited bucket is genuinely exercised ───────────────────
//
// `tally_from_result` can NEVER return FileTally::ResourceLimit — it maps exit codes.
// ResourceLimit arrives only via the mds::lint() Err arm (MdsError::ResourceLimit).
// This test proves the fourth counter is not a permanently-zero constant.

#[test]
fn lint_directory_summary_counts_resource_limited() {
    let dir = tempfile::tempdir().unwrap();

    // One clean file and one that triggers ResourceLimit.
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean.mds")).unwrap();
    fs::write(
        dir.path().join("too_many_blocks.mds"),
        make_resource_limit_content(),
    )
    .unwrap();

    let out = lint_path(dir.path(), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(3),
        "directory with a resource-limited file must exit 3; stderr: {stderr}"
    );
    assert!(
        stderr.contains("1 clean, 0 with warnings, 0 with errors, 1 resource-limited"),
        "AC-Q09: summary must show '1 clean, 0 with warnings, 0 with errors, 1 resource-limited'; \
         got: {stderr:?}"
    );

    // AC-Q08 invariant: 1+0+0+1 == 2 files
    let counts = parse_summary_counts(&stderr);
    assert_eq!(
        counts.iter().sum::<usize>(),
        2,
        "AC-Q08: counters must partition the 2-file set"
    );
}

// ── AC-Q10: quiet warn-only lint is silent (with positive control) ────────────
//
// PF-013/ADR-009: absence assertion + paired positive control.
// D1-a decision: warn-only --quiet exits 1 with no output (mirrors fmt precedent).

#[test]
fn lint_directory_quiet_suppresses_summary_when_only_warnings() {
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_warn_only.mds"), dir.path().join("a.mds")).unwrap();
    fs::copy(fixture("lint_warn_only.mds"), dir.path().join("b.mds")).unwrap();

    let quiet = lint_path(dir.path(), &["--quiet"]);
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);

    assert_eq!(
        quiet.status.code(),
        Some(1),
        "warn-only directory under --quiet must exit 1; stderr: {quiet_stderr:?}"
    );
    assert!(
        !quiet_stderr.contains("clean,"),
        "AC-Q10: --quiet warn-only must NOT emit the summary; got: {quiet_stderr:?}"
    );

    // Positive control: same tree without --quiet MUST emit the summary.
    let control = lint_path(dir.path(), &[]);
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    assert!(
        control_stderr.contains("clean,"),
        "positive control: non-quiet run must emit the summary so the quiet absence \
         assertion is non-vacuous; got: {control_stderr:?}"
    );
}

// ── AC-Q11: quiet lint with error-severity findings still prints summary ──────
//
// ADR-009/PF-013: both directions of the quiet gate are pinned in one test.
// A warn-only file (lint_warn_only.mds) is included so the test can assert:
//   - error-severity diagnostic body DOES appear under --quiet
//   - warn-severity diagnostic body does NOT appear under --quiet
// Without these body assertions the test could pass under the mutation
// `if quiet` (dropping the Severity filter), which would silence all diagnostic
// bodies while still printing the summary via the error_file_count > 0 gate.

#[test]
fn lint_directory_quiet_still_prints_summary_on_errors() {
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean.mds")).unwrap();
    fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();
    // Add a warn-severity file so both directions of the quiet gate can be pinned:
    // error diagnostics must survive --quiet; warn diagnostics must be suppressed.
    fs::copy(fixture("lint_warn_only.mds"), dir.path().join("warn.mds")).unwrap();

    let out = lint_path(dir.path(), &["--quiet"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(2),
        "directory with error file under --quiet must exit 2; stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("1 clean, 1 with warnings, 1 with errors, 0 resource-limited"),
        "AC-Q11: --quiet with error files must still print summary; got: {stderr:?}"
    );
    // Error-severity diagnostic body must not be silenced by --quiet.
    // (render_diag_human suppresses only Info|Warn, never Error.)
    assert!(
        stderr.contains("duplicate-export"),
        "AC-Q11: error-severity diagnostic body must survive --quiet; got: {stderr:?}"
    );
    // Warn-severity diagnostic body must be suppressed.
    // Positive control: non-quiet run must show unused-variable to prove this is
    // non-vacuous (PF-013 / ADR-009).
    let control = lint_path(dir.path(), &[]);
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    assert!(
        control_stderr.contains("unused-variable"),
        "positive control: non-quiet run must print 'unused-variable' warn diagnostic \
         so the absence assertion below is non-vacuous; got: {control_stderr:?}"
    );
    assert!(
        !stderr.contains("unused-variable"),
        "AC-Q11: warn-severity 'unused-variable' diagnostic body must be suppressed \
         under --quiet; got: {stderr:?}"
    );
}

// ── AC-Q12: quiet lint with resource-limit still prints summary ───────────────

#[test]
fn lint_directory_quiet_still_prints_summary_on_resource_limit() {
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean.mds")).unwrap();
    fs::write(
        dir.path().join("too_many_blocks.mds"),
        make_resource_limit_content(),
    )
    .unwrap();

    let out = lint_path(dir.path(), &["--quiet"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(3),
        "directory with resource-limited file under --quiet must exit 3; stderr: {stderr:?}"
    );
    // "1 resource-limited" implies "resource-limited" — one assertion covers both.
    assert!(
        stderr.contains("1 resource-limited"),
        "AC-Q12: --quiet with resource-limited file must still print summary with count; got: {stderr:?}"
    );
}

// ── AC-Q13: --fix --check exit path does not skip the summary ─────────────────
//
// AD-216-7: summary is emitted BEFORE the --fix --check process::exit(1).

#[test]
fn lint_directory_check_exit_still_prints_summary() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("fix_me.mds");
    // lint_partial_fix.mds has auto-fixable issues (Tier A empty-block + duplicate-export).
    fs::copy(fixture("lint_partial_fix.mds"), &target).unwrap();

    let content_before = fs::read_to_string(&target).unwrap();
    let out = lint_path(dir.path(), &["--fix", "--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check with pending fixes must exit 1; stderr: {stderr}"
    );
    // Assert the WHOLE summary line, not a disjunction of two weak substrings: a
    // disjunction would still pass if the line were emitted in a corrupted form.
    let counts = parse_summary_counts(&stderr);
    assert_eq!(
        counts.iter().sum::<usize>(),
        1,
        "AC-Q08/AC-Q13: the summary must be emitted on the --fix --check exit path and its \
         four counters must partition the 1-file tree; got stderr: {stderr:?}"
    );
    // --check must never write (file byte-unchanged).
    let content_after = fs::read_to_string(&target).unwrap();
    assert_eq!(
        content_before, content_after,
        "--fix --check must never modify files"
    );
}

// ── AC-Q14: D1-a contract: warn-only and --fix --check --quiet are silent ────
//
// D1-a decision (mirrors fmt.rs precedent): warnings are status output (main.rs:30)
// and are suppressed under --quiet.  Exit codes are unaffected (AD-216-2).
// This test pins the chosen behaviour so a future plan change is visible as a test failure.

#[test]
fn lint_directory_quiet_check_exit_matches_d1a_contract() {
    // Uses lint_fixable_warn.mds: contains a single `{name}` legacy-interpolation
    // occurrence that triggers `legacy-interpolation` (Severity::Warn, auto-fixable).
    // No error-severity findings, so D1-a applies: summary suppressed under --quiet.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("fix_me.mds");
    fs::copy(fixture("lint_fixable_warn.mds"), &target).unwrap();

    // --fix --check --quiet on a tree with pending fixes.
    // D1-a: silent (warn-only pending fix → status output → suppressed under --quiet).
    let out = lint_path(dir.path(), &["--fix", "--check", "--quiet"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "--fix --check --quiet with pending fixes must still exit 1 (exit codes unaffected); \
         stderr: {stderr:?}"
    );
    // The residual tally is WarnOnly (legacy-interpolation is Severity::Warn).
    // D1-a gate: !quiet || error_file_count > 0 || limit_file_count > 0 = false → no summary.
    assert!(
        !stderr.contains("clean,"),
        "D1-a: --fix --check --quiet with warn-only residual must not print summary; \
         got: {stderr:?}"
    );
    // 'Would fix:' is a status message (lint.rs: `if check && !quiet`) and must also
    // be suppressed under --quiet, distinguishing the --fix --check pending-fix code
    // path from the ordinary warn-only exit 1 (lint.rs severity exit).  Without this
    // assertion the test cannot tell those two paths apart if legacy-interpolation ever
    // loses auto-fixability or the fixture drifts.
    assert!(
        !stderr.contains("Would fix:"),
        "D1-a: --fix --check --quiet must not print 'Would fix:'; got: {stderr:?}"
    );

    // PF-013 / ADR-009 — PAIRED POSITIVE CONTROL, in this test, on THIS tree.
    //
    // Without it the absence assertion above passes vacuously: deleting the summary
    // emit outright leaves this test green (verified by mutation — removing the
    // `eprintln!` in `run_lint_directory` failed 8 of the #216 tests but NOT this one).
    // A cross-test control in `lint_directory_check_exit_still_prints_summary` does not
    // count: it uses a different fixture, so it cannot prove that THIS tree's summary is
    // suppressed by `--quiet` rather than never produced at all.
    let control = lint_path(dir.path(), &["--fix", "--check"]);
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    assert_eq!(
        control.status.code(),
        Some(1),
        "positive control: the same tree without --quiet must also exit 1; \
         got stderr: {control_stderr:?}"
    );
    assert!(
        control_stderr.contains("clean,"),
        "positive control: the same tree WITHOUT --quiet must print the summary, proving the \
         --quiet absence assertion above is non-vacuous; got: {control_stderr:?}"
    );
    // Paired positive control for the 'Would fix:' absence assertion: confirm the
    // --fix --check path really was engaged (i.e. the fixture is still auto-fixable).
    assert!(
        control_stderr.contains("Would fix:"),
        "positive control: --fix --check without --quiet must print 'Would fix:', confirming \
         the test exercises the --fix --check code path and not just an ordinary warn exit; \
         got: {control_stderr:?}"
    );
}

// ── AC-Q15: JSON directory mode keeps stdout a single clean JSON object ────────
//
// The summary MUST appear on stderr and stdout MUST NOT contain the summary text.
// Positive control: stderr DOES contain the summary (so the stdout-absence check
// can't pass vacuously on a broken empty-stdout test).

#[test]
fn lint_directory_json_summary_goes_to_stderr_only() {
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean.mds")).unwrap();
    fs::copy(fixture("lint_warn_only.mds"), dir.path().join("warn.mds")).unwrap();

    let out = lint_path(dir.path(), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Stdout must be a single parseable JSON object.
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_ok(),
        "AC-Q15: stdout must be a parseable JSON object; got: {stdout:?}"
    );
    // Stdout must NOT contain the summary text.
    assert!(
        !stdout.contains("clean,"),
        "AC-Q15: stdout must NOT contain the summary text in JSON mode; got: {stdout:?}"
    );
    // Positive control: stderr DOES contain the summary.
    assert!(
        stderr.contains("clean,"),
        "AC-Q15 positive control: stderr must contain the summary; got: {stderr:?}"
    );
}

// ── AC-Q16: JSON envelope key contract is unchanged (D2-B: freeze the envelope) ─
//
// D2-B decision: no `summary` key is added to the JSON stdout.  The envelope
// remains {"files":…,"truncated":…,"version":1}.

#[test]
fn lint_directory_json_envelope_key_contract_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("a.mds")).unwrap();
    fs::copy(fixture("lint_warn_only.mds"), dir.path().join("b.mds")).unwrap();

    let out = lint_path(dir.path(), &["--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");

    let keys: Vec<&str> = json
        .as_object()
        .expect("top-level must be an object")
        .keys()
        .map(|k| k.as_str())
        .collect();

    // D2-B: envelope is exactly {files, truncated, version} — no summary key.
    assert_eq!(
        keys,
        vec!["files", "truncated", "version"],
        "AC-Q16 (D2-B): JSON envelope keys must remain exactly [files, truncated, version]; \
         got: {keys:?}"
    );
    assert_eq!(
        json["version"], 1,
        "AC-Q16: version must remain the integer 1"
    );
}

// ── AC-Q18: single-file lint prints no directory summary ──────────────────────

#[test]
fn lint_single_file_prints_no_directory_summary() {
    let out = lint_path(&fixture("lint_clean.mds"), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "clean single file must exit 0");
    assert!(
        stderr.contains("Clean:"),
        "single-file clean lint must still print 'Clean:' line; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("clean,"),
        "AC-Q18: single-file lint must NOT print a directory summary; got: {stderr:?}"
    );

    // PF-013 / ADR-009: the absence assertion above is only meaningful if "clean," is
    // the exact substring a real directory summary contains.  A typo in the needle would
    // make the assertion pass on any output at all.  Prove the needle by running the same
    // source through directory mode, where the summary IS expected.
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("a.mds")).unwrap();
    let control = lint_path(dir.path(), &[]);
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("clean,"),
        "positive control: the same source in directory mode must produce a summary \
         containing the exact needle \"clean,\"; got: {:?}",
        String::from_utf8_lossy(&control.stderr)
    );
}

// ── AC-Q19: stdin lint prints no directory summary ────────────────────────────

#[test]
fn lint_stdin_prints_no_directory_summary() {
    let content = fs::read_to_string(fixture("lint_clean.mds")).unwrap();
    let out = lint_stdin(&content, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(0),
        "clean stdin lint must exit 0; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("clean,"),
        "AC-Q19: stdin lint must NOT print a directory summary; got: {stderr:?}"
    );

    // PF-013 / ADR-009: the absence assertion above is only meaningful if `"clean,"` is
    // the string a real summary would actually contain. A typo in the needle would make
    // it pass on any output at all. Prove the needle by feeding the SAME source through
    // directory mode, where the summary IS expected.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.mds"), &content).unwrap();
    let control = lint_path(dir.path(), &[]);
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("clean,"),
        "positive control: the same source in directory mode must produce a summary \
         containing the exact needle \"clean,\"; got: {:?}",
        String::from_utf8_lossy(&control.stderr)
    );
}

// ── D4 cross-mode parity: status messages honour --quiet in ALL four input modes ──
//
// PF-004 — an alternate output path can silently bypass a guard. Directory mode
// has TWO separate emitters (lint_one_file_human for --format human, and
// lint_one_file_accumulating for --format json), plus single-file mode and stdin mode.
// That is eight `fix rejected:` call sites in lint.rs (cited by function + match arm
// so the reference survives future line-number shifts):
//   run_lint_stdin             apply  — FixFileOutcome::Rejected arm
//   run_lint_stdin             preview — PreviewOutcome::Rejected arm
//   run_lint_file              apply  — FixFileOutcome::Rejected arm
//   run_lint_file              preview — PreviewOutcome::Rejected arm
//   lint_one_file_accumulating apply  — FixFileOutcome::Rejected arm
//   lint_one_file_accumulating preview — PreviewOutcome::Rejected arm
//   lint_one_file_human        apply  — FixFileOutcome::Rejected arm
//   lint_one_file_human        preview — PreviewOutcome::Rejected arm
// This test covers all four modes, each with its own paired positive control, so no
// assertion can pass because the fix was never attempted rather than suppressed.

/// Source with a single Tier A fix that the reverify gate rejects whole-file:
/// removing the empty `@define` would orphan the `@export`, so `FixFileOutcome`
/// is `Rejected` (not `PartiallyFixed`) and `fix rejected:` is emitted.
const FIX_REJECTED_SOURCE: &str = "\
@define empty_fn():

@end

@export empty_fn
";

#[test]
fn fix_rejected_message_honours_quiet_in_all_four_modes() {
    let needle = "fix rejected";

    // ── Mode 1: single file ──────────────────────────────────────────────────
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("r.mds");
    fs::write(&target, FIX_REJECTED_SOURCE).unwrap();

    let loud = lint_path(&target, &["--fix"]);
    let loud_stderr = String::from_utf8_lossy(&loud.stderr);
    assert!(
        loud_stderr.contains(needle),
        "positive control (single file): without --quiet the rejection MUST be reported, \
         otherwise the quiet assertion below is vacuous; got: {loud_stderr:?}"
    );

    let quiet = lint_path(&target, &["--fix", "--quiet"]);
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        !quiet_stderr.contains(needle),
        "single-file --fix --quiet must suppress 'fix rejected:' (D4/PF-004); got: {quiet_stderr:?}"
    );
    assert_eq!(
        quiet.status.code(),
        loud.status.code(),
        "single file: --quiet must not move the exit code"
    );

    // ── Mode 2: stdin ────────────────────────────────────────────────────────
    let loud_stdin = lint_stdin(FIX_REJECTED_SOURCE, &["--fix"]);
    let loud_stdin_stderr = String::from_utf8_lossy(&loud_stdin.stderr);
    assert!(
        loud_stdin_stderr.contains(needle),
        "positive control (stdin): without --quiet the rejection MUST be reported; \
         got: {loud_stdin_stderr:?}"
    );

    let quiet_stdin = lint_stdin(FIX_REJECTED_SOURCE, &["--fix", "--quiet"]);
    let quiet_stdin_stderr = String::from_utf8_lossy(&quiet_stdin.stderr);
    assert!(
        !quiet_stdin_stderr.contains(needle),
        "stdin --fix --quiet must suppress 'fix rejected:' (D4/PF-004); got: {quiet_stdin_stderr:?}"
    );
    assert_eq!(
        quiet_stdin.status.code(),
        loud_stdin.status.code(),
        "stdin: --quiet must not move the exit code"
    );

    // ── Mode 3: directory ────────────────────────────────────────────────────
    let loud_dir = lint_path(dir.path(), &["--fix"]);
    let loud_dir_stderr = String::from_utf8_lossy(&loud_dir.stderr);
    assert!(
        loud_dir_stderr.contains(needle),
        "positive control (directory): without --quiet the rejection MUST be reported; \
         got: {loud_dir_stderr:?}"
    );

    let quiet_dir = lint_path(dir.path(), &["--fix", "--quiet"]);
    let quiet_dir_stderr = String::from_utf8_lossy(&quiet_dir.stderr);
    assert!(
        !quiet_dir_stderr.contains(needle),
        "directory --fix --quiet must suppress 'fix rejected:' (D4/PF-004); got: {quiet_dir_stderr:?}"
    );
    assert_eq!(
        quiet_dir.status.code(),
        loud_dir.status.code(),
        "directory: --quiet must not move the exit code"
    );

    // ── Mode 4: directory, --format json (lint_one_file_accumulating) ───────────
    // PF-004: --format json routes directory files through lint_one_file_accumulating,
    // a separate emitter from lint_one_file_human (Mode 3).  This covers the two
    // gated sites in that emitter:
    //   lint_one_file_accumulating apply  — FixFileOutcome::Rejected arm  (--fix)
    //   lint_one_file_accumulating preview — PreviewOutcome::Rejected arm (--fix --check)

    // Apply path (lint_one_file_accumulating, FixFileOutcome::Rejected): --fix --format json.
    let loud_json = lint_path(dir.path(), &["--fix", "--format", "json"]);
    let loud_json_stderr = String::from_utf8_lossy(&loud_json.stderr);
    assert!(
        loud_json_stderr.contains(needle),
        "positive control (dir --fix --format json): without --quiet the rejection MUST be \
         reported in stderr; got: {loud_json_stderr:?}"
    );

    let quiet_json = lint_path(dir.path(), &["--fix", "--format", "json", "--quiet"]);
    let quiet_json_stderr = String::from_utf8_lossy(&quiet_json.stderr);
    assert!(
        !quiet_json_stderr.contains(needle),
        "dir --fix --format json --quiet must suppress 'fix rejected:' \
         (D4/PF-004 lint_one_file_accumulating FixFileOutcome::Rejected); \
         got: {quiet_json_stderr:?}"
    );
    assert_eq!(
        quiet_json.status.code(),
        loud_json.status.code(),
        "dir --format json --fix: --quiet must not move the exit code"
    );

    // Preview path (lint_one_file_accumulating, PreviewOutcome::Rejected): --fix --check --format json.
    let loud_json_check = lint_path(dir.path(), &["--fix", "--check", "--format", "json"]);
    let loud_json_check_stderr = String::from_utf8_lossy(&loud_json_check.stderr);
    assert!(
        loud_json_check_stderr.contains(needle),
        "positive control (dir --fix --check --format json): without --quiet the rejection MUST \
         be reported in stderr; got: {loud_json_check_stderr:?}"
    );

    let quiet_json_check =
        lint_path(dir.path(), &["--fix", "--check", "--format", "json", "--quiet"]);
    let quiet_json_check_stderr = String::from_utf8_lossy(&quiet_json_check.stderr);
    assert!(
        !quiet_json_check_stderr.contains(needle),
        "dir --fix --check --format json --quiet must suppress 'fix rejected:' \
         (D4/PF-004 lint_one_file_accumulating PreviewOutcome::Rejected); \
         got: {quiet_json_check_stderr:?}"
    );
    assert_eq!(
        quiet_json_check.status.code(),
        loud_json_check.status.code(),
        "dir --format json --fix --check: --quiet must not move the exit code"
    );

    // The rejection leaves the file unmodified in every mode.
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        FIX_REJECTED_SOURCE,
        "a rejected fix must never rewrite the source file"
    );
}

// ── D4 cross-mode parity: diagnostic-cap notice honours --quiet in all four modes ──
//
// PF-004: the cap notice is emitted from four separate call sites:
//   run_lint_stdin          (stdin mode)
//   run_lint_file           (single-file mode)
//   lint_one_file_human     (directory --format human, the default)
//   lint_one_file_accumulating (directory --format json)
// Each mode must gate on !quiet independently — PF-004 (avoids #43/#173 divergence class).
// PF-013 / ADR-009: each mode carries a paired positive control so the quiet
// assertion cannot pass vacuously on an uncovered path.
//
// Source: 1001 legacy-interpolation instances (one per line) exceed MAX_DIAGNOSTICS
// (1000) so LintResult.truncated = true.  --fix --check is used so no file is
// modified between the positive-control run and the quiet run.

/// Build a source that triggers exactly 1001 legacy-interpolation diagnostics
/// (one `{var_N}` per line).  1001 > MAX_DIAGNOSTICS (1000) forces truncation.
fn make_cap_source() -> String {
    (0..=1000)
        .map(|i| format!("{{var_{i}}}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The diagnostic-cap notice is suppressed by `--quiet` in all four input modes.
///
/// D4 (AD-216-11): the cap notice is a *status* message — gated on `!quiet`
/// at every emitter.  PF-004: directory-human, directory-JSON, single-file, and stdin
/// are SEPARATE emitters; a gate on one is not inherited by the others
/// (the #43/#173 divergence class).
#[test]
fn cap_notice_honours_quiet_in_all_four_modes() {
    let needle = "diagnostic cap";
    let source = make_cap_source();

    // ── Mode 1: single file ──────────────────────────────────────────────────
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cap.mds");
    fs::write(&target, &source).unwrap();

    // Positive control: without --quiet the cap notice MUST appear.
    let loud = lint_path(&target, &["--fix", "--check"]);
    let loud_stderr = String::from_utf8_lossy(&loud.stderr);
    assert!(
        loud_stderr.contains(needle),
        "positive control (single file): without --quiet the cap notice MUST be reported \
         (otherwise the quiet assertion is vacuous; ADR-009); got: {loud_stderr:?}"
    );

    let quiet = lint_path(&target, &["--fix", "--check", "--quiet"]);
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        !quiet_stderr.contains(needle),
        "single-file --fix --check --quiet must suppress the cap notice (D4/PF-004); \
         got: {quiet_stderr:?}"
    );
    assert_eq!(
        quiet.status.code(),
        loud.status.code(),
        "single file: --quiet must not move the exit code"
    );

    // ── Mode 2: stdin ────────────────────────────────────────────────────────
    let loud_stdin = lint_stdin(&source, &["--fix", "--check"]);
    let loud_stdin_stderr = String::from_utf8_lossy(&loud_stdin.stderr);
    assert!(
        loud_stdin_stderr.contains(needle),
        "positive control (stdin): without --quiet the cap notice MUST be reported; \
         got: {loud_stdin_stderr:?}"
    );

    let quiet_stdin = lint_stdin(&source, &["--fix", "--check", "--quiet"]);
    let quiet_stdin_stderr = String::from_utf8_lossy(&quiet_stdin.stderr);
    assert!(
        !quiet_stdin_stderr.contains(needle),
        "stdin --fix --check --quiet must suppress the cap notice (D4/PF-004); \
         got: {quiet_stdin_stderr:?}"
    );
    assert_eq!(
        quiet_stdin.status.code(),
        loud_stdin.status.code(),
        "stdin: --quiet must not move the exit code"
    );

    // ── Mode 3: directory ────────────────────────────────────────────────────
    let loud_dir = lint_path(dir.path(), &["--fix", "--check"]);
    let loud_dir_stderr = String::from_utf8_lossy(&loud_dir.stderr);
    assert!(
        loud_dir_stderr.contains(needle),
        "positive control (directory): without --quiet the cap notice MUST be reported; \
         got: {loud_dir_stderr:?}"
    );

    let quiet_dir = lint_path(dir.path(), &["--fix", "--check", "--quiet"]);
    let quiet_dir_stderr = String::from_utf8_lossy(&quiet_dir.stderr);
    assert!(
        !quiet_dir_stderr.contains(needle),
        "directory --fix --check --quiet must suppress the cap notice (D4/PF-004); \
         got: {quiet_dir_stderr:?}"
    );
    assert_eq!(
        quiet_dir.status.code(),
        loud_dir.status.code(),
        "directory: --quiet must not move the exit code"
    );

    // ── Mode 4: directory, --format json (lint_one_file_accumulating) ───────────
    // PF-004: --format json routes directory files through lint_one_file_accumulating,
    // a SEPARATE emitter from lint_one_file_human (Mode 3).  The cap-notice gate
    // (`if fix && !quiet`) inside lint_one_file_accumulating must also be tested;
    // a regression that leaves it unguarded would pass Modes 1–3 and escape CI.
    // The same temp `dir` / `cap.mds` file is reused (not modified by --fix --check).

    // Positive control: without --quiet the cap notice MUST appear.
    let loud_json = lint_path(dir.path(), &["--fix", "--check", "--format", "json"]);
    let loud_json_stderr = String::from_utf8_lossy(&loud_json.stderr);
    assert!(
        loud_json_stderr.contains(needle),
        "positive control (dir --format json): without --quiet the cap notice MUST be reported \
         (otherwise the quiet assertion is vacuous; ADR-009); got: {loud_json_stderr:?}"
    );

    let quiet_json = lint_path(
        dir.path(),
        &["--fix", "--check", "--format", "json", "--quiet"],
    );
    let quiet_json_stderr = String::from_utf8_lossy(&quiet_json.stderr);
    assert!(
        !quiet_json_stderr.contains(needle),
        "directory --fix --check --format json --quiet must suppress the cap notice \
         (D4/PF-004 lint_one_file_accumulating cap-notice gate); got: {quiet_json_stderr:?}"
    );
    assert_eq!(
        quiet_json.status.code(),
        loud_json.status.code(),
        "dir --format json: --quiet must not move the exit code"
    );
}

// ── AC-Q20: bare `mds lint` cannot reach directory mode ───────────────────────
//
// `auto_detect_mds_file` filters on path.is_file(), so a directory auto-detect
// never resolves to a directory.  Pin this so a future auto-detect change that
// starts returning `.` cannot silently attach a summary to the most common invocation.

#[test]
fn bare_lint_never_enters_directory_mode() {
    // Single-file case: bare `mds lint` auto-detects the one file.
    let dir1 = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir1.path().join("template.mds")).unwrap();

    let out1 = mds_bin()
        .arg("lint")
        .current_dir(dir1.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr1 = String::from_utf8_lossy(&out1.stderr);

    assert_eq!(
        out1.status.code(),
        Some(0),
        "bare lint in single-file dir must exit 0; stderr: {stderr1}"
    );
    assert!(
        stderr1.contains("Clean:"),
        "bare lint must print 'Clean:' (single-file); got: {stderr1:?}"
    );
    assert!(
        !stderr1.contains("clean,"),
        "AC-Q20: bare lint must NOT enter directory mode; got: {stderr1:?}"
    );

    // Two-file case: bare `mds lint` must error with "multiple .mds files found".
    let dir2 = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir2.path().join("a.mds")).unwrap();
    fs::copy(fixture("lint_clean.mds"), dir2.path().join("b.mds")).unwrap();

    let out2 = mds_bin()
        .arg("lint")
        .current_dir(dir2.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let stderr2 = String::from_utf8_lossy(&out2.stderr);

    assert!(
        !out2.status.success(),
        "bare lint with two files must fail; stderr: {stderr2}"
    );
    assert!(
        !stderr2.contains("clean,"),
        "AC-Q20: bare lint with two files must NOT emit a directory summary; got: {stderr2:?}"
    );

    // PF-013 / ADR-009: prove "clean," is the exact substring a real directory summary
    // contains, so both absence assertions above are not vacuous.
    let control_dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), control_dir.path().join("a.mds")).unwrap();
    let control = lint_path(control_dir.path(), &[]);
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("clean,"),
        "positive control: a clean file in directory mode must produce a summary \
         containing the exact needle \"clean,\"; got: {:?}",
        String::from_utf8_lossy(&control.stderr)
    );
}

// ── AC-Q21: empty and all-excluded directory print no summary ─────────────────

#[test]
fn lint_directory_empty_prints_no_summary() {
    // Empty directory.
    let dir = tempfile::tempdir().unwrap();

    let non_quiet = lint_path(dir.path(), &[]);
    let stderr_nq = String::from_utf8_lossy(&non_quiet.stderr);

    assert_eq!(
        non_quiet.status.code(),
        Some(0),
        "empty directory lint must exit 0; stderr: {stderr_nq}"
    );
    assert!(
        stderr_nq.contains("No .mds files found"),
        "empty directory must print 'No .mds files found'; got: {stderr_nq:?}"
    );
    assert!(
        !stderr_nq.contains("clean,"),
        "AC-Q21: empty directory must NOT print a summary; got: {stderr_nq:?}"
    );

    let quiet = lint_path(dir.path(), &["--quiet"]);
    let stderr_q = String::from_utf8_lossy(&quiet.stderr);

    assert_eq!(
        quiet.status.code(),
        Some(0),
        "empty directory lint --quiet must exit 0; stderr: {stderr_q}"
    );
    assert!(
        stderr_q.is_empty(),
        "AC-Q21: empty directory under --quiet must produce no stderr; got: {stderr_q:?}"
    );
}

// ── AC-Q22: all-excluded quiet still shouts, and gains no summary ─────────────
//
// Runs the existing dir_lint_all_excluded_quiet_still_emits_diagnostic scenario
// and adds the no-summary assertion.

#[test]
fn lint_directory_all_excluded_quiet_no_summary() {
    let dir = tempfile::tempdir().unwrap();
    let hidden = dir.path().join(".github");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("template.mds"), "Hello!\n").unwrap();

    let out = lint_path(dir.path(), &["--quiet"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "all-excluded under --quiet must exit non-zero; stderr: {stderr}"
    );
    assert!(
        stderr.contains("excluded") || stderr.contains("default-excluded"),
        "AC-Q22: all-excluded diagnostic must appear under --quiet; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("clean,"),
        "AC-Q22: all-excluded must NOT emit a summary line; got: {stderr:?}"
    );

    // PF-013 / ADR-009: prove "clean," is the exact substring a real directory summary
    // contains.  This scenario fires the all-excluded early return, so no summary is
    // emitted even without --quiet.  The positive control must therefore use a separate
    // directory that reaches the full summary path.
    let control_dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), control_dir.path().join("a.mds")).unwrap();
    let control = lint_path(control_dir.path(), &[]);
    assert!(
        String::from_utf8_lossy(&control.stderr).contains("clean,"),
        "positive control: a clean file in directory mode must produce a summary \
         containing the exact needle \"clean,\"; got: {:?}",
        String::from_utf8_lossy(&control.stderr)
    );
}

// ── AC-Q25: hostile filename cannot forge a summary line ─────────────────────
//
// PF-018: construct the hostile byte at RUNTIME, never as a source literal.
// PF-013/ADR-009: positive control — assert the escaped form IS in stderr,
// proving the hostile filename was reached and neutralised.

#[cfg(unix)]
#[test]
fn lint_directory_summary_is_not_forgeable() {
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();

    // PF-018: construct the ESC byte (0x1b) at runtime, never as a source literal.
    let esc_byte: u8 = 0x1b;
    // Build the hostile filename with an embedded ESC byte.
    // If an attacker could control a filename, they could try to embed
    // "3 clean, 0 with warnings, 0 with errors, 0 resource-limited" to forge a summary.
    let forged_suffix = b"3 clean, 0 with warnings, 0 with errors, 0 resource-limited";
    let mut hostile_name_bytes = vec![esc_byte];
    hostile_name_bytes.extend_from_slice(forged_suffix);
    hostile_name_bytes.extend_from_slice(b".mds");
    let hostile_os = std::ffi::OsStr::from_bytes(&hostile_name_bytes);
    let hostile_path = dir.path().join(hostile_os);

    // Use error-severity content so the file is lint-processed and its path
    // appears in the output (duplicate-export from lint_error.mds content).
    let error_content = fs::read(fixture("lint_error.mds")).unwrap();
    fs::write(&hostile_path, &error_content).unwrap();

    let out = lint_path(dir.path(), &[]);
    let stderr_bytes = &out.stderr;
    let stderr = String::from_utf8_lossy(stderr_bytes);

    // Primary assertion: no raw 0x1b (ESC) byte in stderr.
    assert!(
        !stderr_bytes.contains(&esc_byte),
        "AC-Q25: raw ESC byte must not appear in stderr; got: {stderr:?}"
    );

    // Positive control: the escaped form of 0x1b MUST appear in stderr,
    // proving the hostile filename was reached and processed, not silently skipped.
    // The sanitizer emits control bytes as \uXXXX with UPPERCASE hex digits (e.g. \u001B).
    // Check both cases defensively; the canonical form confirmed in error_tests.rs is \u001B.
    assert!(
        stderr.contains("\\u001B")
            || stderr.contains("\\u001b")
            || stderr.contains("\\x1B")
            || stderr.contains("\\x1b"),
        "AC-Q25 positive control: escaped form of ESC must appear in stderr, \
         confirming the hostile file was processed; got: {stderr:?}"
    );

    // The forged summary text must NOT appear as a standalone summary line.
    // The genuine summary will have a different count (1 with errors, not 3 clean).
    // Note: the hostile FILENAME embeds the forged text so it DOES appear inside the
    // diagnostic frame line ("╭─[\u001B3 clean...mds:6:1]").  We must check for an
    // exact line match, not a substring match, to distinguish the two cases.
    let forged_line = "3 clean, 0 with warnings, 0 with errors, 0 resource-limited";
    assert!(
        !stderr.lines().any(|l| l.trim() == forged_line),
        "AC-Q25: forged summary text must not appear as a standalone summary line; \
         got: {stderr:?}"
    );

    // AC-Q25 also requires the GENUINE summary appears exactly once, confirming the
    // directory summary path was reached and a real count was emitted (not suppressed or
    // omitted).  The directory contains exactly 1 error-severity file, so the genuine
    // summary is the line below.
    let genuine_summary = "0 clean, 0 with warnings, 1 with errors, 0 resource-limited";
    let genuine_count = stderr.lines().filter(|l| l.trim() == genuine_summary).count();
    assert_eq!(
        genuine_count,
        1,
        "AC-Q25: genuine summary must appear exactly once in stderr; got: {stderr:?}"
    );
}

// ── AC-Q26: help text is machine-verified, not eyeballed ─────────────────────

#[test]
fn help_documents_quiet_summary_contract() {
    // mds lint --help must document the directory summary and --quiet behaviour.
    let lint_help = mds_bin()
        .arg("lint")
        .arg("--help")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let help_text = String::from_utf8_lossy(&lint_help.stdout);

    // Pin the two documentation sites independently. A bare `contains("summary")` is
    // satisfied by EITHER site alone, so removing the long_about paragraph would leave
    // this test green on the strength of the after_help example line (and vice versa).
    assert!(
        help_text.contains("summary line to stderr"),
        "AC-Q26: mds lint --help long_about must document the directory summary line; \
         got: {help_text:?}"
    );
    assert!(
        help_text.contains("N clean, N with warnings, N with errors, N resource-limited"),
        "AC-Q26: mds lint --help must show the literal summary format string so the shipped \
         format and the documented format cannot drift; got: {help_text:?}"
    );
    assert!(
        help_text.contains("mds lint --quiet ."),
        "AC-Q26: mds lint --help must carry the `mds lint --quiet .` directory example; \
         got: {help_text:?}"
    );

    // mds --help must contain the global --quiet description.
    let global_help = mds_bin()
        .arg("--help")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    let global_text = String::from_utf8_lossy(&global_help.stdout);

    assert!(
        global_text.contains("Suppress status"),
        "AC-Q26: mds --help must contain the --quiet description; got: {global_text:?}"
    );
}

// ── AC-Q30 (lint half): --quiet in both argument positions ────────────────────
//
// --quiet is `global = true` (main.rs:31); both orderings must be identical.

#[test]
fn lint_directory_quiet_works_in_pre_subcommand_position() {
    let dir = tempfile::tempdir().unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("a.mds")).unwrap();
    fs::copy(fixture("lint_clean.mds"), dir.path().join("b.mds")).unwrap();

    // Post-subcommand.
    let post = lint_path(dir.path(), &["--quiet"]);

    // Pre-subcommand (global position).
    let pre = mds_bin()
        .arg("--quiet")
        .arg("lint")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(
        pre.status.code(),
        post.status.code(),
        "AC-Q30: pre-subcommand --quiet must produce the same exit code as post-subcommand"
    );
    assert_eq!(
        pre.stderr, post.stderr,
        "AC-Q30: pre-subcommand --quiet must produce identical stderr to post-subcommand"
    );

    // ANCHOR (PF-013): equality alone is vacuous — it holds just as well when BOTH
    // orderings are broken. Verified by mutation: with the summary gate removed
    // entirely, the two `assert_eq!`s above still passed. Pin the absolute value so
    // the test fails if `--quiet` stops suppressing, in either position.
    assert!(
        post.stderr.is_empty(),
        "AC-Q30 anchor: a clean tree under --quiet must produce empty stderr in BOTH \
         orderings, not merely equal stderr; got: {:?}",
        String::from_utf8_lossy(&post.stderr)
    );

    // Paired positive control: the same tree without --quiet DOES print the summary,
    // proving the emptiness assertion above is a real suppression, not a dead code path.
    let control = lint_path(dir.path(), &[]);
    assert_eq!(
        String::from_utf8_lossy(&control.stderr).trim(),
        "2 clean, 0 with warnings, 0 with errors, 0 resource-limited",
        "positive control: the same tree without --quiet must print the summary"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// D3-a: per-file analysis failures are counted in the "with errors" bucket
// ═══════════════════════════════════════════════════════════════════════════════
//
// spec.md §7.5 "With errors" bullet (lines 964-966): "error-severity lint findings
// OR a per-file analysis failure (source read, config load, or lint call failure)".
// Every prior #216 test populates the error bucket via lint findings (lint_error.mds).
// The tests below exercise the analysis-failure half, which has separate code paths
// in lint.rs (config-discovery error arms, source-read IO error arms) and must be
// covered independently (D3-a bucket-membership claim).

// ── D3-a: config-load failure counted in "with errors" (not "clean") ──────────

/// Config-load failure must land in the "with errors" bucket of the directory summary,
/// not the "clean" bucket.  A malformed `mds.json` in a subdirectory triggers a
/// config-load error for files in that subtree; the test verifies bucket membership
/// and that the four counters partition the full file set (AC-Q08).
#[test]
fn lint_directory_config_failure_counted_in_error_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // One clean file at root — no mds.json present, defaults apply.
    fs::copy(fixture("lint_clean.mds"), root.join("clean.mds")).unwrap();

    // Subdirectory with an unparseable mds.json; the file inside it triggers a
    // config-load failure (not a lint finding — different code path in lint.rs).
    let bad = root.join("bad");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("mds.json"), "{ INVALID JSON").unwrap();
    fs::copy(fixture("lint_clean.mds"), bad.join("bad.mds")).unwrap();

    let out = lint_path(root, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(2),
        "config-load failure must cause exit 2; stderr: {stderr}"
    );
    // D3-a: the file whose config load failed must be counted in "with errors".
    assert!(
        stderr.contains("1 clean, 0 with warnings, 1 with errors, 0 resource-limited"),
        "D3-a: config-load failure must be counted as 'with errors' in the summary; \
         got: {stderr:?}"
    );
    // AC-Q08: the four counters must partition the 2-file tree with no drops.
    let counts = parse_summary_counts(&stderr);
    assert_eq!(
        counts,
        [1, 0, 1, 0],
        "D3-a/AC-Q08: 1 clean + 0 warn + 1 error + 0 limit must sum to 2 files; \
         got counts: {counts:?}"
    );
}

// ── D3-a: config-load failure forces summary under --quiet ────────────────────
//
// The quiet gate is: !quiet || error_file_count > 0 || limit_file_count > 0.
// A config-load failure increments error_file_count, so the summary must be
// forced even under --quiet.
//
// PF-013 / ADR-009: the positive control (non-quiet run) confirms that "with errors"
// appears on the same tree, making the with-quiet assertion non-vacuous.

#[test]
fn lint_directory_config_failure_forces_summary_under_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::copy(fixture("lint_clean.mds"), root.join("clean.mds")).unwrap();
    let bad = root.join("bad");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("mds.json"), "{ INVALID JSON").unwrap();
    fs::copy(fixture("lint_clean.mds"), bad.join("bad.mds")).unwrap();

    let quiet = lint_path(root, &["--quiet"]);
    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);

    assert_eq!(
        quiet.status.code(),
        Some(2),
        "config-load failure under --quiet must still exit 2; stderr: {quiet_stderr:?}"
    );
    // D3-a: error_file_count > 0 forces the summary even under --quiet.
    assert!(
        quiet_stderr.contains("with errors"),
        "D3-a: config-load failure must force the summary under --quiet \
         (error_file_count > 0 gate); got: {quiet_stderr:?}"
    );

    // PF-013 / ADR-009 — paired positive control on the same tree.
    let control = lint_path(root, &[]);
    let control_stderr = String::from_utf8_lossy(&control.stderr);
    assert!(
        control_stderr.contains("with errors"),
        "positive control: non-quiet run with config-load failure must also emit the \
         summary with 'with errors', proving the --quiet assertion is non-vacuous; \
         got: {control_stderr:?}"
    );
}

// ── D3-a: chmod-000 source-read failure counted in "with errors" (unix only) ──
//
// A chmod-000 file causes a source-read IO error in the lint engine, which must be
// counted as FileTally::Error (not FileTally::Clean).  This is the source-read
// population of the "with errors" disjunction (spec.md §7.5 lines 964-966).
//
// Not run on Windows where permission enforcement uses ACLs, not Unix mode bits.
// Running as root bypasses Unix permission checks: the chmod-000 file is readable,
// so the tree lints clean and `assert_eq!(status, Some(2))` fails loudly rather
// than passing vacuously.  CI does not run as root.

#[cfg(unix)]
#[test]
fn lint_directory_unreadable_file_counted_in_error_bucket() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();

    // One clean, readable file.
    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean.mds")).unwrap();

    // One file made unreadable via chmod 000.
    let unreadable = dir.path().join("unreadable.mds");
    fs::copy(fixture("lint_clean.mds"), &unreadable).unwrap();
    fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = lint_path(dir.path(), &[]);

    // Restore permissions before any assertion so a test-failure panic cannot
    // leave a 000-mode file that confuses temporary-directory cleanup.
    let _ = fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644));

    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(2),
        "chmod-000 file must cause exit 2; stderr: {stderr}"
    );
    // D3-a: the source-read failure must land in "with errors", not "clean".
    assert!(
        stderr.contains("1 clean, 0 with warnings, 1 with errors, 0 resource-limited"),
        "D3-a: source-read failure must be counted as 'with errors' in the summary; \
         got: {stderr:?}"
    );
    let counts = parse_summary_counts(&stderr);
    assert_eq!(
        counts,
        [1, 0, 1, 0],
        "D3-a/AC-Q08: 1 clean + 0 warn + 1 error + 0 limit must sum to 2 files; \
         got counts: {counts:?}"
    );
}

// ── D3-a: chmod-000 source-read failure forces summary under --quiet (unix only)
//
// PF-013 / ADR-009: both the --quiet run and the positive control are captured
// while the file is still unreadable, then permissions are restored before
// asserting, so a panic during assertion cannot leave a bad-permission file.

#[cfg(unix)]
#[test]
fn lint_directory_unreadable_file_forces_summary_under_quiet() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();

    fs::copy(fixture("lint_clean.mds"), dir.path().join("clean.mds")).unwrap();

    let unreadable = dir.path().join("unreadable.mds");
    fs::copy(fixture("lint_clean.mds"), &unreadable).unwrap();
    fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    // Capture both runs while the file is still unreadable (positive control must
    // see the same tree state as the --quiet run).
    let quiet = lint_path(dir.path(), &["--quiet"]);
    let control = lint_path(dir.path(), &[]);

    // Restore permissions before any assertion.
    let _ = fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644));

    let quiet_stderr = String::from_utf8_lossy(&quiet.stderr);
    let control_stderr = String::from_utf8_lossy(&control.stderr);

    assert_eq!(
        quiet.status.code(),
        Some(2),
        "chmod-000 file under --quiet must still exit 2; stderr: {quiet_stderr:?}"
    );
    // D3-a: error_file_count > 0 forces the summary even under --quiet.
    assert!(
        quiet_stderr.contains("with errors"),
        "D3-a: source-read failure must force the summary under --quiet; \
         got: {quiet_stderr:?}"
    );

    // PF-013 / ADR-009 — paired positive control on the same (unreadable) tree.
    assert!(
        control_stderr.contains("with errors"),
        "positive control: non-quiet run with unreadable file must also show 'with errors', \
         proving the --quiet assertion is non-vacuous; got: {control_stderr:?}"
    );
}

// ── D4 (PF-004): Fixed: and Would fix: honour --quiet in dir-mode JSON emitter ─
//
// Finding: lint_one_file_accumulating (JSON) was missing Fixed: and Would fix:
// messages, creating a divergence from lint_one_file_human (PF-004 class).
// These tests verify: (a) the messages appear without --quiet (positive control per
// PF-013/ADR-009), and (b) --quiet suppresses them.

/// D4/PF-004: dir-mode `--fix --format json` emits `Fixed:` to stderr (positive),
/// and `--quiet` suppresses it (negative).  Both arms per PF-013.
#[test]
fn dir_json_fix_emits_fixed_and_quiet_suppresses_it() {
    // Case A (positive control — ADR-009/PF-013): Fixed: must appear without --quiet.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix", "--format", "json"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Fixed:"),
            "D4/PF-004: dir --fix --format json must emit 'Fixed:' without --quiet; \
             got stderr: {stderr}"
        );
        // Stdout must remain parseable JSON (PR1 contract: stdout carries ONLY the JSON).
        let stdout = String::from_utf8_lossy(&out.stdout);
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("stdout must be valid JSON; err: {e}; stdout: {stdout}")
        });
    }

    // Case B (quiet gate): Fixed: must be suppressed by --quiet.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix", "--format", "json", "--quiet"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Fixed:"),
            "D4/PF-004: dir --fix --format json --quiet must suppress 'Fixed:'; \
             got stderr: {stderr}"
        );
        // Stdout must still be parseable JSON even under --quiet.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("stdout must remain valid JSON under --quiet; err: {e}; stdout: {stdout}")
        });
    }
}

/// D4/PF-004: dir-mode `--fix --check --format json` emits `Would fix:` to stderr
/// (positive), and `--quiet` suppresses it (negative).  Both arms per PF-013.
#[test]
fn dir_json_fix_check_emits_would_fix_and_quiet_suppresses_it() {
    // Case A (positive control — ADR-009/PF-013): Would fix: must appear without --quiet.
    {
        let dir = tempfile::tempdir().unwrap();
        // Use a fresh copy so any prior --fix write in this run doesn't affect us.
        fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix", "--check", "--format", "json"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Would fix:"),
            "D4/PF-004: dir --fix --check --format json must emit 'Would fix:' without \
             --quiet; got stderr: {stderr}"
        );
        // Stdout must remain parseable JSON (PR1 contract).
        let stdout = String::from_utf8_lossy(&out.stdout);
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("stdout must be valid JSON; err: {e}; stdout: {stdout}")
        });
    }

    // Case B (quiet gate): Would fix: must be suppressed by --quiet.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix", "--check", "--format", "json", "--quiet"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Would fix:"),
            "D4/PF-004: dir --fix --check --format json --quiet must suppress 'Would fix:'; \
             got stderr: {stderr}"
        );
        // Stdout must still be parseable JSON.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let _: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!("stdout must remain valid JSON under --quiet; err: {e}; stdout: {stdout}")
        });
    }
}

// ── D4 (PF-004): Fixed: honours --quiet in dir-mode HUMAN emitter ─────────────
//
// PF-007: per-surface assertions cannot prove cross-surface parity.
// The JSON-side test (dir_json_fix_emits_fixed_and_quiet_suppresses_it) cannot
// substitute for this test — lint_one_file_human is a separate emitter from
// lint_one_file_accumulating.
// Mutation-verified: without this test, changing the human-mode gate at
// lint.rs:1723 from `!quiet` to `true` leaves all prior tests passing.

/// D4/PF-004: dir-mode `--fix` (human format, the default) emits `Fixed:` to stderr
/// (positive), and `--quiet` suppresses it (negative).  Both arms per PF-013/ADR-009.
///
/// PF-007: this is a separate emitter (`lint_one_file_human`) from the JSON-mode
/// equivalent (`lint_one_file_accumulating`); a gate on one is not inherited by the
/// other.  The JSON-mode equivalent is `dir_json_fix_emits_fixed_and_quiet_suppresses_it`.
#[test]
fn dir_human_fix_emits_fixed_and_quiet_suppresses_it() {
    // Case A (positive control — ADR-009/PF-013): Fixed: must appear without --quiet.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Fixed:"),
            "D4/PF-004: dir --fix (human format) must emit 'Fixed:' when a file is \
             successfully fixed (lint_one_file_human gate, lint.rs:1723); got stderr: {stderr}"
        );
        // Stdout must remain empty in human format mode.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.is_empty(),
            "D4: dir --fix human format must not write to stdout; got: {stdout:?}"
        );
    }

    // Case B (quiet gate): Fixed: must be suppressed by --quiet.
    {
        let dir = tempfile::tempdir().unwrap();
        fs::copy(fixture("lint_error.mds"), dir.path().join("err.mds")).unwrap();

        let out = lint_path(dir.path(), &["--fix", "--quiet"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("Fixed:"),
            "D4/PF-004: dir --fix --quiet (human format) must suppress 'Fixed:' \
             (lint_one_file_human gate, lint.rs:1723); got stderr: {stderr}"
        );
    }
}

// ── D1-a exception: --fix --diff --format json writes diff then JSON envelope ──
//
// spec.md:938 and lint.rs:1231-1233 document that `--fix --diff --format json`
// writes the unified diff to stdout before the JSON envelope, making stdout
// non-JSON-parseable as a whole by design.  This test pins the wire contract so
// rerouting the diff to stderr (silently 'fixing' the exception) is caught.

/// D1-a: `mds lint --fix --diff --format json <dir>` writes the unified diff to
/// stdout before the JSON envelope.  The final stdout line must parse as JSON;
/// stdout as a whole must NOT parse as JSON (the diff header breaks it).
///
/// Pins spec.md:938 and the D2-decision comment at lint.rs:1231-1233.  Without
/// this test, rerouting the diff to stderr would silently remove the documented
/// exception with no test failure.
#[test]
fn lint_fix_diff_format_json_diff_precedes_json_envelope() {
    let dir = tempfile::tempdir().unwrap();
    // lint_error.mds has a Tier A auto-fixable issue (duplicate @export greet),
    // so --fix --diff produces a non-empty unified diff.
    fs::copy(fixture("lint_error.mds"), dir.path().join("fixable.mds")).unwrap();

    let out = lint_path(dir.path(), &["--fix", "--diff", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    assert!(
        !lines.is_empty(),
        "--fix --diff --format json must produce output on stdout; got empty"
    );

    // The final stdout line must parse as JSON (the envelope).
    let last = lines[lines.len() - 1];
    let _: serde_json::Value = serde_json::from_str(last).unwrap_or_else(|e| {
        panic!(
            "D1-a: final stdout line must parse as JSON (the envelope follows the diff); \
             err: {e}; last line: {last:?}; full stdout: {stdout:?}"
        )
    });

    // At least one line preceding the envelope must be a unified-diff marker.
    let diff_present = lines[..lines.len().saturating_sub(1)]
        .iter()
        .any(|l| l.starts_with("@@") || l.starts_with("--- ") || l.starts_with("+++ "));
    assert!(
        diff_present,
        "D1-a: unified diff must precede the JSON envelope on stdout (spec.md:938, \
         lint.rs:1231-1233); full stdout: {stdout:?}"
    );

    // Stdout as a whole must NOT parse as JSON — the diff header breaks the shape.
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err(),
        "D1-a: stdout as a whole must not parse as JSON under --fix --diff --format json; \
         this is the documented exception (spec.md:938); stdout: {stdout:?}"
    );
}
