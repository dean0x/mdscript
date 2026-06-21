//! CLI integration tests for `mds build --format messages`.
//!
//! Coverage:
//! - AC-2.1: `--format messages -o -` → valid pretty-printed JSON array
//! - AC-2.2: `--format messages -o out.json` → file written with valid JSON
//! - AC-2.3: default (no --format, `-o -`) → plain text, unchanged
//! - AC-2.4: `--format markdown -o -` → identical to default
//! - AC-2.5: `--format xml` (invalid) → non-zero exit, error lists valid values
//! - AC-2.6: template with NO @message blocks + `--format messages` → non-zero exit

mod common;
use common::{fixture, mds_bin};

// ── AC-2.1: --format messages → valid JSON array on stdout ───────────────────

#[test]
fn format_messages_to_stdout_is_valid_json_array() {
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
        output.status.success(),
        "build --format messages should succeed; stderr: {}",
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
}

// ── AC-2.2: --format messages -o file → file written with valid JSON ─────────

#[test]
fn format_messages_to_file_is_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("out.json");

    let output = mds_bin()
        .args([
            "build",
            fixture("messages.mds").to_str().unwrap(),
            "--format",
            "messages",
            "-o",
            out_path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build --format messages -o file should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(out_path.exists(), "output file should be created");

    let content = std::fs::read_to_string(&out_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("file contents must be valid JSON");
    assert!(parsed.is_array(), "file must contain a JSON array");
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 messages; got: {arr:#?}");
}

// ── AC-2.3: default (no --format) → plain text output ────────────────────────

#[test]
fn default_format_produces_plain_text() {
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
        "default build should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Must not be a JSON array.
    assert!(
        !stdout.trim_start().starts_with('['),
        "default output must not be a JSON array; got: {stdout:?}"
    );
    // Body content should be present (text mode renders @message body inline).
    assert!(
        stdout.contains("helpful") || stdout.contains("Hello"),
        "text-mode output should contain message bodies; got: {stdout:?}"
    );
}

// ── AC-2.4: --format markdown → identical to default ─────────────────────────

#[test]
fn format_markdown_produces_same_as_default() {
    let default_output = mds_bin()
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

    let markdown_output = mds_bin()
        .args([
            "build",
            fixture("messages.mds").to_str().unwrap(),
            "--format",
            "markdown",
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        default_output.status.success(),
        "default build should succeed"
    );
    assert!(
        markdown_output.status.success(),
        "build --format markdown should succeed; stderr: {}",
        String::from_utf8_lossy(&markdown_output.stderr)
    );

    let default_stdout = String::from_utf8(default_output.stdout).unwrap();
    let markdown_stdout = String::from_utf8(markdown_output.stdout).unwrap();

    assert_eq!(
        default_stdout, markdown_stdout,
        "--format markdown must produce identical output to the default"
    );
}

// ── AC-2.5: --format xml (invalid) → non-zero exit ───────────────────────────

#[test]
fn invalid_format_value_exits_nonzero_with_error() {
    let output = mds_bin()
        .args([
            "build",
            fixture("messages.mds").to_str().unwrap(),
            "--format",
            "xml",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "--format xml should exit non-zero"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    // clap should list valid values in the error.
    assert!(
        stderr.contains("markdown") || stderr.contains("messages") || stderr.contains("invalid"),
        "error should list valid format values; got: {stderr}"
    );
}

// ── AC-2.6: no @message blocks + --format messages → non-zero exit ───────────

#[test]
fn format_messages_without_message_blocks_exits_nonzero() {
    let output = mds_bin()
        .args([
            "build",
            fixture("simple.mds").to_str().unwrap(),
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
        "--format messages on a template with no @message blocks should fail"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("@message") || stderr.contains("message") || stderr.contains("no "),
        "error should mention missing @message blocks; got: {stderr}"
    );
}

// ── AC-2.1 via stdin: --format messages from stdin → valid JSON ───────────────

#[test]
fn format_messages_from_stdin_produces_valid_json() {
    let mut child = mds_bin()
        .args(["build", "-", "--format", "messages", "-o", "-"])
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
        "--format messages from stdin should succeed; stderr: {}",
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

// ── AC-1: symlinked entry rejected in --format messages ───────────────────────

/// Symlinked entry path in messages mode must be rejected with a symlink error
/// (proves the PR-A2 fix: entry now routes through resolve_path_messages →
/// NativeFs::check_symlink, the same canonicalize-comparison gate used by
/// compile_with_deps for markdown mode).
#[test]
#[cfg(unix)]
fn format_messages_rejects_symlinked_entry() {
    let dir = tempfile::tempdir().unwrap();

    // Real file with valid @message content
    let real_file = dir.path().join("real.mds");
    std::fs::write(&real_file, "@message system:\nYou are helpful.\n@end\n").unwrap();

    // Symlink pointing to real.mds
    let link_file = dir.path().join("link.mds");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    let output = mds_bin()
        .args([
            "build",
            link_file.to_str().unwrap(),
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
        "build --format messages with a symlinked entry must fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction; got: {stderr}"
    );
}

// ── AC-2: symlinked --vars file rejected ──────────────────────────────────────

#[test]
#[cfg(unix)]
fn format_messages_rejects_symlinked_vars_file() {
    let dir = tempfile::tempdir().unwrap();

    // A valid .mds file (entry is not symlinked)
    let entry = dir.path().join("chat.mds");
    std::fs::write(&entry, "@message user:\n{greeting}\n@end\n").unwrap();

    // A real vars.json
    let real_vars = dir.path().join("real_vars.json");
    std::fs::write(&real_vars, r#"{"greeting": "Hello!"}"#).unwrap();

    // A symlink pointing to the vars file
    let link_vars = dir.path().join("link_vars.json");
    std::os::unix::fs::symlink(&real_vars, &link_vars).unwrap();

    let output = mds_bin()
        .args([
            "build",
            entry.to_str().unwrap(),
            "--format",
            "messages",
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
        "build --vars with a symlinked vars file must fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction for vars file; got: {stderr}"
    );
}

// ── AC-6: directory rejected in messages mode ─────────────────────────────────

#[test]
fn format_messages_rejects_directory_input() {
    let dir = tempfile::tempdir().unwrap();

    let output = mds_bin()
        .args([
            "build",
            dir.path().to_str().unwrap(),
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
        "build --format messages on a directory must fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("directory") || stderr.contains("expected a file"),
        "error must mention directory restriction; got: {stderr}"
    );
}

// ── AC-6: stdin still works in messages mode ──────────────────────────────────
// (already covered by format_messages_from_stdin_produces_valid_json above,
//  but we add an explicit AC-6 label here for traceability)

// ── I11: oversized file → non-zero exit with clear error message ──────────────

#[test]
fn format_messages_rejects_oversized_file() {
    // MAX_FILE_SIZE is 10 MiB. Write a file just over that limit.
    let dir = tempfile::tempdir().unwrap();
    let big_file = dir.path().join("big.mds");

    // Write a valid header plus enough padding to exceed 10 MiB.
    let header = b"@message system:\nYou are helpful.\n@end\n";
    let padding_size = 10 * 1024 * 1024 + 1 - header.len();
    let mut contents = header.to_vec();
    contents.extend(std::iter::repeat_n(b' ', padding_size));

    std::fs::write(&big_file, &contents).unwrap();

    let output = mds_bin()
        .args([
            "build",
            big_file.to_str().unwrap(),
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
        "build --format messages should fail for a file exceeding MAX_FILE_SIZE; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("too large") || stderr.contains("max") || stderr.contains("bytes"),
        "error should mention file-size limit; got: {stderr}"
    );
}

// ── #90 boundary: dynamic role with special characters (CLI process layer) ────
//
// Content escaping is already covered at the core level (crates/mds-core/tests/messages.rs:448).
// This test covers the CLI-process layer boundary: the dynamic role with special chars
// survives the compile → JSON serialization → stdout → parse round-trip intact with
// correct message count and role content.

#[test]
fn format_messages_dynamic_role_special_chars_round_trip() {
    let dir = tempfile::tempdir().unwrap();

    // Use a vars file to inject the role with special JSON characters.
    // The role contains characters that are significant to JSON: double-quote and backslash.
    let vars_file = dir.path().join("vars.json");
    // Note: the role value in JSON must have these chars escaped; the compiled role should be
    // the literal string: ad"min\nuser
    std::fs::write(&vars_file, r#"{"role": "ad\"min\nuser"}"#).unwrap();

    let entry = dir.path().join("chat.mds");
    std::fs::write(&entry, "@message {role}:\nRequest received.\n@end\n").unwrap();

    let output = mds_bin()
        .args([
            "build",
            entry.to_str().unwrap(),
            "--format",
            "messages",
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

    // Message count must be exactly 1, unchanged by special characters in the role.
    assert_eq!(arr.len(), 1, "expected exactly 1 message; got: {arr:#?}");

    // The role value must round-trip exactly through JSON: the parsed string
    // must equal the original Go-string-value from the vars, not the raw JSON escape.
    // serde_json parses escape sequences, so we compare the decoded value.
    let role = arr[0]["role"].as_str().expect("role must be a string");
    assert_eq!(
        role, "ad\"min\nuser",
        "role must round-trip exactly with escaped chars; got: {role:?}"
    );

    // Content must be intact regardless of role special chars.
    let content = arr[0]["content"]
        .as_str()
        .expect("content must be a string");
    assert_eq!(content, "Request received.");
}
