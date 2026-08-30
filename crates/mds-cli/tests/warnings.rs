mod common;
use common::{assert_no_control_chars, fixture, mds_bin};

#[test]
fn check_collecting_warnings_returns_warnings_for_empty_include() {
    // check_collecting_warnings should succeed (Ok) and surface the empty-@include
    // warning in the returned Vec<String> without printing to stderr.
    let path = fixture("include_empty_body.mds");
    let ((), warnings) = mds::check_collecting_warnings(&path, None)
        .expect("check_collecting_warnings should succeed on a valid file");
    assert!(
        warnings.iter().any(|w| w.contains("empty output")),
        "expected at least one warning about empty @include, got: {warnings:?}"
    );
}

#[test]
fn check_str_collecting_warnings_no_warnings_for_clean_source() {
    // A well-formed source with no warnings should return an empty warnings vec.
    let source = "---\nname: Test\n---\nHello {{name}}!\n";
    let ((), warnings) = mds::check_str_collecting_warnings(source, None, None)
        .expect("check_str_collecting_warnings should succeed on clean source");
    assert!(
        warnings.is_empty(),
        "clean source should produce no warnings, got: {warnings:?}"
    );
}

#[test]
fn check_str_collecting_warnings_errors_on_invalid_source() {
    // check_str_collecting_warnings should return Err for sources with compile errors,
    // independently of CLI argument parsing.
    let source = "{{undefined_variable}}";
    let result = mds::check_str_collecting_warnings(source, None, None);
    assert!(
        result.is_err(),
        "check_str_collecting_warnings should return Err for undefined variable"
    );
}

#[test]
fn warning_cap_at_max_warnings() {
    // Build a template with many @include of modules with no body.
    // Each @include of an empty module produces one warning.
    // We use a subdirectory with a shared empty library module.
    let dir = tempfile::tempdir().unwrap();

    // Create a shared empty module (no body — just a @define with no body text)
    let lib_path = dir.path().join("empty_lib.mds");
    std::fs::write(&lib_path, "@define noop():\n@end\n").unwrap();

    // Build main template: import empty_lib as 'lib' and @include it 1010 times.
    let mut src = String::from("@import \"./empty_lib.mds\" as lib\n");
    for _ in 0..1010 {
        src.push_str("@include lib\n");
    }
    let main_path = dir.path().join("main.mds");
    std::fs::write(&main_path, &src).unwrap();

    let result = mds::compile_collecting_warnings(&main_path, None)
        .expect("template should compile successfully");
    let warnings = result.warnings;

    assert_eq!(
        warnings.len(),
        1000,
        "warnings must be capped at exactly 1000, got {}",
        warnings.len()
    );
}

#[test]
fn check_empty_body_no_warning_in_quiet_mode() {
    // When -q/--quiet is set, the warning from @include of an empty module
    // should be suppressed for `mds check` too (not just `mds build`).
    let output = mds_bin()
        .args([
            "check",
            fixture("include_empty_body.mds").to_str().unwrap(),
            "--quiet",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "quiet check should succeed");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "quiet flag should suppress warnings for check command, got: {stderr}"
    );
}

#[test]
fn include_empty_body_no_warning_in_quiet_mode() {
    // When -q/--quiet is set, the warning should be suppressed.
    let output = mds_bin()
        .args([
            "build",
            fixture("include_empty_body.mds").to_str().unwrap(),
            "-o",
            "-",
            "--quiet",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "quiet build should succeed");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "quiet flag should suppress the empty-include warning, got: {stderr}"
    );
}

#[test]
fn include_empty_body_emits_warning() {
    // Per spec 4.8: @include of a module with no body text should emit a warning
    // to stderr (when not in quiet mode).
    let output = mds_bin()
        .args([
            "build",
            fixture("include_empty_body.mds").to_str().unwrap(),
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build should succeed even when include is empty"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("warning") && stderr.contains("fns"),
        "expected warning about empty @include on stderr, got: {stderr}"
    );
}

// ── I1-I7: duplicate --set / --set-string warnings (issue #200) ──────────────

/// Helper: count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

// Pinned warning strings — a USER-FACING CONTRACT.
const DUP_SET_WARNING: &str =
    "warning: variable 'x' is set more than once by --set; the last value wins";
const DUP_SET_STRING_WARNING: &str =
    "warning: variable 'x' is set more than once by --set-string; the last value wins";

#[test]
fn i1_set_duplicate_warns_exactly_once_on_stderr() {
    // I1: mds build with --set x=1 --set x=2 must produce the warning exactly once.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "x=1",
            "--set",
            "x=2",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with duplicate --set must succeed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let count = count_occurrences(&stderr, DUP_SET_WARNING);
    assert_eq!(
        count, 1,
        "I1: expected warning exactly once, found {count} times; stderr:\n{stderr}"
    );
}

#[test]
fn i2_set_string_duplicate_warns_exactly_once_on_stderr() {
    // I2: mds build with --set-string x=a --set-string x=b must produce the warning
    // exactly once.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            "-",
            "--set-string",
            "x=a",
            "--set-string",
            "x=b",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with duplicate --set-string must succeed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let count = count_occurrences(&stderr, DUP_SET_STRING_WARNING);
    assert_eq!(
        count, 1,
        "I2: expected warning exactly once, found {count} times; stderr:\n{stderr}"
    );
}

#[test]
fn i3_quiet_suppresses_duplicate_warning() {
    // I3: --quiet must suppress the duplicate-key warning.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "x=1",
            "--set",
            "x=2",
            "--quiet",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(output.status.success(), "quiet build must succeed");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains(DUP_SET_WARNING),
        "I3: --quiet must suppress duplicate-key warning; got stderr:\n{stderr}"
    );
}

#[test]
fn i4_check_parity_with_build() {
    // I4: mds check must emit the same duplicate-key warning as mds build.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "check",
            src.to_str().unwrap(),
            "--set",
            "x=1",
            "--set",
            "x=2",
        ])
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "check with duplicate --set must succeed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let count = count_occurrences(&stderr, DUP_SET_WARNING);
    assert_eq!(
        count, 1,
        "I4: check must warn exactly once; found {count} times; stderr:\n{stderr}"
    );
}

#[test]
fn i5_lint_parity_with_build() {
    // I5: mds lint must emit the same duplicate-key warning as mds build.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "lint",
            src.to_str().unwrap(),
            "--set",
            "x=1",
            "--set",
            "x=2",
        ])
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // lint exits 0 when no lint findings (content has no issues).
    assert!(
        output.status.success(),
        "lint with duplicate --set must succeed (no lint errors); stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let count = count_occurrences(&stderr, DUP_SET_WARNING);
    assert_eq!(
        count, 1,
        "I5: lint must warn exactly once; found {count} times; stderr:\n{stderr}"
    );
}

#[test]
fn i6_triple_repeat_warns_exactly_once() {
    // I6: three repetitions of the same key must still produce exactly one warning.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "x=1",
            "--set",
            "x=2",
            "--set",
            "x=3",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with triple --set must succeed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let count = count_occurrences(&stderr, DUP_SET_WARNING);
    assert_eq!(
        count, 1,
        "I6: triple repeat must still produce exactly one warning; found {count}; stderr:\n{stderr}"
    );
}

#[test]
fn i7_hostile_key_is_wire_escaped_in_warning() {
    // I7: a key containing an ESC byte (U+001B) and an RLO (U+202E) must appear in
    // the warning with those codepoints replaced by their \uXXXX escape sequences,
    // never as raw control bytes.
    //
    // PF-018: authoring control-character strings with an LLM agent silently decodes
    // \uXXXX 4-hex sequences to live bytes.  Use Rust braced escapes (\u{1b},
    // \u{202e}) — the editor layer does NOT decode those.
    //
    // The hostile key is: ESC (U+001B) followed by "[31m" followed by RLO (U+202E).
    // Built from char literals to avoid any tool-layer decoding.
    let esc: char = '\u{1b}';
    let rlo: char = '\u{202e}';
    let hostile_key = format!("{esc}[31m{rlo}");

    // The expected ESCAPED forms as literal backslash sequences in the warning.
    // Written as escaped-backslash Rust string literals so the tool layer has no
    // 4-hex sequence to decode.
    let expected_esc_form = "\\u001B"; // six-char: backslash + u001B
    let expected_rlo_form = "\\u202E"; // six-char: backslash + u202E

    let arg_set_first = format!("{hostile_key}=1");
    let arg_set_second = format!("{hostile_key}=2");

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "Hello world").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            "-",
            "--set",
            &arg_set_first,
            "--set",
            &arg_set_second,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with hostile key must succeed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();

    // PF-013 NON-VACUITY FIRST: assert the escaped form IS present before asserting
    // absence of raw control bytes.  A test that only asserts absence passes trivially
    // if the feature never ran.
    assert!(
        stderr.contains(expected_esc_form),
        "I7: expected escaped ESC form '{}' in stderr; got:\n{:?}",
        expected_esc_form,
        stderr
    );
    assert!(
        stderr.contains(expected_rlo_form),
        "I7: expected escaped RLO form '{}' in stderr; got:\n{:?}",
        expected_rlo_form,
        stderr
    );

    // Now assert the raw control bytes are absent.
    assert_no_control_chars(&stderr, "I7 stderr");
}

// ── R2: @include warning precision ───────────────────────────────────────────

#[test]
fn r2_include_export_hides_prompt_warns_about_export_list() {
    // include_export_hides_prompt.mds imports a module that HAS body text but
    // whose @export list excludes "prompt".  The warning should mention the
    // exports list, NOT say "no body text".
    let output = mds_bin()
        .args([
            "build",
            "-o",
            "-",
            fixture("include_export_hides_prompt.mds").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build of include_export_hides_prompt.mds should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Warning should be present.
    assert!(
        stderr.contains("empty output"),
        "R2: expected 'empty output' warning; got: {stderr:?}"
    );
    // WARN-B: mentions the exports list.
    assert!(
        stderr.contains("does not export") || stderr.contains("@export list"),
        "R2: WARN-B must mention the @export list; got: {stderr:?}"
    );
    // WARN-B must NOT say "no body text".
    assert!(
        !stderr.contains("no body text"),
        "R2: WARN-B must NOT say 'no body text'; got: {stderr:?}"
    );
    assert_no_control_chars(&stderr, "R2 warning line");
}
