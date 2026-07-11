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
//! - L-CLI-STDIN1: stdin (no fix) → diagnostics to stderr, stdout empty
//! - L-CLI-STDIN2: --fix stdin → fixed source to stdout, diagnostics to stderr
//! - L-CLI-VARS: --set passes runtime variables to the gate check
//! - L-CLI-QUIET1: --quiet suppresses warnings, exit 0 on clean
//! - L-CLI-DIR1: directory mode path-sorts and lints all files including partials

mod common;
use common::{fixture, mds_bin};

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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
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
    let source = "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n";
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
    let source = "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n";
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
    let out = lint_stdin("Hello {name}!", &["--fix", "--format", "json"]);
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
