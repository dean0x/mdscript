mod common;
use common::{fixture, mds_bin};

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
    let source = "---\nname: Test\n---\nHello {name}!\n";
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
    let source = "{undefined_variable}";
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

    let (_, warnings) = mds::compile_collecting_warnings(&main_path, None)
        .expect("template should compile successfully");

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
