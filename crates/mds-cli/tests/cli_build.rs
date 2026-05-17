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
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
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
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {name}!\n").expect("write fixture");

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
    std::fs::write(&mds_path, "---\nname: World\n---\nHello {name}!\n").unwrap();

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
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();

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
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();

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
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
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
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
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
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
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
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();

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
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
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
