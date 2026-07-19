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
//! - L-CLI-FIX3: block-spanning --fix is refused fail-closed; file unchanged (TEST-3)
//! - L-CLI-STDIN1: stdin (no fix) → diagnostics to stderr, stdout empty
//! - L-CLI-STDIN2: --fix stdin → fixed source to stdout, diagnostics to stderr
//! - L-CLI-VARS: --set passes runtime variables to the gate check
//! - L-CLI-QUIET1: --quiet suppresses warnings, exit 0 on clean
//! - L-CLI-DIR1: directory mode path-sorts and lints all files including partials
//! - L-CLI-RESOURCE: nesting > MAX_NESTING_DEPTH (64) → exit 3 ResourceLimit (TEST-4)
//! - L-CLI-DIR2: directory --format json files[] order is deterministic (TEST-6)
//! - I-24: unreachable-branch --fix is refused (block-spanning); file unchanged, exit 2
//! - I-26: shadow-variable Info severity emits diagnostic and exits 0 (Info never affects exit)

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

// ── L-CLI-FIX3: block-spanning --fix refusal (TEST-3) ────────────────────────
//
// Exercises the KB gotcha "block-spanning Tier A fixes are always refused":
// diag_to_edit() removes only the opening directive line (span-guided byte
// removal per ADR-001), orphaning the @end. The reverify gate calls
// lint_str_with on the edited source, which fails to parse (orphaned @end),
// and refuses the entire fix batch fail-closed.
//
// Fixture: lint_block_span_empty.mds — multi-line empty @define whose body is
// a single blank line (whitespace-only Text node). The empty-block rule fires
// (Tier A, Warn), the fix is attempted and refused, and the residual Warn
// finding determines exit code 1. (applies ADR-001)

#[test]
fn fix_refused_for_block_spanning_empty_define_and_file_unchanged() {
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

    // The fix must be refused: removing the @define line orphans @end, which
    // the reverify gate catches as a parse error and rejects fail-closed.
    assert!(
        stderr.contains("fix rejected"),
        "fix must be refused for block-spanning empty-block; got stderr: {stderr}"
    );

    // Residual finding: the original empty-block Warn survives → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit code must reflect residual warn finding after fix refusal; got stderr: {stderr}"
    );

    // Critical: the file on disk must be left UNCHANGED.
    let after = fs::read_to_string(&target).unwrap();
    assert_eq!(
        original, after,
        "file must be left unchanged when --fix is refused"
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

// ── I-24: unreachable-branch --fix refusal (block-spanning, ADR-001) ─────────
//
// unreachable-branch is Tier A (auto-fixable per tier.rs), but the fix is always
// refused for @if blocks: diag_to_edit() removes only the opening @if directive
// line (span-guided byte removal per ADR-001), orphaning @else/@end. The reverify
// gate calls lint_str_with on the edited source, which fails to parse (orphaned
// @else), and refuses the entire fix batch fail-closed.
//
// Fixture: lint_unreachable_branch.mds — always-true @if "x" == "x": with a
// later @else branch (the later branch makes unreachable-branch fire at its
// default Error severity). After fix refusal the residual Error finding
// determines exit 2. Non-vacuous: asserts "fix rejected" in stderr, exit 2,
// AND file content byte-identical to before --fix. (applies ADR-001)

#[test]
fn fix_refused_for_unreachable_branch_and_file_unchanged() {
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

    // The fix must be refused: removing the @if line orphans @else/@end, caught
    // by the reverify gate as a parse error and refused fail-closed (ADR-001).
    assert!(
        stderr.contains("fix rejected"),
        "fix must be refused for block-spanning unreachable-branch; got stderr: {stderr}"
    );

    // Residual: unreachable-branch Error finding survives fix refusal → exit 2.
    assert_eq!(
        out.status.code(),
        Some(2),
        "residual Error finding after fix refusal must exit 2; got stderr: {stderr}"
    );

    // Critical: file on disk must be left UNCHANGED.
    let after = fs::read_to_string(&target).unwrap();
    assert_eq!(
        original, after,
        "file must be unchanged when unreachable-branch --fix is refused"
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
                  @define greet(name):\n  Hello {name}!\n@end\n\n\
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

// ── Test (c): --fix --check on refused-fix fixture → prints "fix rejected" ───
//
// Pins bug-5 / PF-004 fix for the check path: preview_fixes now returns a
// PreviewOutcome::Rejected so --fix --check can surface the rejection reason.
//
// Pre-Phase-B behavior: preview_fixes returned Option<String> and mapped Rejected
// to None — --fix --check never printed "fix rejected" even when the reverify gate
// refused the edit.
//
// Fixture: lint_block_span_empty.mds (multi-line empty @define). The fix removes
// the opening @define line, orphaning @end → reverify gate fails → Rejected.

#[test]
fn fix_check_refused_fix_prints_rejected_not_would_fix() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_block_span_empty.mds");
    fs::copy(fixture("lint_block_span_empty.mds"), &target).unwrap();
    let original = fs::read_to_string(&target).unwrap();

    let out = lint_path(&target, &["--fix", "--check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // "fix rejected" must appear: the reverify gate refused the empty-block removal.
    assert!(
        stderr.contains("fix rejected"),
        "--fix --check must print 'fix rejected' when the reverify gate refuses; got stderr: {stderr}"
    );

    // "Would fix" must NOT appear: the fix was rejected, not pending.
    assert!(
        !stderr.contains("Would fix"),
        "--fix --check must NOT print 'Would fix' when fix is rejected; got stderr: {stderr}"
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

// ── Test (f): Overlap fixture → visible "fix rejected"/overlap message ────────
//
// Pins bug-12 / preview-honesty: when two Tier-A edits target the same line
// (overlap detected), the fix is refused with "Overlapping fix spans detected"
// and the output is NOT silent.
//
// Fixture: lint_overlap.mds — a @define containing an @if "a"=="a" block with
// a @elseif "a"=="a" that is both unreachable AND has an empty body.  Both
// empty-block and unreachable-branch fire on the same @elseif line → same byte
// range → overlap detected → Rejected.
//
// Tests --fix (apply) path: the rejection message appears on stderr, the file is
// untouched, and exit code reflects the residual diagnostics.

#[test]
fn fix_overlap_surfaced_not_silent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("lint_overlap.mds");
    fs::copy(fixture("lint_overlap.mds"), &target).unwrap();
    let original = fs::read_to_string(&target).unwrap();

    let out = lint_path(&target, &["--fix"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // "fix rejected" must appear — not silent.
    assert!(
        stderr.contains("fix rejected"),
        "--fix on overlap fixture must print 'fix rejected'; got stderr: {stderr}"
    );

    // The overlap reason must mention "overlap" or "Overlapping".
    assert!(
        stderr.to_lowercase().contains("overlap"),
        "rejection reason must mention 'overlap'; got stderr: {stderr}"
    );

    // File must be unchanged (fix was refused, no write).
    let after = fs::read_to_string(&target).unwrap();
    assert_eq!(
        original, after,
        "overlap-rejected file must be left unchanged"
    );
}

// ── Test (g): PartiallyFixed end-to-end: applied count in summary ─────────────
//
// Pins the PartiallyFixed outcome: when some edits pass the reverify gate and
// some fail, the CLI writes the partially-fixed file and emits a
// "{applied} of {total} fixes applied" summary.
//
// Fixture: lint_partial_fix.mds — contains a multi-line empty @define (Tier A,
// fix fails reverify because @end is orphaned after removing the @define line)
// and a duplicate @export (Tier A, fix passes — just removes a line).
//
// Expected behaviour:
// - Batch attempt: fails (empty-block removal + dup-export removal together →
//   @end orphaned → reverify rejects).
// - Per-edit fallback right-to-left:
//   1. duplicate-export (higher offset) → applied, reverify passes.
//   2. empty-block (lower offset) → reverify fails → rejected.
// - File written with one @export greet remaining; empty @define still present.
// - Stderr: "1 of 2 fixes applied" (or "Partially fixed: … (1 of 2 fixes applied)").

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

    // The empty @define must still be present (empty-block fix was rejected).
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
    let source = "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n";
    let out = lint_stdin(source, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // "duplicate-export" diagnostic must appear.
    assert!(
        stderr.contains("duplicate-export"),
        "diagnostic must appear in stdin report-only mode; got: {stderr}"
    );

    // "input.mds" must appear: miette renders it as the file reference in the span header.
    assert!(
        stderr.contains("input.mds"),
        "stdin mode must show 'input.mds' in the code frame; got: {stderr}"
    );

    // At least one token from the source must appear in the code frame context.
    // miette renders the offending line; "@export greet" is on that line.
    assert!(
        stderr.contains("@export"),
        "code frame must include the offending source line '@export greet'; got: {stderr}"
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
    let source = "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n";
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

// ── ESC injection regression (issue #5 / ESC-INJECTION) ──────────────────────

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
        "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n",
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
        "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n",
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
        "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n",
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
        "@define greet(name):\n  Hello {name}!\n@end\n\n@export greet\n@export greet\n",
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
