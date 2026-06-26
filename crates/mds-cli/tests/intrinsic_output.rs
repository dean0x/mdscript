//! CLI integration tests for intrinsic output format (AC-FUNC-09, -11, -14, -25).
//!
//! The output shape (Markdown or JSON messages) is determined by what the template
//! compiles to — there is no `--format` flag. Coverage:
//!
//! - AC-FUNC-09: `messages.mds -o -` → valid pretty-printed JSON array with trailing newline
//! - AC-FUNC-09: `messages.mds` (no -o) → `messages.json` created next to source
//! - AC-FUNC-09: `plain.mds -o -` → plain Markdown text (not JSON)
//! - AC-FUNC-11: `-o path.md` on messages template → warns, writes verbatim to `.md`
//! - AC-FUNC-11: `-o path.json` on plain template → warns, writes verbatim to `.json`
//! - AC-FUNC-14: stdin with @message + `-o -` → JSON array on stdout
//! - AC-FUNC-14: stdin with @message + `--out-dir` → `output.json` in dir
//! - AC-FUNC-25: mixed-content (text + @message) → non-zero exit
//! - Unknown `--format` flag → non-zero exit (clap rejects it)
//! - Oversized file → non-zero exit with clear error
//! - Symlinked entry → non-zero exit (security gate)
//! - Symlinked --vars file → non-zero exit (security gate)
//! - Directory input → non-zero exit
//! - Dynamic role with special chars round-trips through JSON correctly

mod common;
use common::{fixture, mds_bin};

// ── AC-FUNC-09: messages.mds → stdout is a pretty JSON array ─────────────────

#[test]
fn messages_to_stdout_is_valid_json_array() {
    let output = mds_bin()
        .args([
            "build",
            fixture("messages.mds").to_str().unwrap(),
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build messages.mds -o - should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Must parse as a JSON array.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    assert!(
        parsed.is_array(),
        "output must be a JSON array; got: {stdout}"
    );
    let arr = parsed.as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "expected 2 messages (system + user); got: {arr:#?}"
    );

    // Verify first message structure.
    assert_eq!(arr[0]["role"].as_str().unwrap(), "system");
    assert!(
        arr[0]["content"].as_str().unwrap().contains("helpful"),
        "system message content should mention 'helpful'; got: {:?}",
        arr[0]["content"]
    );

    // Verify second message structure.
    assert_eq!(arr[1]["role"].as_str().unwrap(), "user");
    assert_eq!(arr[1]["content"].as_str().unwrap(), "Hello!");

    // Must be pretty-printed (contains newlines/indentation).
    assert!(
        stdout.contains('\n'),
        "output should be pretty-printed (contain newlines); got: {stdout:?}"
    );

    // AC-FUNC-09: trailing newline after the JSON array.
    assert!(
        stdout.ends_with('\n'),
        "output should end with a trailing newline; got: {stdout:?}"
    );
}

// ── AC-FUNC-09: messages.mds (no -o) → messages.json next to source ──────────

#[test]
fn messages_default_output_creates_json_file() {
    let dir = tempfile::tempdir().unwrap();
    // Copy the fixture into a temp dir so we don't pollute the fixture directory.
    let src = dir.path().join("messages.mds");
    std::fs::copy(fixture("messages.mds"), &src).unwrap();

    let output = mds_bin()
        .args(["build", src.to_str().unwrap()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build messages.mds should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json_path = dir.path().join("messages.json");
    assert!(
        json_path.exists(),
        "messages.json should be created next to source"
    );

    let content = std::fs::read_to_string(&json_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("messages.json must be valid JSON");
    assert!(parsed.is_array(), "messages.json must contain a JSON array");
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 messages; got: {arr:#?}");
}

// ── AC-FUNC-09: plain.mds -o - → Markdown text (not JSON) ───────────────────

#[test]
fn plain_template_to_stdout_is_markdown() {
    let output = mds_bin()
        .args(["build", fixture("plain.mds").to_str().unwrap(), "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build plain.mds -o - should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Must NOT be a JSON array — markdown is emitted as plain text.
    assert!(
        !stdout.trim_start().starts_with('['),
        "plain template output must not be a JSON array; got: {stdout:?}"
    );
    assert!(
        !stdout.is_empty(),
        "plain template must produce non-empty output; got: {stdout:?}"
    );
}

// ── AC-FUNC-11: -o path.md on messages template → warn, write verbatim ───────

#[test]
fn explicit_output_path_with_wrong_ext_for_messages_warns_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("output.md");

    let output = mds_bin()
        .args([
            "build",
            fixture("messages.mds").to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // Must still succeed — explicit -o is used verbatim.
    assert!(
        output.status.success(),
        "-o with mismatched ext should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // File is written to the requested path.
    assert!(
        out_path.exists(),
        "output.md should be created at explicit path"
    );

    // Stderr should contain a warning about the extension mismatch.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("json")
            || stderr.contains("warn")
            || stderr.contains("extension")
            || stderr.contains("mismatch"),
        "stderr should warn about extension mismatch; got: {stderr}"
    );

    // Content should be valid JSON (it's messages, just written with .md extension).
    let content = std::fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("explicit-path output must be valid JSON");
    assert!(parsed.is_array(), "content must be a JSON array");
}

// ── AC-FUNC-11: -o path.json on plain template → warn, write verbatim ────────

#[test]
fn explicit_output_path_with_wrong_ext_for_plain_warns_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("output.json");

    let output = mds_bin()
        .args([
            "build",
            fixture("plain.mds").to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    // Must still succeed.
    assert!(
        output.status.success(),
        "-o with mismatched ext on plain template should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // File is written to the requested path.
    assert!(
        out_path.exists(),
        "output.json should be created at explicit path"
    );

    // Stderr should contain a warning about the extension mismatch.
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("md")
            || stderr.contains("warn")
            || stderr.contains("extension")
            || stderr.contains("mismatch"),
        "stderr should warn about extension mismatch; got: {stderr}"
    );

    // Content should be Markdown text (not JSON), just written with .json extension.
    let content = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&content).is_err(),
        "output.json content should not be JSON for a plain template; got: {content:?}"
    );
}

// ── AC-FUNC-14: stdin with @message + -o - → JSON array on stdout ────────────

#[test]
fn stdin_messages_template_produces_json_on_stdout() {
    let mut child = mds_bin()
        .args(["build", "-", "-o", "-"])
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
        .write_all(b"@message system:\nYou are helpful.\n@end\n@message user:\nHello!\n@end\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdin messages should compile to JSON; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdin messages output must be valid JSON");
    assert!(parsed.is_array(), "stdin output must be a JSON array");
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 messages; got: {arr:#?}");
    assert_eq!(arr[0]["role"].as_str().unwrap(), "system");
    assert_eq!(arr[1]["role"].as_str().unwrap(), "user");
}

// ── AC-FUNC-14: stdin with @message + --out-dir → output.json in dir ─────────

#[test]
fn stdin_messages_with_out_dir_creates_output_json() {
    let dir = tempfile::tempdir().unwrap();

    let mut child = mds_bin()
        .args(["build", "-", "--out-dir", dir.path().to_str().unwrap()])
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
        .write_all(b"@message system:\nYou are helpful.\n@end\n")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdin messages with --out-dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json_path = dir.path().join("output.json");
    assert!(
        json_path.exists(),
        "output.json should be created in --out-dir for stdin messages"
    );

    let content = std::fs::read_to_string(&json_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("output.json must be valid JSON");
    assert!(parsed.is_array(), "output.json must contain a JSON array");
}

// ── AC-FUNC-25: mixed-content → non-zero exit ────────────────────────────────

#[test]
fn mixed_content_template_build_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let mixed = dir.path().join("mixed.mds");
    // This template has both top-level text AND @message blocks — that is mixed content.
    std::fs::write(
        &mixed,
        "Some top-level text\n@message user:\nHello!\n@end\n",
    )
    .unwrap();

    let output = mds_bin()
        .args(["build", mixed.to_str().unwrap(), "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "mixed-content template should fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── AC-FUNC-25: mixed-content → mds check exits nonzero ─────────────────────

#[test]
fn mixed_content_template_check_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let mixed = dir.path().join("mixed.mds");
    std::fs::write(
        &mixed,
        "Some top-level text\n@message user:\nHello!\n@end\n",
    )
    .unwrap();

    let output = mds_bin()
        .args(["check", mixed.to_str().unwrap()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "mds check on mixed-content template should fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── Unknown --format flag → non-zero exit (clap rejects it) ──────────────────

#[test]
fn unknown_format_flag_exits_nonzero() {
    let output = mds_bin()
        .args([
            "build",
            fixture("messages.mds").to_str().unwrap(),
            "--format",
            "messages",
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "--format flag must be rejected (unknown arg); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("format")
            || stderr.contains("unexpected")
            || stderr.contains("unknown")
            || stderr.contains("unrecognized"),
        "error should mention the unknown --format flag; got: {stderr}"
    );
}

// ── Oversized file → non-zero exit with clear error ──────────────────────────

#[test]
fn oversized_file_exits_nonzero_with_size_error() {
    let dir = tempfile::tempdir().unwrap();
    let big_file = dir.path().join("big.mds");

    // Write a valid header plus enough padding to exceed 10 MiB.
    let header = b"@message system:\nYou are helpful.\n@end\n";
    let padding_size = 10 * 1024 * 1024 + 1 - header.len();
    let mut contents = header.to_vec();
    contents.extend(std::iter::repeat_n(b' ', padding_size));
    std::fs::write(&big_file, &contents).unwrap();

    let output = mds_bin()
        .args(["build", big_file.to_str().unwrap(), "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build should fail for a file exceeding MAX_FILE_SIZE; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("too large") || stderr.contains("max") || stderr.contains("bytes"),
        "error should mention file-size limit; got: {stderr}"
    );
}

// ── Symlinked entry → non-zero exit (security gate) ──────────────────────────

#[test]
#[cfg(unix)]
fn symlinked_entry_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    let real_file = dir.path().join("real.mds");
    std::fs::write(&real_file, "@message system:\nYou are helpful.\n@end\n").unwrap();

    let link_file = dir.path().join("link.mds");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    let output = mds_bin()
        .args(["build", link_file.to_str().unwrap(), "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with a symlinked entry must fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction; got: {stderr}"
    );
}

// ── Symlinked --vars file → non-zero exit (security gate) ────────────────────

#[test]
#[cfg(unix)]
fn symlinked_vars_file_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    let entry = dir.path().join("chat.mds");
    std::fs::write(&entry, "@message user:\n{greeting}\n@end\n").unwrap();

    let real_vars = dir.path().join("real_vars.json");
    std::fs::write(&real_vars, r#"{"greeting": "Hello!"}"#).unwrap();

    let link_vars = dir.path().join("link_vars.json");
    std::os::unix::fs::symlink(&real_vars, &link_vars).unwrap();

    let output = mds_bin()
        .args([
            "build",
            entry.to_str().unwrap(),
            "--vars",
            link_vars.to_str().unwrap(),
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build with a symlinked --vars file must fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction for vars file; got: {stderr}"
    );
}

// ── Directory input + -o flag → non-zero exit (dir mode rejects -o) ──────────

#[test]
fn directory_input_with_output_flag_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    // Directory mode does not support -o/--output; must use --out-dir instead.
    let output = mds_bin()
        .args(["build", dir.path().to_str().unwrap(), "-o", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "build <dir> -o - must fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("directory") || stderr.contains("out-dir"),
        "error must mention directory mode restriction; got: {stderr}"
    );
}

// ── Dynamic role with special characters round-trips correctly ────────────────

#[test]
fn dynamic_role_special_chars_round_trip() {
    let dir = tempfile::tempdir().unwrap();

    let vars_file = dir.path().join("vars.json");
    // role value is: ad"min\nuser (double-quote and newline)
    std::fs::write(&vars_file, r#"{"role": "ad\"min\nuser"}"#).unwrap();

    let entry = dir.path().join("chat.mds");
    std::fs::write(&entry, "@message {role}:\nRequest received.\n@end\n").unwrap();

    let output = mds_bin()
        .args([
            "build",
            entry.to_str().unwrap(),
            "--vars",
            vars_file.to_str().unwrap(),
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "dynamic role with special chars must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output must be valid JSON");
    let arr = parsed.as_array().expect("output must be a JSON array");

    assert_eq!(arr.len(), 1, "expected exactly 1 message; got: {arr:#?}");

    let role = arr[0]["role"].as_str().expect("role must be a string");
    assert_eq!(
        role, "ad\"min\nuser",
        "role must round-trip exactly with escaped chars; got: {role:?}"
    );

    let content = arr[0]["content"]
        .as_str()
        .expect("content must be a string");
    assert_eq!(content, "Request received.");
}
