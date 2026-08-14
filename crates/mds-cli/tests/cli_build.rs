mod common;
use common::{fixture, mds_bin};

#[test]
fn build_to_file() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("output.md");

    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "build to file should succeed");
    let content = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        content.contains("Hello Alice!"),
        "output file should contain compiled content, got: {content}"
    );
}

#[test]
fn build_from_stdin() {
    let mut child = mds_bin()
        .args(["build", "-"])
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
        .write_all(b"---\nname: World\n---\nHello {{name}}!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "build from stdin should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello World!"),
        "stdin build should produce 'Hello World!', got: {stdout}"
    );
}

#[test]
fn build_with_vars_file() {
    let dir = tempfile::tempdir().unwrap();
    let vars_path = dir.path().join("vars.json");
    std::fs::write(&vars_path, r#"{"name": "Overridden"}"#).unwrap();

    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            "-",
            "--vars",
            vars_path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with vars file should succeed"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello Overridden!"),
        "vars file should override frontmatter 'name', got: {stdout}"
    );
}

#[test]
fn build_invalid_input_exits_nonzero() {
    let output = mds_bin()
        .args(["build", "nonexistent_file_that_does_not_exist.mds"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with nonexistent file should exit non-zero"
    );
}

#[test]
fn build_quiet_flag() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("output.md");

    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
            "-q",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "quiet build should succeed");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "quiet flag should produce no stderr output, got: {stderr}"
    );
}

#[test]
fn build_auto_detects_single_mds_file_in_directory() {
    // Create a temp directory with exactly one .mds file.
    // The default output is <name>.md next to the source, so auto.md should be created.
    let dir = tempfile::tempdir().expect("create temp dir");
    let mds_path = dir.path().join("auto.mds");
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {{name}}!\n").expect("write fixture");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run mds build");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "auto-detect should succeed with one .mds file; stderr: {stderr}"
    );
    // Default output: auto.md written next to auto.mds
    let md_path = dir.path().join("auto.md");
    assert!(
        md_path.exists(),
        "auto.md should be created next to auto.mds; stderr: {stderr}"
    );
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "auto-detect output file should contain 'Hello World!', got: {content}"
    );
}

#[test]
fn build_errors_when_no_mds_files_in_directory() {
    let dir = tempfile::tempdir().expect("create temp dir");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .output()
        .expect("run mds build");

    assert!(
        !output.status.success(),
        "build with no .mds files should fail"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("no .mds files") || stderr.contains("No .mds files"),
        "error should mention missing .mds files, got: {stderr}"
    );
}

#[test]
fn build_errors_when_multiple_mds_files_in_directory() {
    let dir = tempfile::tempdir().expect("create temp dir");
    std::fs::write(dir.path().join("a.mds"), "---\n---\nhello\n").expect("write a.mds");
    std::fs::write(dir.path().join("b.mds"), "---\n---\nworld\n").expect("write b.mds");

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .output()
        .expect("run mds build");

    assert!(
        !output.status.success(),
        "build with multiple .mds files should fail"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("multiple") || stderr.contains("Multiple"),
        "error should mention multiple .mds files, got: {stderr}"
    );
}

#[test]
fn build_auto_detect_writes_file() {
    // Auto-detect + default file output: auto.mds → auto.md.
    let dir = tempfile::tempdir().unwrap();
    let mds_path = dir.path().join("auto.mds");
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("build")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "auto-detect build should succeed; stderr: {stderr}"
    );
    let md_path = dir.path().join("auto.md");
    assert!(
        md_path.exists(),
        "auto.md should be written next to auto.mds"
    );
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "auto-detect default output should contain 'Hello World!', got: {content}"
    );
}

#[test]
fn build_default_writes_file_next_to_source() {
    // With no -o or --out-dir, `mds build foo.mds` writes `foo.md` next to the source.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "default build should succeed; stderr: {stderr}"
    );

    let md_path = dir.path().join("hello.md");
    assert!(
        md_path.exists(),
        "hello.md should be written next to hello.mds"
    );
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "output file should contain compiled content, got: {content}"
    );
    // stdout should be empty (output went to file)
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.is_empty(),
        "stdout should be empty when writing to file, got: {stdout}"
    );
    // stderr should mention the output path
    assert!(
        stderr.contains("hello.md"),
        "stderr should mention the output file, got: {stderr}"
    );
}

#[test]
fn build_dash_o_dash_writes_to_stdout() {
    // `-o -` forces stdout regardless of the default file-output behavior.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    let output = mds_bin()
        .args(["build", src.to_str().unwrap(), "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build -o - should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello World!"),
        "-o - should produce output on stdout, got: {stdout}"
    );
    // No .md file should be written
    assert!(
        !dir.path().join("hello.md").exists(),
        "no hello.md should be written when -o - is used"
    );
}

#[test]
fn build_stdin_defaults_to_stdout() {
    // `mds build -` (stdin) with no -o writes to stdout.
    let mut child = mds_bin()
        .args(["build", "-"])
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
        .write_all(b"---\nname: World\n---\nHello {{name}}!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "stdin build should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("Hello World!"),
        "stdin input with no -o should default to stdout, got: {stdout}"
    );
}

#[test]
fn build_stdin_with_output_writes_file() {
    // `echo "..." | mds build - -o out.md` writes to a file.
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("out.md");

    let mut child = mds_bin()
        .args(["build", "-", "-o", out_path.to_str().unwrap()])
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
        .write_all(b"---\nname: World\n---\nHello {{name}}!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdin build with -o should succeed"
    );
    assert!(out_path.exists(), "output file should be created");
    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "output file should contain compiled content, got: {content}"
    );
    // stdout should be empty
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.is_empty(),
        "stdout should be empty when -o writes to file, got: {stdout}"
    );
}

#[test]
fn build_stdin_with_out_dir_writes_to_directory() {
    // `echo "..." | mds build - --out-dir dist` should write `dist/output.md`.
    let dir = tempfile::tempdir().unwrap();
    let dist = dir.path().join("dist");

    let mut child = mds_bin()
        .args(["build", "-", "--out-dir", dist.to_str().unwrap()])
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
        .write_all(b"---\nname: World\n---\nHello {{name}}!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdin + --out-dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let md_path = dist.join("output.md");
    assert!(
        md_path.exists(),
        "output.md should be written inside dist/ for stdin + --out-dir"
    );
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "output file should contain compiled content, got: {content}"
    );
    // stdout should be empty (output went to file)
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.is_empty(),
        "stdout should be empty when --out-dir writes to file, got: {stdout}"
    );
}

#[test]
fn build_out_dir_writes_to_directory() {
    // `--out-dir dist` writes `dist/foo.md`.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("foo.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    let dist = dir.path().join("dist");
    // dist does not exist yet — should be created.

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "--out-dir",
            dist.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build --out-dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let md_path = dist.join("foo.md");
    assert!(md_path.exists(), "foo.md should be written inside dist/");
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "out-dir output should contain compiled content, got: {content}"
    );
}

#[test]
fn build_out_dir_creates_directory() {
    // `--out-dir` auto-creates the directory (including nested paths).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("template.mds");
    std::fs::write(&src, "Hello!\n").unwrap();

    let nested = dir.path().join("a").join("b").join("c");

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "--out-dir",
            nested.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build --out-dir with nested path should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        nested.join("template.md").exists(),
        "template.md should be written inside nested directory"
    );
}

#[test]
fn build_o_and_out_dir_mutually_exclusive() {
    // `-o` and `--out-dir` cannot be used together.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("foo.mds");
    std::fs::write(&src, "Hello!\n").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            "out.md",
            "--out-dir",
            "dist",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "-o and --out-dir should be mutually exclusive"
    );
}

#[test]
fn build_mds_json_output_dir() {
    // mds.json with `build.output_dir` controls where the output goes.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "out"}}"#,
    )
    .unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with mds.json output_dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let md_path = dir.path().join("out").join("hello.md");
    assert!(md_path.exists(), "hello.md should be in out/ per mds.json");
    let content = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        content.contains("Hello World!"),
        "mds.json output should contain compiled content, got: {content}"
    );
}

#[test]
fn build_mds_json_creates_output_dir() {
    // mds.json output_dir is auto-created when it doesn't exist.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tpl.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "generated/docs"}}"#,
    )
    .unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build with nested output_dir in mds.json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dir.path()
            .join("generated")
            .join("docs")
            .join("tpl.md")
            .exists(),
        "nested output_dir should be auto-created"
    );
}

#[test]
fn build_out_dir_overrides_mds_json() {
    // `--out-dir` takes precedence over mds.json output_dir.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "config_out"}}"#,
    )
    .unwrap();

    let cli_out = dir.path().join("cli_out");

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "--out-dir",
            cli_out.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "--out-dir should override mds.json; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // --out-dir wins: output in cli_out/, not config_out/
    assert!(
        cli_out.join("hello.md").exists(),
        "hello.md should be in cli_out/ (--out-dir wins over mds.json)"
    );
    assert!(
        !dir.path().join("config_out").join("hello.md").exists(),
        "config_out/ should not have hello.md when --out-dir overrides"
    );
}

#[test]
fn build_dash_o_overrides_mds_json() {
    // `-o <path>` takes precedence over mds.json output_dir.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "config_out"}}"#,
    )
    .unwrap();

    let explicit_out = dir.path().join("explicit.md");

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "-o",
            explicit_out.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "-o should override mds.json; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        explicit_out.exists(),
        "explicit.md should be created by -o flag"
    );
    assert!(
        !dir.path().join("config_out").exists(),
        "config_out/ should not be created when -o overrides"
    );
}

#[test]
fn build_invalid_mds_json_errors() {
    // Invalid JSON in mds.json should produce a hard error.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(dir.path().join("mds.json"), "{ invalid json }").unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "invalid mds.json should cause a build error"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("mds.json") || stderr.contains("invalid"),
        "error should mention mds.json or 'invalid', got: {stderr}"
    );
}

#[test]
fn build_empty_mds_json_uses_defaults() {
    // `{}` in mds.json is valid and falls back to default behavior (file next to source).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(dir.path().join("mds.json"), "{}").unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "empty mds.json should not cause errors; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Default: hello.md next to hello.mds
    assert!(
        dir.path().join("hello.md").exists(),
        "default output should be hello.md next to source"
    );
}

#[test]
fn build_mds_json_discovery_walks_up() {
    // mds.json found in a parent directory is used.
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("sub");
    std::fs::create_dir(&subdir).unwrap();

    let src = subdir.join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    // mds.json is in the parent directory
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "out"}}"#,
    )
    .unwrap();

    let output = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "mds.json in parent dir should be discovered; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // output_dir is resolved relative to mds.json location (parent dir)
    let md_path = dir.path().join("out").join("hello.md");
    assert!(
        md_path.exists(),
        "hello.md should be in <mds.json-dir>/out/, got path: {}",
        md_path.display()
    );
}

// ── Bare-filename regression tests (release blocker — effective_parent fix) ──

/// `mds build hello.mds` from the directory containing `hello.mds` must succeed.
/// Before the effective_parent fix, Path::parent() returned Some("") for a bare
/// filename, causing "".canonicalize() to fail with file_not_found on EVERY
/// subcommand when the user passed a bare relative filename as the argument.
#[test]
fn build_bare_filename_from_cwd_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.mds"), "Hello!\n").unwrap();

    let output = mds_bin()
        .current_dir(dir.path())
        .args(["build", "hello.mds"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "mds build <bare-filename> from cwd should succeed; stderr: {stderr}"
    );
    assert!(
        dir.path().join("hello.md").exists(),
        "hello.md should be created next to hello.mds"
    );
}

/// `mds check hello.mds` from the directory containing it must succeed.
#[test]
fn check_bare_filename_from_cwd_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.mds"), "Hello!\n").unwrap();

    let output = mds_bin()
        .current_dir(dir.path())
        .args(["check", "hello.mds"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "mds check <bare-filename> from cwd should succeed; stderr: {stderr}"
    );
}

/// `mds fmt hello.mds` from the directory containing it must succeed.
#[test]
fn fmt_bare_filename_from_cwd_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.mds"), "Hello!\n").unwrap();

    let output = mds_bin()
        .current_dir(dir.path())
        .args(["fmt", "hello.mds"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "mds fmt <bare-filename> from cwd should succeed; stderr: {stderr}"
    );
}

/// `mds lint hello.mds` from the directory containing it must succeed (exit 0 for clean file).
#[test]
fn lint_bare_filename_from_cwd_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    // Clean file: no lint findings expected.
    std::fs::write(dir.path().join("hello.mds"), "Hello!\n").unwrap();

    let output = mds_bin()
        .current_dir(dir.path())
        .args(["lint", "hello.mds"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "mds lint <bare-filename> from cwd should succeed for a clean file; stderr: {stderr}"
    );
}

/// `mds build --vars <file>` with a malformed JSON vars file exits 1 and the
/// error message names the vars file so the user knows which file to fix.
#[test]
fn vars_file_malformed_json_error_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    let vars = dir.path().join("vars.json");
    std::fs::write(&src, "Hello!\n").unwrap();
    // Write intentionally invalid JSON.
    std::fs::write(&vars, "{ not valid json }").unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "--vars",
            vars.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "malformed vars JSON should cause a non-zero exit"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "malformed vars JSON should exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("vars.json"),
        "error must name the vars file; got: {stderr}"
    );
    assert!(
        stderr.contains("mds::invalid_vars"),
        "error code must be mds::invalid_vars; got: {stderr}"
    );
}

/// `mds build --vars <file>` with a non-object JSON root (array) exits 1 and
/// the error message names the vars file.
#[test]
fn vars_file_non_object_json_error_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    let vars = dir.path().join("myvars.json");
    std::fs::write(&src, "Hello!\n").unwrap();
    // Write a JSON array — valid JSON but not a JSON object.
    std::fs::write(&vars, r#"["a", "b"]"#).unwrap();

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "--vars",
            vars.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "non-object vars JSON should cause a non-zero exit"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "non-object vars JSON should exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("myvars.json"),
        "error must name the vars file; got: {stderr}"
    );
    assert!(
        stderr.contains("mds::invalid_vars"),
        "error code must be mds::invalid_vars; got: {stderr}"
    );
}

/// `mds build --vars <missing-file>` exits 2 with `mds::file_not_found` (C1/F2).
///
/// Before this fix, `load_optional_vars_file` used `miette::miette!("{e}")` which
/// wrapped the typed `MdsError::FileNotFound` in an opaque report; `exit_code`
/// couldn't downcast it and fell through to exit 1 instead of 2.
#[test]
fn vars_file_missing_exits_2_with_file_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    // Point --vars at a path that doesn't exist.
    let missing = dir.path().join("no_such_vars_file_12345.json");

    let output = mds_bin()
        .args([
            "build",
            src.to_str().unwrap(),
            "--vars",
            missing.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(2),
        "missing vars file must exit 2 (file-system error, not content error); got: {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mds::file_not_found") || stderr.contains("file not found"),
        "error must be file-not-found; got: {stderr}"
    );
}

// ── stdin exit-code preservation (AD-211-5 / StdinRelabeledError ladder) ─────

/// `mds build -` on a stdin source whose `@import` target does not exist must
/// exit 2 (file-system error), not 1.
///
/// Error path: the resolver returns `MdsError::FileNotFound`; `compile_to_content`
/// wraps it in `StdinRelabeledError` via `relabel_stdin_error`; `build::exit_code`
/// must unwrap the wrapper before classifying.  Without the
/// `downcast_ref::<StdinRelabeledError>().map(inner)` ladder the outer downcast
/// for `MdsError` misses and the process silently exits 1.
#[test]
fn build_stdin_missing_import_exits_2() {
    let mut child = mds_bin()
        .args(["build", "-"])
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
        .write_all(b"@import \"./nonexistent_module_xyz_12345.mds\"\nHello!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "build stdin with missing @import must exit 2 (file-system error, not 1); got: {:?}",
        output.status.code()
    );
}

/// `mds check -` on a stdin source whose `@import` target does not exist must
/// exit 2 (file-system error), not 1.
///
/// Same ladder gap as `build_stdin_missing_import_exits_2`: `run_check` calls
/// `relabel_stdin_error` then propagates the wrapped `StdinRelabeledError`
/// to `build::exit_code`.
#[test]
fn check_stdin_missing_import_exits_2() {
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
        .write_all(b"@import \"./nonexistent_module_xyz_12345.mds\"\nHello!\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "check stdin with missing @import must exit 2 (file-system error, not 1); got: {:?}",
        output.status.code()
    );
}

/// `mds check -` on a stdin source that trips `MAX_TOTAL_ITERATIONS` must exit 3
/// (resource limit), not 1.
///
/// Error path: the evaluator returns `MdsError::ResourceLimit`; `run_check` wraps
/// it in `StdinRelabeledError`; `build::exit_code` must unwrap the wrapper to
/// reach the inner `ResourceLimit` variant and return 3.  Without the ladder the
/// process exits 1.
///
/// Source: nested `@for` loops — 1001 outer x 1000 inner = 1_001_000 total
/// iterations, which exceeds `MAX_TOTAL_ITERATIONS` (1_000_000).  Both arrays are
/// declared inline in the YAML frontmatter so no `--vars` flag is needed.
#[test]
fn check_stdin_resource_limit_exits_3() {
    let outer: Vec<String> = (0..1001usize).map(|i| format!("  - {i}")).collect();
    let inner: Vec<String> = (0..1000usize).map(|i| format!("  - {i}")).collect();
    let source = format!(
        "---\nouter:\n{}\ninner:\n{}\n---\n@for o in outer:\n@for i in inner:\n@end\n@end\n",
        outer.join("\n"),
        inner.join("\n")
    );

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
        .write_all(source.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "check stdin hitting MAX_TOTAL_ITERATIONS must exit 3 (resource limit, not 1); got: {:?}",
        output.status.code()
    );
}

// ── Bare-filename regression (PF-006 / issue #11) ────────────────────────────

/// `mds watch hello.mds` from the directory containing `hello.mds` must start up,
/// complete the initial compile, and write `hello.md` with the correct content.
///
/// PF-006 fifth sibling: bare-filename watch invocation.  The other four siblings
/// (build / check / fmt / lint) are above.  Watch is the one subcommand that
/// resolves parents through its own call sites; this test locks in that startup path.
///
/// Only asserts the INITIAL BUILD (bounded 10-second wait) — no event-timing
/// assertions that would be timing-flaky on Linux CI.
#[test]
fn watch_bare_filename_from_cwd_succeeds() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    // RAII guard — kills + waits the child on drop so the test never leaks processes.
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let dir = tempfile::tempdir().unwrap();
    // Use a distinguishable sentinel so "exit 0 + empty file" can't pass.
    std::fs::write(dir.path().join("hello.mds"), "Hello from watch!\n").unwrap();
    let out = dir.path().join("hello.md");

    let _child = ChildGuard(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "hello.mds", "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn mds watch"),
    );

    // Poll until the output file appears and contains the compiled content.
    // Bounded to 10 s; the initial compile typically finishes in < 100 ms.
    let deadline = Instant::now() + Duration::from_secs(10);
    let found = loop {
        if let Ok(content) = std::fs::read_to_string(&out) {
            if content.contains("Hello from watch!") {
                break true;
            }
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        found,
        "mds watch <bare-filename> from cwd should complete initial compile and write hello.md \
         containing 'Hello from watch!'"
    );
}

/// `load_config` must find `mds.json` in a grandparent directory when building
/// from a bare filename in a deeply nested subdirectory.
///
/// Regression for issue #11: a relative `start_dir` (`"."`) caused
/// `current.parent()` to return `None` after just one iteration, making any
/// `mds.json` beyond the immediate parent unreachable even when
/// `MAX_TRAVERSAL_DEPTH` would allow it.
///
/// Verifier: we configure `build.output_dir = "out"` in the root `mds.json`.
/// If `load_config` finds the config, output lands in `root/out/hello.md`.
/// If it silently skips it, output falls back to the input directory
/// (`root/sub/nested/hello.md`), failing the assertion.
#[test]
fn build_load_config_finds_grandparent_mds_json() {
    let root = tempfile::tempdir().unwrap();
    // Create: root/sub/nested/hello.mds
    let nested = root.path().join("sub").join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("hello.mds"), "Hello!\n").unwrap();
    // Place mds.json at root/ with an output_dir that differs from the input dir.
    std::fs::write(
        root.path().join("mds.json"),
        r#"{"build": {"output_dir": "out"}}"#,
    )
    .unwrap();

    let output = mds_bin()
        .arg("build")
        .arg("hello.mds") // bare filename — not an absolute path
        .current_dir(&nested)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build from bare filename in nested dir must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `load_config` found root/mds.json → output goes to root/out/hello.md.
    // Without the fix it would fall back to root/sub/nested/hello.md.
    let config_output = root.path().join("out").join("hello.md");
    assert!(
        config_output.exists(),
        "output must be in root/out/ (per grandparent mds.json); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── ESC injection regression (issue #176 / ESC-INJECTION) ─────────────────────

/// Regression gate: `mds build` (single-file mode) must not emit raw ESC bytes to
/// stderr when the source file embeds a raw ESC byte (U+001B) in content that reaches
/// `MdsError::Syntax`.
///
/// Single-file builds do not go through the per-file handler in `run_build_directory`;
/// the error propagates all the way to `main()`, which is the last-resort render boundary.
/// This test validates that boundary specifically.
///
/// Companion to `build_esc_byte_in_syntax_error_is_sanitized_on_stderr` which covers
/// directory mode (the first guarded path). Both paths must be sanitized to prevent
/// terminal escape injection from untrusted source files (PF-004 / ESC-INJECTION).
#[test]
fn build_single_file_esc_byte_in_syntax_error_is_sanitized_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("esc.mds");
    // Raw ESC byte (0x1B) in a .mds file that has a syntax error (unclosed @define).
    // The ESC is on the error line so miette renders it inside the source context frame.
    std::fs::write(&path, b"@define \x1bfoo:\nhello\n").unwrap();

    let out = mds_bin()
        .arg("build")
        .arg(&path) // single-file mode (not a directory)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // Build must fail (syntax error in the file).
    assert_ne!(
        out.status.code(),
        Some(0),
        "build with syntax error should fail; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Raw ESC byte (0x1B) must not appear anywhere in stderr.
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must be sanitized before writing to stderr (single-file mode); \
         got (hex): {:02x?}",
        &out.stderr[..out.stderr.len().min(512)]
    );
}

/// Regression gate: `mds build` (directory mode) must not emit raw ESC bytes to
/// stderr when a source file embeds a raw ESC byte (U+001B) in content that reaches
/// `MdsError::Syntax`.
///
/// Uses directory mode so the error is rendered by the per-file error handler in
/// `run_build_directory` (the `build.rs` render boundary that was vulnerable).
/// For single-file builds the error propagates through `main`; this test
/// validates the directory-mode path specifically.
#[test]
fn build_esc_byte_in_syntax_error_is_sanitized_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    // Raw ESC byte (0x1B) in a .mds file that has a syntax error (unclosed @define).
    // The ESC is on the error line so miette renders it inside the source context frame.
    std::fs::write(dir.path().join("esc.mds"), b"@define \x1bfoo:\nhello\n").unwrap();

    let out = mds_bin()
        .arg("build")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // Build must fail (syntax error in the file).
    assert_ne!(
        out.status.code(),
        Some(0),
        "build with syntax error should fail; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Raw ESC byte (0x1B) must not appear anywhere in stderr.
    assert!(
        !out.stderr.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must be sanitized before writing to stderr; \
         got (hex): {:02x?}",
        &out.stderr[..out.stderr.len().min(512)]
    );
}

// ── AC-224-14 / D2(a): build and fmt (dir) do not emit unknown-rule warnings ──
//
// Only `mds lint` reads `lint.rules` and emits an unknown-rule warning — single-file
// mode via `load_lint_config`, directory mode via `LintDirCtx::config_for`.
// `mds build` and `mds fmt <DIR>` read `mds.json` via `load_config` but deserialize
// the `lint` field without calling `load_lint_config`, so they never warn — an
// accepted D2(a) asymmetry (see build.rs:49-51 and CHANGELOG).  `mds check` and
// `mds fmt <FILE>` do not call `load_config` at all and never reach any lint-config
// path; their absence assertions are structural, not D2(a).
//
// The build and fmt (dir) tests mechanically hold the AC-224-14 watch-path invariant:
// `watch.rs:822` calls `load_config(...).unwrap_or(None)`.  Because `build` and
// `mds fmt <DIR>` share the same `load_config` implementation, a passing build or
// dir-fmt proves `load_config` returns `Ok` for configs with unknown rule names —
// so `unwrap_or(None)` cannot collapse `output_dir` to `None` on account of an
// unknown lint rule name alone.  Non-vacuity is secured by the positive-control
// arms in each test (ADR-009 / PF-013).

/// AC-224-14 / D2(a): `mds build` must NOT emit an unknown-rule warning even
/// when `mds.json` names a lint rule that does not exist.
///
/// Non-vacuity (ADR-009 / PF-013): a paired positive-control arm runs the same
/// config with an unknown SEVERITY value — `mds build` exits non-zero on that
/// config, proving `load_config` was reached and the JSON was parsed.  The main
/// assertion (unknown rule name → exits 0, no warning) is therefore non-vacuous:
/// `load_config` was called; only the warning was declined.  The complementary
/// positive control showing the warning IS emitted lives in `cli_lint.rs`.
#[test]
fn build_unknown_lint_rule_in_mds_json_emits_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tpl.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"lint":{"rules":{"no-such-rule-xyzzy":"warn"}}}"#,
    )
    .unwrap();

    let out = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "D2(a): mds build must succeed even with an unknown lint rule in mds.json; \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown lint rule"),
        "D2(a): mds build must NOT emit an unknown-rule warning; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("no-such-rule-xyzzy"),
        "D2(a): mds build must NOT name the unknown rule in stderr; stderr: {stderr}"
    );

    // Positive-control arm (ADR-009 / PF-013): an unknown SEVERITY value is a hard
    // serde parse error in load_config (Severity is a closed enum; "banana" is not a
    // recognised variant).  build must exit non-zero, proving load_config was called
    // and the JSON was actually parsed in the no-warning arm above.  If build ever
    // stops reading mds.json this arm will fail, surfacing the regression.
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"lint":{"rules":{"no-such-rule-xyzzy":"banana"}}}"#,
    )
    .unwrap();
    let out_bad = mds_bin()
        .arg("build")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !out_bad.status.success(),
        "D2(a) positive-control: mds build must exit non-zero on an unknown severity \
         value ('banana') in mds.json — this proves load_config was reached in the \
         no-warning arm; stderr: {}",
        String::from_utf8_lossy(&out_bad.stderr)
    );
}

/// AC-224-14: `mds check` must NOT emit an unknown-rule warning even
/// when `mds.json` names a lint rule that does not exist.
///
/// `mds check <FILE>` does not call `load_config` at all (see main.rs:288-290);
/// it never reads `mds.json`.  The absence is structural, not a D2(a) choice.
/// Non-vacuity (ADR-009 / PF-013): a positive-control arm confirms that even an
/// unknown SEVERITY value in `mds.json` leaves `mds check`'s exit code unchanged,
/// proving the file is not read rather than merely that a warning was declined.
#[test]
fn check_unknown_lint_rule_in_mds_json_emits_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tpl.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"lint":{"rules":{"no-such-rule-xyzzy":"warn"}}}"#,
    )
    .unwrap();

    let out = mds_bin()
        .arg("check")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "AC-224-14: mds check must succeed even with an unknown lint rule in mds.json; \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown lint rule"),
        "AC-224-14: mds check must NOT emit an unknown-rule warning; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("no-such-rule-xyzzy"),
        "AC-224-14: mds check must NOT name the unknown rule in stderr; stderr: {stderr}"
    );

    // Positive-control arm (ADR-009 / PF-013): write mds.json with an unknown
    // SEVERITY value ("banana" is not a recognised Severity variant) — a config
    // that would cause `mds build` to exit non-zero (hard serde parse error).
    // `mds check <FILE>` must still exit 0, proving that check never reads
    // mds.json at all.  If check ever starts reading the config, this arm will
    // fail, surfacing the regression.
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"lint":{"rules":{"no-such-rule-xyzzy":"banana"}}}"#,
    )
    .unwrap();
    let out_bad = mds_bin()
        .arg("check")
        .arg(&src)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out_bad.status.success(),
        "AC-224-14 positive-control: mds check must still exit 0 with an unknown \
         severity in mds.json — check does not read the config; \
         stderr: {}",
        String::from_utf8_lossy(&out_bad.stderr)
    );
}

/// AC-224-14 / D2(a): `mds fmt <DIR>` must NOT emit an unknown-rule warning even
/// when `mds.json` names a lint rule that does not exist.
///
/// `mds fmt <DIR>` calls `load_config` (fmt.rs:308) and thus follows the same
/// code path as `mds build` and `watch.rs:822`.  A passing test proves
/// `load_config` returns `Ok` for configs with unknown rule names.
/// Note: `mds fmt <FILE>` does NOT call `load_config`; only the directory target
/// exercises this code path.
/// Non-vacuity (ADR-009 / PF-013): a positive-control arm confirms that an
/// unknown SEVERITY value causes `mds fmt <DIR>` to exit non-zero, proving
/// `load_config` was reached and the JSON was parsed in the no-warning arm.
#[test]
fn fmt_unknown_lint_rule_in_mds_json_emits_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tpl.mds");
    std::fs::write(&src, "Hello!\n").unwrap();
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"lint":{"rules":{"no-such-rule-xyzzy":"warn"}}}"#,
    )
    .unwrap();

    // Target the DIRECTORY so load_config (fmt.rs:308) is actually called.
    // Targeting a single file bypasses load_config entirely.
    let out = mds_bin()
        .arg("fmt")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "D2(a): mds fmt must succeed even with an unknown lint rule in mds.json; \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown lint rule"),
        "D2(a): mds fmt must NOT emit an unknown-rule warning; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("no-such-rule-xyzzy"),
        "D2(a): mds fmt must NOT name the unknown rule in stderr; stderr: {stderr}"
    );

    // Positive-control arm (ADR-009 / PF-013): an unknown SEVERITY value is a hard
    // serde parse error in load_config (Severity is a closed enum).  mds fmt <DIR>
    // must exit non-zero, proving load_config was called in the no-warning arm above.
    std::fs::write(
        dir.path().join("mds.json"),
        r#"{"lint":{"rules":{"no-such-rule-xyzzy":"banana"}}}"#,
    )
    .unwrap();
    let out_bad = mds_bin()
        .arg("fmt")
        .arg(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !out_bad.status.success(),
        "D2(a) positive-control: mds fmt <DIR> must exit non-zero on an unknown \
         severity value ('banana') in mds.json — this proves load_config was reached \
         in the no-warning arm; stderr: {}",
        String::from_utf8_lossy(&out_bad.stderr)
    );
}
