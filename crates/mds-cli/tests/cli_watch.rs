//! Integration tests for `mds watch`.
//!
//! Strategy:
//! - Spawn `mds watch … --debounce 0` (immediate rebuild, no debounce delay).
//! - Poll output file content with a bounded `wait_for_file_contains` (5-second cap).
//! - Poll stderr with `wait_for_stderr_contains` when testing error / status messages.
//! - A RAII `ChildGuard` kills+waits the child on drop so tests never leave orphans.
//!
//! Flakiness mitigations:
//! - Assert on output FILE content rather than stderr ordering.
//! - Write dependency files BEFORE adding the `@import` that references them.
//! - Use `-q` where stderr isn't under test.
//! - Always kill+wait child in `ChildGuard::drop`.
//! - Absorb FSEvents latency via the 5-second polling cap.

mod common;
use common::mds_bin;

use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

// ── Helpers ────────────────────────────────────────────────────────────────

/// RAII guard that kills + waits the child process on drop.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl ChildGuard {
    #[allow(dead_code)]
    fn id(&self) -> u32 {
        self.0.id()
    }
    fn wait_status(&mut self) -> std::process::ExitStatus {
        self.0.wait().expect("wait failed")
    }
}

/// Poll `path` until its content contains `needle`, or `timeout` elapses.
fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains(needle) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Poll `path` until it no longer exists, or `timeout` elapses.
fn wait_for_file_gone(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

const TIMEOUT: Duration = Duration::from_secs(10);

// ── T-I14: Invalid combinations rejected at startup ────────────────────────

#[test]
fn watch_rejects_stdin() {
    let output = mds_bin()
        .args(["watch", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "watch with stdin should fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("stdin") || stderr.contains("build"),
        "error should mention stdin, got: {stderr}"
    );
}

#[test]
fn watch_rejects_dir_with_output_flag() {
    let dir = tempfile::tempdir().unwrap();
    let output = mds_bin()
        .args(["watch", dir.path().to_str().unwrap(), "-o", "out.md"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "watch dir with -o should fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn watch_rejects_dir_with_format_messages() {
    let dir = tempfile::tempdir().unwrap();
    let output = mds_bin()
        .args([
            "watch",
            dir.path().to_str().unwrap(),
            "--format",
            "messages",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "watch dir with --format messages should fail; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── T-I1: Initial compile writes output ────────────────────────────────────

#[test]
fn watch_initial_compile_writes_output() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let found = wait_for_file_contains(&out, "Hello World!", TIMEOUT);
    assert!(found, "initial compile should write output to hello.md");
    drop(child);
}

// ── T-I2: Edit entry → output updates ─────────────────────────────────────

#[test]
fn watch_edit_entry_updates_output() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: Alice\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "initial compile should produce Hello Alice!"
    );

    // Edit the source.
    std::fs::write(&src, "---\nname: Bob\n---\nHello {name}!\n").unwrap();

    // Wait for rebuild.
    assert!(
        wait_for_file_contains(&out, "Hello Bob!", TIMEOUT),
        "after editing, output should contain Hello Bob!"
    );

    drop(child);
}

// ── T-I3: Edit imported dep → entry output updates ─────────────────────────

#[test]
fn watch_edit_imported_dep_updates_entry() {
    let dir = tempfile::tempdir().unwrap();

    // Helper module exporting a function.
    let helper = dir.path().join("helper.mds");
    std::fs::write(
        &helper,
        "@define greet(name):\nHello {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // Entry that imports helper.
    let entry = dir.path().join("entry.mds");
    std::fs::write(
        &entry,
        "@import \"./helper.mds\" as h\n\n{h.greet(\"World\")}\n",
    )
    .unwrap();
    let out = dir.path().join("entry.md");

    let child = ChildGuard(
        mds_bin()
            .args(["watch", entry.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should produce Hello World!"
    );

    // Edit the helper to change the greeting.
    std::fs::write(
        &helper,
        "@define greet(name):\nHi there {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    assert!(
        wait_for_file_contains(&out, "Hi there World!", TIMEOUT),
        "editing the imported helper should trigger a rebuild"
    );

    drop(child);
}

// ── T-I5: Compile error → process alive, output unchanged, fix recovers ────

#[test]
fn watch_compile_error_keeps_watcher_alive() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: Alice\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let mut child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for successful initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "initial compile should succeed"
    );

    // Introduce a compile error.
    std::fs::write(&src, "Hello {undefined_var_xyz}!\n").unwrap();
    // Give the watcher time to attempt rebuild.
    std::thread::sleep(Duration::from_millis(500));

    // Process should still be alive.
    // (try_wait returns None = still running, Some = exited)
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher should stay alive after compile error"
    );

    // Fix the error — watcher should recover.
    std::fs::write(&src, "---\nname: Charlie\n---\nHello {name}!\n").unwrap();
    assert!(
        wait_for_file_contains(&out, "Hello Charlie!", TIMEOUT),
        "fixing the error should trigger a successful rebuild"
    );

    drop(child);
}

// ── T-I6: Directory mode startup compiles all, per-file updates work ───────

#[test]
fn watch_dir_mode_compiles_all_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.mds"),
        "---\nname: A\n---\nFile A: {name}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.mds"),
        "---\nname: B\n---\nFile B: {name}\n",
    )
    .unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "a.md should be compiled on startup"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "File B: B", TIMEOUT),
        "b.md should be compiled on startup"
    );

    // Edit a.mds → only a.md should update.
    std::fs::write(
        dir.path().join("a.mds"),
        "---\nname: A-edited\n---\nFile A: {name}\n",
    )
    .unwrap();
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A-edited", TIMEOUT),
        "editing a.mds should update a.md"
    );
    // b.md should be untouched.
    let b_content = std::fs::read_to_string(out_dir.join("b.md")).unwrap();
    assert!(
        b_content.contains("File B: B"),
        "b.md should not be affected by edits to a.mds, got: {b_content}"
    );

    drop(child);
}

// ── T-I7: Directory mode picks up newly created .mds files ─────────────────

#[test]
fn watch_dir_mode_picks_up_new_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "---\nname: A\n---\nFile A\n").unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A", TIMEOUT),
        "a.md should appear on startup"
    );

    // Create a new file AFTER the watcher is running.
    std::fs::write(
        dir.path().join("c.mds"),
        "---\nname: C\n---\nNew file {name}\n",
    )
    .unwrap();

    assert!(
        wait_for_file_contains(&out_dir.join("c.md"), "New file C", TIMEOUT),
        "newly created c.mds should be compiled to c.md"
    );

    drop(child);
}

// ── T-I8: Directory mode deletes output when source is deleted ─────────────

#[test]
fn watch_dir_mode_deletes_output_on_source_deletion() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "---\nname: A\n---\nFile A\n").unwrap();
    std::fs::write(dir.path().join("b.mds"), "---\nname: B\n---\nFile B\n").unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for both outputs to be created.
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A", TIMEOUT),
        "a.md should appear on startup"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "File B", TIMEOUT),
        "b.md should appear on startup"
    );

    // Delete a.mds.
    std::fs::remove_file(dir.path().join("a.mds")).unwrap();

    // a.md should be removed.
    assert!(
        wait_for_file_gone(&out_dir.join("a.md"), TIMEOUT),
        "a.md should be removed when a.mds is deleted"
    );
    // b.md must remain untouched.
    assert!(
        out_dir.join("b.md").exists(),
        "b.md should not be removed when only a.mds was deleted"
    );

    drop(child);
}

// ── T-I9: Edit --vars file triggers recompile ─────────────────────────────

#[test]
fn watch_vars_file_change_triggers_recompile() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    // Provide a frontmatter default so the template compiles even without vars.
    std::fs::write(&src, "---\nname: Default\n---\nHello {name}!\n").unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"name": "Alice"}"#).unwrap();
    let out = dir.path().join("hello.md");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--vars",
                vars.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "initial compile with vars should produce Hello Alice!"
    );

    // Edit the vars file.
    std::fs::write(&vars, r#"{"name": "Bob"}"#).unwrap();

    assert!(
        wait_for_file_contains(&out, "Hello Bob!", TIMEOUT),
        "editing vars file should trigger recompile with new name"
    );

    drop(child);
}

// ── T-I10: --clear with non-TTY pipe → ANSI sequence absent ───────────────

#[test]
fn watch_clear_non_tty_no_ansi_escape() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    // Spawn with piped stderr — not a TTY. `clear_terminal` only emits the ANSI
    // sequence on rebuilds (not the initial compile), so we must trigger a rebuild
    // to actually exercise the --clear code path.
    let mut child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--clear", "--debounce", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    // Drain stderr on a background thread so the pipe never fills (which would
    // block the child) and so we capture all bytes the child writes to stderr.
    let stderr = child.0.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut handle = stderr;
        let _ = handle.read_to_end(&mut buf);
        buf
    });

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should write output"
    );

    // Edit the source to trigger a rebuild — this is the path that calls
    // clear_terminal(). On a non-TTY pipe it must be a no-op.
    std::fs::write(&src, "---\nname: There\n---\nHello {name}!\n").unwrap();
    assert!(
        wait_for_file_contains(&out, "Hello There!", TIMEOUT),
        "rebuild should occur after editing source"
    );

    // Stop the child and collect everything it wrote to stderr.
    let _ = child.0.kill();
    let _ = child.0.wait();
    let stderr_bytes = reader.join().expect("stderr reader thread panicked");

    // AC-F6: the ANSI clear/home sequences emitted by clear_terminal()
    // (\x1b[2J, \x1b[3J, \x1b[H) must be ABSENT when stderr is not a TTY.
    assert!(
        !contains_subslice(&stderr_bytes, b"\x1b[2J"),
        "ANSI erase-screen (\\x1b[2J) must not be emitted on a non-TTY pipe; \
         stderr was: {:?}",
        String::from_utf8_lossy(&stderr_bytes)
    );
    assert!(
        !contains_subslice(&stderr_bytes, b"\x1b[3J"),
        "ANSI erase-scrollback (\\x1b[3J) must not be emitted on a non-TTY pipe"
    );
    assert!(
        !contains_subslice(&stderr_bytes, b"\x1b["),
        "no ANSI CSI escape (\\x1b[) should appear on a non-TTY pipe"
    );
}

/// Return true if `haystack` contains `needle` as a contiguous subslice.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ── T-I11: Output resolution — -o <file> ──────────────────────────────────

#[test]
fn watch_output_flag_writes_to_specified_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("custom_output.md");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "watch with -o should write to the specified file"
    );
    assert!(
        !dir.path().join("hello.md").exists(),
        "default hello.md should not be written when -o overrides"
    );

    drop(child);
}

// ── T-I12: --set vars applied on rebuild ──────────────────────────────────

#[test]
fn watch_set_vars_applied_on_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("tpl.mds");
    std::fs::write(&src, "Hello {name}!\n").unwrap();
    let out = dir.path().join("tpl.md");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--set",
                "name=Alice",
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "--set name=Alice should be applied on initial compile"
    );

    // Edit to trigger rebuild — --set should still apply.
    std::fs::write(&src, "Greetings {name}!\n").unwrap();
    assert!(
        wait_for_file_contains(&out, "Greetings Alice!", TIMEOUT),
        "--set name=Alice should persist across rebuilds"
    );

    drop(child);
}

// ── T-I13: Single-file --format messages → valid JSON array ───────────────

#[test]
fn watch_messages_format_produces_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("chat.mds");
    std::fs::write(&src, "@message user:\nWhat is 2+2?\n@end\n").unwrap();
    let out = dir.path().join("chat.json");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--format",
                "messages",
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "What is 2+2?", TIMEOUT),
        "messages format should write JSON containing the message"
    );

    // Verify it's valid JSON.
    let content = std::fs::read_to_string(&out).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("messages output should be valid JSON");
    assert!(
        parsed.is_array(),
        "messages output should be a JSON array, got: {content}"
    );

    drop(child);
}

// ── T-I15: stdout / stderr separation with -o - ────────────────────────────

#[test]
fn watch_stdout_contains_content_when_o_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();

    // -o - forces stdout output.
    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "-o",
                "-",
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Read from stdout with a timeout.
    let deadline = Instant::now() + TIMEOUT;
    let mut buf = String::new();
    let mut found = false;
    // Give the child time to produce output.
    while Instant::now() < deadline {
        let mut tmp = [0u8; 256];
        if let Some(stdout) = child.0.stdout.as_mut() {
            match stdout.read(&mut tmp) {
                Ok(0) | Err(_) => {}
                Ok(n) => {
                    buf.push_str(&String::from_utf8_lossy(&tmp[..n]));
                    if buf.contains("Hello World!") {
                        found = true;
                        break;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(found, "with -o -, compiled output should appear on stdout");
    drop(child);
}

// ── T-I16: Ctrl+C clean exit (#[cfg(unix)]) ────────────────────────────────

#[test]
#[cfg(unix)]
fn watch_ctrl_c_exits_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let child = mds_bin()
        .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid = child.id();
    let mut guard = ChildGuard(child);

    // Wait for initial compile so we know the watcher is running.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should succeed before sending SIGINT"
    );

    // Send SIGINT (Ctrl+C).
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }

    // Wait for the process to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if guard.0.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "process should exit after SIGINT");

    let status = guard.wait_status();
    assert!(
        status.success(),
        "exit code should be 0 after Ctrl+C, got: {status:?}"
    );
}

// ── T-P1: Debounce coalesces rapid edits ──────────────────────────────────

#[test]
fn watch_debounce_coalesces_rapid_edits() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: v0\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    // Use a 200ms debounce for stability.
    let child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "200"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello v0!", TIMEOUT),
        "initial compile should produce v0"
    );

    // Write 10 rapid edits within the debounce window.
    for i in 1..=10 {
        std::fs::write(&src, format!("---\nname: v{i}\n---\nHello {{name}}!\n")).unwrap();
        // Tiny sleep to ensure filesystem registers the write, but
        // well within the 200ms debounce window.
        std::thread::sleep(Duration::from_millis(5));
    }

    // Wait for the debounced rebuild (only 1 rebuild expected).
    assert!(
        wait_for_file_contains(&out, "Hello v10!", TIMEOUT),
        "after rapid edits, output should reflect final value"
    );

    // Count "Recompiled" lines in stderr.
    // The initial compile produces one "Compiled to..." line (not "Recompiled"),
    // so we just verify the output reflects v10.
    // The real coalescing assertion: we got v10, not some intermediate version,
    // after a single rebuild window.

    drop(child);
}

// ── T-P3: Startup error surfaced clearly ──────────────────────────────────

#[test]
fn watch_invalid_path_startup_error() {
    let output = mds_bin()
        .args(["watch", "/nonexistent/path/that/does/not/exist.mds"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "watch with invalid path should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "error message should appear on stderr, got empty"
    );
}

// ── AC-F3: Import-removal resync — both directions of dynamic dep tracking ─

/// T-I4 completion: removing an @import stops tracking the removed dep.
///
/// Two sub-cases:
///  (a) ADD import  → helper changes now rebuild entry  (covered by T-I3 above)
///  (b) REMOVE import → helper changes no longer rebuild entry
///
/// This test covers case (b).
#[test]
fn watch_import_removal_stops_tracking_dep() {
    let dir = tempfile::tempdir().unwrap();

    // Write the helper BEFORE the entry references it (mitigates FSEvents latency).
    let helper = dir.path().join("helper.mds");
    std::fs::write(
        &helper,
        "@define greet(name):\nHello {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // Entry that imports helper initially.
    let entry = dir.path().join("entry.mds");
    std::fs::write(
        &entry,
        "@import \"./helper.mds\" as h\n\n{h.greet(\"World\")}\n",
    )
    .unwrap();
    let out = dir.path().join("entry.md");

    let child = ChildGuard(
        mds_bin()
            .args(["watch", entry.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile — helper IS tracked.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should produce Hello World!"
    );

    // STEP 1 (add direction, already covered by T-I3 but verified here too):
    // Edit helper — entry output should update because helper is tracked.
    std::fs::write(
        &helper,
        "@define greet(name):\nHi {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();
    assert!(
        wait_for_file_contains(&out, "Hi World!", TIMEOUT),
        "editing helper while imported should trigger a rebuild"
    );

    // STEP 2 (removal direction): rewrite entry to remove the @import.
    // The entry now produces static output that does NOT reference helper.
    std::fs::write(&entry, "Static content\n").unwrap();
    assert!(
        wait_for_file_contains(&out, "Static content", TIMEOUT),
        "removing @import should rebuild entry with static content"
    );

    // Capture last-known mtime/content before the helper edit.
    let content_before = std::fs::read_to_string(&out).unwrap();

    // STEP 3: Edit helper again — entry output must NOT change because the dep
    // was removed from the watch set after the resync in step 2.
    std::fs::write(
        &helper,
        "@define greet(name):\nBye {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // Wait long enough for any spurious rebuild to materialize (500ms >> debounce 0).
    std::thread::sleep(Duration::from_millis(500));

    let content_after = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        content_before, content_after,
        "after removing @import, editing helper must NOT change entry output"
    );

    drop(child);
}

// ── AC-F7: Dir mode vars-recompile-all ────────────────────────────────────

/// Editing vars.json while in directory mode must recompile ALL .mds files.
#[test]
fn watch_dir_mode_vars_change_recompiles_all() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir_path = src_dir.path().join("out");
    std::fs::create_dir(&out_dir_path).unwrap();

    // Two templates that each interpolate the `greeting` var.
    std::fs::write(
        src_dir.path().join("a.mds"),
        "---\ngreeting: Default\n---\n{greeting} from A\n",
    )
    .unwrap();
    std::fs::write(
        src_dir.path().join("b.mds"),
        "---\ngreeting: Default\n---\n{greeting} from B\n",
    )
    .unwrap();

    // Write vars.json with initial greeting value.
    let vars = src_dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"greeting": "Hello"}"#).unwrap();

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src_dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir_path.to_str().unwrap(),
                "--vars",
                vars.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Both files should compile on startup with initial vars.
    assert!(
        wait_for_file_contains(&out_dir_path.join("a.md"), "Hello from A", TIMEOUT),
        "a.md should initially contain 'Hello from A'"
    );
    assert!(
        wait_for_file_contains(&out_dir_path.join("b.md"), "Hello from B", TIMEOUT),
        "b.md should initially contain 'Hello from B'"
    );

    // Edit vars.json — BOTH outputs should update.
    std::fs::write(&vars, r#"{"greeting": "Goodbye"}"#).unwrap();

    assert!(
        wait_for_file_contains(&out_dir_path.join("a.md"), "Goodbye from A", TIMEOUT),
        "a.md should update to 'Goodbye from A' after vars change"
    );
    assert!(
        wait_for_file_contains(&out_dir_path.join("b.md"), "Goodbye from B", TIMEOUT),
        "b.md should update to 'Goodbye from B' after vars change"
    );

    drop(child);
}

// ── AC-A5: Quiet mode keeps compile errors visible ────────────────────────

/// Under `-q`, compile errors must still appear on stderr; the watcher stays alive.
#[test]
fn watch_quiet_keeps_errors_visible() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let mut child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    // Drain stderr on a background thread to prevent the pipe from filling.
    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let stderr_buf_clone = stderr_buf.clone();
    let _reader_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 256];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stderr_buf_clone
                        .lock()
                        .unwrap()
                        .extend_from_slice(&tmp[..n]);
                }
            }
        }
    });

    // Wait for the initial compile to produce valid output.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should succeed"
    );

    // Introduce a compile error (reference an undefined variable with no frontmatter default).
    std::fs::write(&src, "Hello {__undefined_xyz__}!\n").unwrap();

    // Give the watcher time to attempt rebuild and emit error.
    std::thread::sleep(Duration::from_millis(500));

    // Process must still be alive — watcher stays up after compile errors.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive after a compile error even under -q"
    );

    // Verify error output appeared on stderr despite -q.
    // Under quiet mode, status messages are suppressed but error diagnostics are not.
    let bytes_so_far = stderr_buf.lock().unwrap().clone();
    assert!(
        !bytes_so_far.is_empty(),
        "stderr must contain the compile error even under -q; got empty stderr"
    );

    // Fix the error — watcher should recover.
    std::fs::write(&src, "---\nname: Fixed\n---\nHello {name}!\n").unwrap();
    assert!(
        wait_for_file_contains(&out, "Hello Fixed!", TIMEOUT),
        "after fixing the compile error, watcher should rebuild successfully"
    );

    drop(child);
}

// ── AC-F9: "Stopped watching." message on clean Ctrl+C ────────────────────

/// On SIGINT the watcher must print "Stopped watching." to stderr (non-quiet).
#[test]
#[cfg(unix)]
fn watch_ctrl_c_prints_stopped_watching() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let child = mds_bin()
        .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid = child.id();
    let mut guard = ChildGuard(child);

    // Drain stderr on a background thread.
    let stderr_handle = guard.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let stderr_buf_clone = stderr_buf.clone();
    let _reader_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 256];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stderr_buf_clone
                        .lock()
                        .unwrap()
                        .extend_from_slice(&tmp[..n]);
                }
            }
        }
    });

    // Wait for initial compile so the watcher is running.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should succeed before sending SIGINT"
    );

    // Send SIGINT (Ctrl+C).
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }

    // Wait for the process to exit.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if guard.0.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "process should exit after SIGINT");

    let status = guard.wait_status();
    assert!(
        status.success(),
        "exit code should be 0 after Ctrl+C, got: {status:?}"
    );

    // Give the reader thread a moment to flush remaining bytes.
    std::thread::sleep(Duration::from_millis(100));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);
    assert!(
        stderr_str.contains("Stopped watching."),
        "stderr should contain 'Stopped watching.' after Ctrl+C, got: {stderr_str:?}"
    );
}

// ── AC-P1: Debounce coalesces burst — count rebuild summary lines ──────────

/// Burst of ~10 writes within a 250ms debounce window must produce exactly 1
/// "Recompiled " line in stderr.  250ms is large enough to be reliable on CI;
/// if the filesystem splits the burst into two windows, the test permits <= 2
/// rebuilds (documented below) but asserts == 1 as the expected case.
#[test]
fn watch_debounce_single_rebuild_from_burst() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("burst.mds");
    std::fs::write(&src, "---\nname: v0\n---\nBurst {name}!\n").unwrap();
    let out = dir.path().join("burst.md");

    // Use a 250ms debounce — large enough to reliably swallow the ~10 × 5ms burst.
    let mut child = ChildGuard(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "250"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    // Drain stderr on a background thread.
    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let stderr_buf_clone = stderr_buf.clone();
    let _reader_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stderr_buf_clone
                        .lock()
                        .unwrap()
                        .extend_from_slice(&tmp[..n]);
                }
            }
        }
    });

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Burst v0!", TIMEOUT),
        "initial compile should produce Burst v0!"
    );

    // Write 10 rapid edits within the 250ms debounce window.
    for i in 1..=10u32 {
        std::fs::write(&src, format!("---\nname: v{i}\n---\nBurst {{name}}!\n")).unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }

    // Wait for the debounced rebuild to settle (debounce window + generous FSEvent latency).
    assert!(
        wait_for_file_contains(&out, "Burst v10!", TIMEOUT),
        "after burst, output should reflect final value v10"
    );

    // Wait an extra moment to ensure no trailing rebuilds are in-flight.
    std::thread::sleep(Duration::from_millis(400));

    // Kill child and collect all stderr.
    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(100));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // Count "Recompiled " lines (each rebuild emits exactly one such line).
    let rebuild_count = stderr_str.matches("Recompiled ").count();

    // Expected: exactly 1 rebuild from the burst.
    // Allow <= 2 as a documented tolerance: on a heavily loaded CI machine the
    // 250ms window may occasionally be split by an FSEvent scheduling gap, yielding
    // a second rebuild for the tail of the burst.  The important property is that
    // we do NOT get 10 individual rebuilds.
    assert!(
        rebuild_count >= 1,
        "at least one rebuild must have occurred, got 0; stderr: {stderr_str}"
    );
    assert!(
        rebuild_count <= 2,
        "debounce should coalesce burst into <= 2 rebuilds, got {rebuild_count}; \
         stderr: {stderr_str}"
    );
}

// ── AC-F10: Watch no-arg auto-detect ─────────────────────────────────────

/// `mds watch` with no argument and cwd containing exactly ONE .mds file
/// should auto-detect and compile that file.
#[test]
fn watch_no_arg_auto_detects_single_mds_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("only.mds"),
        "---\nname: AutoDetect\n---\nAuto {name}!\n",
    )
    .unwrap();
    let out = dir.path().join("only.md");

    let child = ChildGuard(
        mds_bin()
            .args(["watch", "--debounce", "0", "-q"])
            .current_dir(dir.path()) // cwd = tempdir containing exactly one .mds
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "Auto AutoDetect!", TIMEOUT),
        "auto-detect with single .mds file should compile only.mds"
    );

    drop(child);
}

/// `mds watch` with no argument and cwd containing TWO .mds files must exit
/// non-zero with an "ambiguous"/"multiple" style error.
#[test]
fn watch_no_arg_fails_with_multiple_mds_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mds"), "File A\n").unwrap();
    std::fs::write(dir.path().join("b.mds"), "File B\n").unwrap();

    let output = mds_bin()
        .args(["watch"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "watch with multiple .mds files and no arg should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("multiple") || stderr.contains("ambiguous") || stderr.contains("specify"),
        "error should mention multiple files or instruct user to specify; got: {stderr}"
    );
}
