mod common;
use common::{fixture, mds_bin};

#[test]
fn check_stdin_valid() {
    let mut child = mds_bin()
        .args(["check", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "check stdin should succeed for valid input"
    );
}

#[test]
fn check_invalid_exits_nonzero() {
    let output = mds_bin()
        .args(["check", fixture("undefined_var.mds").to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "check on invalid file should exit non-zero"
    );
}

#[test]
fn check_auto_detects_single_mds_file_in_directory() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let mds_path = dir.path().join("valid.mds");
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {name}!\n").expect("write fixture");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("check")
        .output()
        .expect("run mds check");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "check auto-detect should succeed; stderr: {stderr}"
    );
    assert!(
        stderr.contains("OK"),
        "check should print OK message, got stderr: {stderr}"
    );
}

#[test]
fn init_creates_compilable_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("test.mds");

    // Create the file
    let init_output = mds_bin()
        .args(["init", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(init_output.status.success(), "init should succeed");
    assert!(target.exists(), "init should create the file");

    // Compile the created file
    let build_output = mds_bin()
        .args(["build", target.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build_output.status.success(),
        "init-created file should compile successfully; stderr: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );
}

#[test]
fn init_force_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("test.mds");
    std::fs::write(&target, "original content").unwrap();

    let output = mds_bin()
        .args(["init", target.to_str().unwrap(), "--force"])
        .output()
        .unwrap();

    assert!(output.status.success(), "init --force should succeed");
    let content = std::fs::read_to_string(&target).unwrap();
    assert!(
        content != "original content",
        "init --force should overwrite the file"
    );
    assert!(
        content.contains("Hello"),
        "overwritten file should contain template content"
    );
}

#[test]
fn init_does_not_overwrite_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.mds");
    std::fs::write(&existing, "original content").unwrap();

    // Try to init over existing file - should fail
    let output = mds_bin()
        .args(["init", existing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "init should fail on existing file without --force"
    );

    // Verify original content preserved
    let content = std::fs::read_to_string(&existing).unwrap();
    assert_eq!(content, "original content");
}

#[test]
fn set_flag_cli_overrides() {
    // --set name=Test should override the frontmatter variable 'name'
    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "name=Test",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "build with --set should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello Test!"),
        "expected '--set name=Test' to override frontmatter, got: {stdout}"
    );
}

#[test]
fn set_flag_boolean_coercion() {
    // --set premium=false must coerce the string "false" to boolean false,
    // so @if premium: evaluates as falsy and the @else branch is rendered.
    let output = mds_bin()
        .args([
            "build",
            fixture("set_flag_false.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "premium=false",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set premium=false should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Upgrade for premium features."),
        "expected falsy branch when --set premium=false, got: {stdout}"
    );
    assert!(
        !stdout.contains("Thanks for being premium!"),
        "truthy branch must not appear when --set premium=false, got: {stdout}"
    );
}

#[test]
fn set_flag_boolean_true_coercion() {
    // --set premium=true must coerce to boolean true (truthy branch rendered).
    let output = mds_bin()
        .args([
            "build",
            fixture("set_flag_false.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "premium=true",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set premium=true should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Thanks for being premium!"),
        "expected truthy branch when --set premium=true, got: {stdout}"
    );
}

#[test]
fn set_flag_numeric_coercion() {
    // --set count=3 must coerce the string "3" to a number so {count} renders as "3".
    let output = mds_bin()
        .args([
            "build",
            fixture("set_count.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "count=3",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set count=3 should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Count is 3."),
        "expected numeric value from --set count=3, got: {stdout}"
    );
}

#[test]
fn set_flag_null_coercion() {
    // --set premium=null must coerce to Value::Null (falsy), so @else branch renders.
    let output = mds_bin()
        .args([
            "build",
            fixture("set_flag_false.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "premium=null",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set premium=null should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Upgrade for premium features."),
        "expected falsy branch when --set premium=null, got: {stdout}"
    );
}

#[test]
fn set_flag_empty_array() {
    // --set items=[] must produce Value::Array(vec![]) — not [String("")].
    // The empty array is falsy, so the @else branch should render.
    let output = mds_bin()
        .args([
            "build",
            fixture("set_items_empty.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "items=[]",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with --set items=[] should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("empty"),
        "expected falsy branch when --set items=[], got: {stdout}"
    );
}

#[test]
fn set_flag_duplicate_key_last_wins() {
    // Passing --set name=First --set name=Second should use "Second" (last wins).
    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            "-",
            "--set",
            "name=First",
            "--set",
            "name=Second",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build with duplicate --set should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello Second!"),
        "last --set value should win when key repeated, got: {stdout}"
    );
    assert!(
        !stdout.contains("Hello First!"),
        "first --set value should be overridden by second, got: {stdout}"
    );
}

#[test]
fn exit_code_success() {
    // Use a temp directory to avoid writing simple.md into tests/fixtures/.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("simple.mds");
    std::fs::write(&src, "---\nname: Alice\n---\nHello {name}!\n").unwrap();
    let status = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to run mds");
    assert!(
        status.success(),
        "expected exit code 0 for successful build"
    );
}

#[test]
fn exit_code_file_not_found() {
    let status = mds_bin()
        .args(["build", "/tmp/no_such_file_12345.mds"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to run mds");
    assert_eq!(
        status.code(),
        Some(2),
        "expected exit code 2 for file-not-found"
    );
}

#[test]
fn exit_code_syntax_error() {
    // A file with an undefined variable produces a logical/syntax error → exit code 1.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.mds");
    std::fs::write(&path, "{undefined_var}").unwrap();
    let status = mds_bin()
        .args(["build"])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to run mds");
    assert_eq!(
        status.code(),
        Some(1),
        "expected exit code 1 for undefined-variable error"
    );
}

#[test]
fn cli_build_rejects_directory_input() {
    let dir = tempfile::tempdir().unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with directory input must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("expected a file") || stderr.contains("directory"),
        "error should mention expected-a-file or directory, got: {stderr}"
    );
}

#[test]
fn cli_init_rejects_path_traversal() {
    let output = mds_bin()
        .arg("init")
        .arg("../escaped.mds")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "init with path traversal must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("..") || stderr.contains("traversal") || stderr.contains("components"),
        "error should mention path traversal, got: {stderr}"
    );
}
