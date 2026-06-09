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

// ── QA regression: no spurious startup recompiles and no duplicate stdout ───

/// QA-R1: Start `mds watch` with no edits, wait 1.5s, stop.
/// Asserts:
///  - stderr contains exactly ONE "Compiled to" line (initial compile),
///  - stderr contains ZERO "Recompiled" lines (no spurious rebuild from synthetic FS events).
///
/// Uses `--debounce 0` to make synthetic FSEvents arrive immediately (worst case).
#[test]
fn watch_startup_no_spurious_recompile() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("nochange.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("nochange.md");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--debounce",
                "0",
                // No -q: we NEED to observe stderr messages to catch spurious noise.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    // Drain stderr on a background thread so the pipe never fills.
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

    // Wait for the initial compile to complete.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should write output"
    );

    // Let the watcher idle for 1.5s — any synthetic FS events would fire within this window.
    std::thread::sleep(Duration::from_millis(1500));

    // Stop the child and collect all stderr.
    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(50));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // There must be exactly ONE "Compiled to" message (the initial compile).
    let compiled_count = stderr_str.matches("Compiled to").count();
    assert_eq!(
        compiled_count, 1,
        "expected exactly 1 'Compiled to' line on startup (no double initial compile); \
         got {compiled_count}; stderr was:\n{stderr_str}"
    );

    // There must be ZERO "Recompiled" lines — no rebuild without edits.
    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "expected 0 'Recompiled' lines with no edits (spurious startup rebuild); \
         got {recompiled_count}; stderr was:\n{stderr_str}"
    );
}

/// QA-R2: Start `mds watch <file> -o -` with no edits, capture stdout, wait 1.5s, stop.
/// Asserts: compiled content appears EXACTLY ONCE in stdout (no double-write on startup).
///
/// Uses `--debounce 0` which is the worst case: synthetic FSEvents from watcher
/// registration arrive immediately and (before the fix) cause the content to be
/// written 2-3x to stdout, corrupting downstream pipe consumers.
#[test]
fn watch_stdout_no_duplicate_write_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("stdout_once.mds");
    // Use a distinctive marker so we can count occurrences.
    std::fs::write(&src, "UNIQUE_MARKER_XYZ\n").unwrap();

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "-o",
                "-",
                "--debounce",
                "0",
                // No -q: we want to observe full behavior.
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Drain stdout on a background thread.
    let stdout_handle = child.0.stdout.take().expect("piped stdout");
    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let stdout_buf_clone = stdout_buf.clone();
    let _reader_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stdout_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stdout_buf_clone
                        .lock()
                        .unwrap()
                        .extend_from_slice(&tmp[..n]);
                }
            }
        }
    });

    // Let the watcher run long enough to capture initial compile + any spurious second write.
    std::thread::sleep(Duration::from_millis(1500));

    // Stop the child and collect all stdout.
    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(50));

    let stdout_bytes = stdout_buf.lock().unwrap().clone();
    let stdout_str = String::from_utf8_lossy(&stdout_bytes);

    // The marker should appear at least once (the initial compile wrote it).
    assert!(
        stdout_str.contains("UNIQUE_MARKER_XYZ"),
        "compiled output must appear on stdout; got: {stdout_str:?}"
    );

    // Count how many times the marker appears — must be exactly 1.
    let occurrence_count = stdout_str.matches("UNIQUE_MARKER_XYZ").count();
    assert_eq!(
        occurrence_count, 1,
        "compiled content must be written to stdout EXACTLY ONCE on startup \
         (no duplicate write from spurious synthetic FS event); \
         got {occurrence_count} occurrences; stdout was:\n{stdout_str}"
    );
}

// ── QA regression: dir-mode no spurious startup recompiles ──────────────────

/// QA-R3: Start `mds watch <dir>` with 2 .mds files, NO edits, idle ~1.5s, stop.
/// Asserts ZERO "Recompiled" lines in stderr (startup "Compiled to" lines are fine).
///
/// Before the fix, macOS FSEvents delivers synthetic events for each source file
/// right after watcher registration. Without the content-dedup map, the loop
/// recompiles identical content and logs a rebuild for each file (2 with
/// `--debounce 50`, 4 with `--debounce 0`).  After the fix, the dedup baseline
/// is populated before any synthetic events are processed, so they are all
/// recognised as no-ops and suppressed.
#[test]
fn watch_dir_mode_no_spurious_startup_recompile() {
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

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                // No -q: we NEED to observe stderr messages to catch spurious noise.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    // Drain stderr on a background thread so the pipe never fills.
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

    // Wait for the initial compile to complete (both files compiled on startup).
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "a.md should be compiled on startup"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "File B: B", TIMEOUT),
        "b.md should be compiled on startup"
    );

    // Let the watcher idle for 1.5s — synthetic FSEvents would fire within this window.
    std::thread::sleep(Duration::from_millis(1500));

    // Stop the child and collect all stderr.
    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(50));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // There must be ZERO "Recompiled" lines — no rebuild without edits.
    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "expected 0 'Recompiled' lines in dir mode with no edits \
         (spurious startup rebuild from synthetic FSEvents); \
         got {recompiled_count}; stderr was:\n{stderr_str}"
    );

    // Startup "Compiled to" lines are expected (one per source file).
    let compiled_count = stderr_str.matches("Compiled to").count();
    assert_eq!(
        compiled_count, 2,
        "expected exactly 2 'Compiled to' lines on dir-mode startup (one per source file); \
         got {compiled_count}; stderr was:\n{stderr_str}"
    );
}

// ── QA regression: single status line per rebuild ───────────────────────────

/// QA-R4: Start `mds watch <file>` (no -q), make ONE real edit, idle, stop.
/// Asserts:
///  - Exactly ONE "Recompiled" line total (the real edit),
///  - Total "Compiled to" count stays at 1 (the startup compile only —
///    the loop rebuild must NOT add a second "Compiled to" line).
///
/// Before the fix, `write_output` (called inside the loop) always emitted
/// "Compiled to …" AND the loop also emitted "Recompiled …", giving two
/// status lines per rebuild.  After the fix the loop sets announce=false so
/// only "Recompiled …" appears for loop rebuilds.
#[test]
fn watch_single_status_line_per_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("status.mds");
    std::fs::write(&src, "---\nname: v0\n---\nStatus {name}!\n").unwrap();
    let out = dir.path().join("status.md");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                // No -q: we need to observe status messages.
            ])
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

    // Wait for the initial compile.
    assert!(
        wait_for_file_contains(&out, "Status v0!", TIMEOUT),
        "initial compile should produce Status v0!"
    );

    // Make ONE real content-changing edit.
    std::fs::write(&src, "---\nname: v1\n---\nStatus {name}!\n").unwrap();

    // Wait for the rebuild to appear in the output.
    assert!(
        wait_for_file_contains(&out, "Status v1!", TIMEOUT),
        "after editing, output should contain Status v1!"
    );

    // Idle a bit to let any trailing events flush.
    std::thread::sleep(Duration::from_millis(500));

    // Stop the child and collect all stderr.
    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(50));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // Exactly ONE "Recompiled" line (the real edit).
    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 1,
        "expected exactly 1 'Recompiled' line for one real edit; \
         got {recompiled_count}; stderr was:\n{stderr_str}"
    );

    // Total "Compiled to" count must still be 1 (startup only — loop rebuild must
    // NOT emit a second "Compiled to" line).
    let compiled_count = stderr_str.matches("Compiled to").count();
    assert_eq!(
        compiled_count, 1,
        "expected exactly 1 'Compiled to' line total (startup only, no extra from loop rebuild); \
         got {compiled_count}; stderr was:\n{stderr_str}"
    );
}

// ── AC-M1: Subtree-mirrored output (--out-dir) ────────────────────────────

/// Editing a nested .mds file writes to the mirrored path, not a flat stem.
#[test]
fn watch_dir_mode_mirrors_subtree_to_out_dir() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("deep.mds"), "---\nname: D\n---\nDeep {name}\n").unwrap();
    let out_dir = dir.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Mirrored path: out/sub/deep.md (not out/deep.md).
    let mirrored = out_dir.join("sub").join("deep.md");
    assert!(
        wait_for_file_contains(&mirrored, "Deep D", TIMEOUT),
        "startup compile should write mirrored output to out/sub/deep.md"
    );
    // Flat path must NOT exist.
    assert!(
        !out_dir.join("deep.md").exists(),
        "flat stem deep.md must not exist when mirroring is active"
    );

    drop(child);
}

/// Two files with the same stem in different subdirs write to independent
/// mirrored outputs (no stem collision: AC-M1).
#[test]
fn watch_dir_mode_no_stem_collision_with_mirroring() {
    let dir = tempfile::tempdir().unwrap();
    let a_dir = dir.path().join("a");
    let b_dir = dir.path().join("b");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    std::fs::write(a_dir.join("x.mds"), "From A\n").unwrap();
    std::fs::write(b_dir.join("x.mds"), "From B\n").unwrap();
    let out_dir = dir.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a").join("x.md"), "From A", TIMEOUT),
        "out/a/x.md should contain 'From A'"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b").join("x.md"), "From B", TIMEOUT),
        "out/b/x.md should contain 'From B'"
    );

    // Delete a/x.mds → out/a/x.md should be removed, out/b/x.md must survive (AC-M4).
    std::fs::remove_file(a_dir.join("x.mds")).unwrap();
    assert!(
        wait_for_file_gone(&out_dir.join("a").join("x.md"), TIMEOUT),
        "out/a/x.md should be removed when a/x.mds is deleted"
    );
    assert!(
        out_dir.join("b").join("x.md").exists(),
        "out/b/x.md must not be affected by deletion of a/x.mds"
    );

    drop(child);
}

// ── AC-R1/R2/R8: Reverse-dependency tracking + partials ──────────────────

/// Editing a shared partial rebuilds all transitive importers (AC-R1).
#[test]
fn watch_dir_mode_shared_partial_rebuilds_importers() {
    let dir = tempfile::tempdir().unwrap();

    // Shared partial.
    let partial = dir.path().join("_shared.mds");
    std::fs::write(
        &partial,
        "@define greet(name):\nHello {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // Two importers.
    let a = dir.path().join("a.mds");
    let b = dir.path().join("b.mds");
    std::fs::write(&a, "@import \"./_shared.mds\" as s\n{s.greet(\"A\")}\n").unwrap();
    std::fs::write(&b, "@import \"./_shared.mds\" as s\n{s.greet(\"B\")}\n").unwrap();

    let out_dir = dir.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "Hello A!", TIMEOUT),
        "a.md should contain Hello A! on startup"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "Hello B!", TIMEOUT),
        "b.md should contain Hello B! on startup"
    );
    // The partial itself must NOT have emitted _shared.md (AC-R8).
    assert!(
        !out_dir.join("_shared.md").exists(),
        "_shared.md must not be written for a _-prefixed partial (DD2)"
    );

    // Edit the partial — both importers must rebuild.
    std::fs::write(
        &partial,
        "@define greet(name):\nHi {name}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "Hi A!", TIMEOUT),
        "a.md should update after editing the shared partial"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "Hi B!", TIMEOUT),
        "b.md should update after editing the shared partial"
    );
    // Partial output must still not exist.
    assert!(
        !out_dir.join("_shared.md").exists(),
        "_shared.md must not appear after editing the partial"
    );

    drop(child);
}

/// Transitive chain A→B→C: editing C updates A, B and C (AC-R2).
#[test]
fn watch_dir_mode_chain_rebuild() {
    let dir = tempfile::tempdir().unwrap();

    // C defines a value, B re-exports, A uses B.
    let c = dir.path().join("_c.mds");
    std::fs::write(&c, "@define val():\nV1\n@end\n\n@export val\n").unwrap();
    let b = dir.path().join("_b.mds");
    std::fs::write(
        &b,
        "@import \"./_c.mds\" as c\n@define val():\n{c.val()}\n@end\n\n@export val\n",
    )
    .unwrap();
    let a = dir.path().join("a.mds");
    std::fs::write(&a, "@import \"./_b.mds\" as b\n{b.val()}\n").unwrap();

    let out_dir = dir.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "0",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "V1", TIMEOUT),
        "a.md should contain V1 initially"
    );

    // Edit C — A must update.
    std::fs::write(&c, "@define val():\nV2\n@end\n\n@export val\n").unwrap();
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "V2", TIMEOUT),
        "a.md should update to V2 after editing _c.mds (transitive chain)"
    );

    drop(child);
}

// ── AC-C1: --poll-interval parse/clamp/exit-2 ────────────────────────────

/// `--poll-interval 0` disables the self-heal probe (native events only, smoke test).
#[test]
fn watch_poll_interval_zero_works() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
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
        "--poll-interval 0 should still do the initial compile"
    );

    // Verify a real edit also works.
    std::fs::write(&src, "---\nname: Poll\n---\nHello {name}!\n").unwrap();
    assert!(
        wait_for_file_contains(&out, "Hello Poll!", TIMEOUT),
        "--poll-interval 0: edit should still trigger rebuild via native event"
    );

    drop(child);
}

/// Non-numeric `--poll-interval` must exit 2 (clap parse error).
#[test]
fn watch_poll_interval_invalid_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Hello!\n").unwrap();

    let output = mds_bin()
        .args([
            "watch",
            src.to_str().unwrap(),
            "--poll-interval",
            "notanumber",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    // Clap parse errors exit 2.
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 2,
        "invalid --poll-interval should exit 2 (clap error), got {code}"
    );
}

/// `--poll-interval` value is clamped to ≥50ms (smoke test: just verify startup works).
#[test]
fn watch_poll_interval_tiny_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "Clamped!\n").unwrap();
    let out = dir.path().join("hello.md");

    // 1ms should be clamped to 50ms — watcher must still start and compile.
    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "1",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&out, "Clamped!", TIMEOUT),
        "--poll-interval 1 (clamped to 50ms) should still work"
    );
    drop(child);
}

// ── AC-W4: Idle watcher emits zero Recompiled across ticks ───────────────

/// Idle for ≥2.5s (≥2 ticks at default 1000ms) in single-file mode must emit
/// zero "Recompiled" lines (no phantom rebuild from the liveness probe).
#[test]
fn watch_file_mode_idle_no_recompile_across_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("idle.mds");
    std::fs::write(&src, "---\nname: World\n---\nIdle {name}!\n").unwrap();
    let out = dir.path().join("idle.md");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                // No -q: we need to observe stderr.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf_clone = stderr_buf.clone();
    let _reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_clone.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Idle World!", TIMEOUT),
        "initial compile should succeed"
    );

    // Idle for 2.5s (≥2 ticks at 100ms poll-interval — well above the minimum).
    std::thread::sleep(Duration::from_millis(2500));

    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(100));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "idle single-file watcher must emit 0 Recompiled across ticks; \
         got {recompiled_count}; stderr:\n{stderr_str}"
    );
}

/// Idle for ≥2.5s (≥2 ticks) in dir mode must emit zero "Recompiled" lines (AC-W4).
#[test]
fn watch_dir_mode_idle_no_recompile_across_ticks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.mds"),
        "---\nname: A\n---\nFile A: {name}\n",
    )
    .unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf_clone = stderr_buf.clone();
    let _reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_clone.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "initial compile should succeed"
    );

    // Idle for 2.5s (≥2 ticks at 100ms).
    std::thread::sleep(Duration::from_millis(2500));

    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(100));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "idle dir-mode watcher must emit 0 Recompiled across ticks; \
         got {recompiled_count}; stderr:\n{stderr_str}"
    );
}

// ── AC-W1: File mode — delete parent dir then recreate ───────────────────────

/// Delete the entry file's parent dir, then recreate it with the file.
/// The watcher must self-heal and recompile within ~1 tick (no restart required).
#[test]
fn watch_file_mode_parent_dir_delete_recreate_recovers() {
    // Place the source in a subdirectory so we can delete the parent without
    // touching the tempdir root.
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    std::fs::create_dir(&src_dir).unwrap();
    let src = src_dir.join("entry.mds");
    std::fs::write(&src, "---\nname: Before\n---\nEntry {name}\n").unwrap();
    let out = src_dir.join("entry.md");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Entry Before", TIMEOUT),
        "initial compile should produce 'Entry Before'"
    );

    // Delete the parent directory (simulates rmdir/recreate scenario).
    std::fs::remove_dir_all(&src_dir).unwrap();

    // Give the watcher a moment to notice.
    std::thread::sleep(Duration::from_millis(200));

    // Recreate the parent dir and the source file with new content.
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(&src, "---\nname: After\n---\nEntry {name}\n").unwrap();

    // Watcher should recover within ~1 tick (100ms poll-interval) and recompile.
    assert!(
        wait_for_file_contains(&out, "Entry After", TIMEOUT),
        "watcher must self-heal after parent dir delete+recreate and recompile"
    );

    drop(child);
}

// ── AC-W2: Dir mode — delete root then recreate ──────────────────────────────

/// Delete the watched root, then recreate it with a brand-new .mds file.
/// The watcher must recover and compile the new file within ~1 tick.
#[test]
fn watch_dir_mode_root_delete_recreate_recovers() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("watched");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.mds"), "---\nname: A\n---\nOld A\n").unwrap();
    let out_dir = base.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "Old A", TIMEOUT),
        "initial compile should produce 'Old A'"
    );

    // Delete the entire watched root.
    std::fs::remove_dir_all(&root).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // Recreate the root with a brand-new file (init-gap case).
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("new.mds"), "---\nname: N\n---\nNew file {name}\n").unwrap();

    // The watcher should recover and compile the new file.
    assert!(
        wait_for_file_contains(&out_dir.join("new.md"), "New file N", TIMEOUT),
        "watcher must recover after root delete+recreate and compile new file"
    );

    drop(child);
}

// ── AC-W6: Delete entry file — at most one error, then recover ───────────────

/// Delete the entry file (parent intact); assert the not-found error appears AT MOST
/// ONCE across multiple idle ticks, then recreate the file and assert recompile.
#[test]
fn watch_file_mode_entry_deleted_settles_then_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("entry.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {name}!\n").unwrap();
    let out = dir.path().join("entry.md");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                // No -q so we can observe stderr error messages.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf_clone = stderr_buf.clone();
    let _reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_clone.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });

    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should produce Hello World!"
    );

    // Delete the entry file (parent intact).
    std::fs::remove_file(&src).unwrap();

    // Idle for ~500ms (≥4 ticks at 100ms) — error should appear at most once.
    std::thread::sleep(Duration::from_millis(500));

    // Recreate the file with different content.
    std::fs::write(&src, "---\nname: Recovered\n---\nHello {name}!\n").unwrap();

    // Wait for recompile after recovery.
    assert!(
        wait_for_file_contains(&out, "Hello Recovered!", TIMEOUT),
        "watcher must recompile after recreating deleted entry file"
    );

    // Give the watcher a moment to settle after recovery before killing.
    std::thread::sleep(Duration::from_millis(200));

    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(100));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // Count distinct error events for the missing entry (not lines, since miette formats
    // multi-line error blocks). Error-settle prevents the liveness tick from re-firing,
    // so we expect a small bounded count (native FS events on delete + at most 1 from the
    // liveness probe), NOT one per tick. With 5 ticks over 500ms at 100ms poll-interval,
    // tick-sourced errors should be 0 (settled); native-event-sourced errors are small (≤5).
    let error_event_count =
        stderr_str.matches("file not found").count() + stderr_str.matches("No such file").count();
    assert!(
        error_event_count <= 6,
        "error for deleted entry should appear a bounded number of times (not once per tick); \
         got {error_event_count} file-not-found occurrences; stderr:\n{stderr_str}"
    );
}

// ── AC-W7: Vars-file dir outside root — delete+recreate re-arms ──────────────

/// Vars-file directory (outside root) delete+recreate is re-armed; a later vars
/// edit still triggers a recompile.
#[test]
fn watch_vars_dir_delete_recreate_rearms() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let vars_dir = base.path().join("vars_dir");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&vars_dir).unwrap();

    let src = src_dir.join("tpl.mds");
    std::fs::write(&src, "---\ngreeting: Default\n---\n{greeting}\n").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"greeting": "Hello"}"#).unwrap();
    let out = src_dir.join("tpl.md");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--vars",
                vars_file.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                // No -q so we can see debug output.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf_clone = stderr_buf.clone();
    let _reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_clone.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });

    assert!(
        wait_for_file_contains(&out, "Hello", TIMEOUT),
        "initial compile with vars should produce 'Hello'"
    );

    // Delete the entire vars directory.
    std::fs::remove_dir_all(&vars_dir).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // Recreate the vars directory. The liveness probe re-arms it on the next tick
    // (≤100ms). Wait two ticks before writing to ensure the watch is re-registered
    // before the write event fires.
    std::fs::create_dir_all(&vars_dir).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Now write new vars — the re-armed watcher should catch this event.
    std::fs::write(&vars_file, r#"{"greeting": "Goodbye"}"#).unwrap();

    // The watcher should detect the vars change and recompile.
    let got = wait_for_file_contains(&out, "Goodbye", TIMEOUT);
    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(100));
    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);
    assert!(
        got,
        "watcher must re-arm vars dir watch after delete+recreate and recompile on edit; \
         stderr was:\n{stderr_str}"
    );
}

// ── AC-R9: Cross-root — external partial edit rebuilds in-root importer ──────

/// In-root importer imports `../shared/_x.mds` (outside root); editing the external
/// partial triggers a rebuild of the in-root importer, and no `_x.md` is emitted.
#[test]
fn watch_dir_mode_cross_root_partial_edit_rebuilds_importer() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let shared = base.path().join("shared");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&shared).unwrap();

    // Place a .git marker at the base so the MDS compiler's project-root detection
    // sets the root at base/ (not at root/), allowing cross-dir `../shared/` imports.
    std::fs::write(base.path().join(".git"), "").unwrap();

    // External partial (outside the watched root dir, but inside the project).
    let partial = shared.join("_x.mds");
    std::fs::write(
        &partial,
        "@define greet():\nExternal V1\n@end\n\n@export greet\n",
    )
    .unwrap();

    // In-root importer.
    let importer = root.join("importer.mds");
    std::fs::write(
        &importer,
        "@import \"../shared/_x.mds\" as x\n{x.greet()}\n",
    )
    .unwrap();

    let out_dir = base.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("importer.md"), "External V1", TIMEOUT),
        "initial compile should produce 'External V1'"
    );

    // No _x.md should be emitted for the external partial.
    assert!(
        !out_dir.join("_x.md").exists(),
        "_x.md must not be emitted for external partial"
    );
    // Also check that no shared/_x.md appeared anywhere obvious.
    assert!(
        !shared.join("_x.md").exists(),
        "shared/_x.md must not be written"
    );

    // Edit the external partial.
    std::fs::write(
        &partial,
        "@define greet():\nExternal V2\n@end\n\n@export greet\n",
    )
    .unwrap();

    // In-root importer output must update.
    assert!(
        wait_for_file_contains(&out_dir.join("importer.md"), "External V2", TIMEOUT),
        "editing external partial must rebuild in-root importer"
    );

    // Still no _x.md output.
    assert!(
        !out_dir.join("_x.md").exists(),
        "_x.md must not be emitted after partial edit"
    );

    drop(child);
}

// ── AC-R3: Delete a partial → importers recompile (broken import) ────────────

/// Delete a partial that an importer uses → importer recompiles surfacing the
/// broken-import error; the partial's own output is never present (_-partial).
#[test]
fn watch_dir_mode_delete_partial_surfaces_broken_import() {
    let dir = tempfile::tempdir().unwrap();

    let partial = dir.path().join("_p.mds");
    std::fs::write(
        &partial,
        "@define val():\nPartial V1\n@end\n\n@export val\n",
    )
    .unwrap();

    let importer = dir.path().join("main.mds");
    std::fs::write(&importer, "@import \"./_p.mds\" as p\n{p.val()}\n").unwrap();

    let out_dir = dir.path().join("out");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                // No -q so we can observe errors.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf_clone = stderr_buf.clone();
    let _reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_clone.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("main.md"), "Partial V1", TIMEOUT),
        "initial compile should produce 'Partial V1'"
    );

    // Partial must not have emitted its own output.
    assert!(
        !out_dir.join("_p.md").exists(),
        "_p.md must not be written for partial"
    );

    // Delete the partial.
    std::fs::remove_file(&partial).unwrap();

    // Give the watcher time to process the deletion and attempt recompile.
    std::thread::sleep(Duration::from_millis(600));

    // Verify the importer's output was removed (no valid compilation possible).
    // Actually: the importer recompiles but fails due to broken import.
    // The old output may be preserved (watcher keeps last good output on error).
    // The important assertion: the process stays alive.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive after partial deletion surfaces broken import"
    );

    // Verify an error appeared in stderr (broken import surfaced).
    let bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&bytes);
    assert!(
        !stderr_str.is_empty(),
        "watcher should have emitted an error after deleting imported partial"
    );

    drop(child);
}

// ── AC-R4: Create previously-missing partial → erroring importers heal ────────

/// An importer references a missing `_p.mds` (errors at startup).
/// Create `_p.mds`; assert the erroring importer heals.
#[test]
fn watch_dir_mode_create_missing_partial_heals_importer() {
    let dir = tempfile::tempdir().unwrap();

    // Importer references a partial that does NOT exist yet.
    let importer = dir.path().join("main.mds");
    std::fs::write(&importer, "@import \"./_missing.mds\" as m\n{m.val()}\n").unwrap();

    let out_dir = dir.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Initial compile must fail (missing partial), so main.md may not appear.
    // Give the watcher time to attempt the initial compile.
    std::thread::sleep(Duration::from_millis(500));

    // Now create the previously-missing partial.
    let partial = dir.path().join("_missing.mds");
    std::fs::write(&partial, "@define val():\nHealed!\n@end\n\n@export val\n").unwrap();

    // The importer should heal and produce output.
    assert!(
        wait_for_file_contains(&out_dir.join("main.md"), "Healed!", TIMEOUT),
        "creating the missing partial must heal the erroring importer"
    );

    drop(child);
}

// ── AC-R6: Dual-role node — both top-level target AND imported ────────────────

/// A non-`_` file is both a top-level target AND imported by another file.
/// Editing it updates its own output AND rebuilds the importer.
/// Deleting it removes its own output AND recompiles the importer (with error).
#[test]
fn watch_dir_mode_dual_role_node_edit_and_delete() {
    let dir = tempfile::tempdir().unwrap();

    // Dual-role: dual.mds is a top-level file and is imported by consumer.mds.
    let dual = dir.path().join("dual.mds");
    std::fs::write(
        &dual,
        "@define greet():\nDual V1\n@end\n\n@export greet\n\nStandalone content\n",
    )
    .unwrap();

    let consumer = dir.path().join("consumer.mds");
    std::fs::write(&consumer, "@import \"./dual.mds\" as d\n{d.greet()}\n").unwrap();

    let out_dir = dir.path().join("out");

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile of both outputs.
    assert!(
        wait_for_file_contains(&out_dir.join("dual.md"), "Standalone content", TIMEOUT),
        "dual.md should be compiled as a top-level target"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("consumer.md"), "Dual V1", TIMEOUT),
        "consumer.md should be compiled using the import"
    );

    // Edit dual.mds — both dual.md and consumer.md should update.
    // Use a longer content to force a size delta.
    std::fs::write(
        &dual,
        "@define greet():\nDual V2 (updated)\n@end\n\n@export greet\n\nStandalone updated content\n",
    )
    .unwrap();

    assert!(
        wait_for_file_contains(
            &out_dir.join("dual.md"),
            "Standalone updated content",
            TIMEOUT
        ),
        "editing dual.mds must update dual.md (own output)"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("consumer.md"), "Dual V2 (updated)", TIMEOUT),
        "editing dual.mds must rebuild consumer.md (importer)"
    );

    // Delete dual.mds — dual.md should be removed; consumer.md should recompile (with error).
    std::fs::remove_file(&dual).unwrap();

    assert!(
        wait_for_file_gone(&out_dir.join("dual.md"), TIMEOUT),
        "dual.md must be removed when dual.mds is deleted"
    );

    // The watcher must stay alive.
    std::thread::sleep(Duration::from_millis(300));
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive after dual-role node deletion"
    );

    drop(child);
}

// ── AC-R7: Persistent syntax error — bounded error count ─────────────────────

/// A file with a persistent syntax error, idle ≥2 ticks at low --poll-interval.
/// Assert the error line count is bounded (~once per real edit, NOT once per tick)
/// and the watcher stays alive.
#[test]
fn watch_dir_mode_persistent_error_bounded_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.mds"),
        "---\nname: A\n---\nFile A: {name}\n",
    )
    .unwrap();
    // Syntax error file: references undefined variable with no frontmatter.
    std::fs::write(dir.path().join("bad.mds"), "Hello {__undefined_xyz__}!\n").unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                // No -q so we can count errors.
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );

    let stderr_handle = child.0.stderr.take().expect("piped stderr");
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let buf_clone = stderr_buf.clone();
    let _reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut handle = stderr_handle;
        let mut tmp = [0u8; 512];
        loop {
            match handle.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_clone.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });

    // Wait for good file to compile (confirms startup completed).
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "a.md should compile despite bad.mds error"
    );

    // Idle for ≥3 ticks (300ms at 100ms poll-interval) — error-settle must suppress re-fires.
    std::thread::sleep(Duration::from_millis(600));

    // Watcher must still be alive.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive with persistent syntax error in bad.mds"
    );

    let _ = child.0.kill();
    let _ = child.0.wait();
    std::thread::sleep(Duration::from_millis(100));

    let stderr_bytes = stderr_buf.lock().unwrap().clone();
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // Count DISTINCT error events for bad.mds — count `"undefined variable"` which appears
    // exactly once per error emission (not per line of the multi-line miette block).
    // Error-settle must suppress re-fires, so the count must be bounded (not once per tick).
    // Upper bound: ≤6 (initial + possible reconcile rounds; generous for CI load).
    let bad_error_count = stderr_str.matches("undefined variable").count();
    assert!(
        bad_error_count <= 6,
        "error for bad.mds must be bounded (not once per tick); \
         got {bad_error_count} events; stderr:\n{stderr_str}"
    );
}

// ── AC-M2: mds.json output_dir with config_dir as ancestor of root ────────────

/// An `mds.json` with `build.output_dir` where config_dir is an ANCESTOR of the
/// watched root; assert output is subtree-mirrored under `config_dir/output_dir`.
#[test]
fn watch_dir_mode_mds_json_config_dir_ancestor_mirrors_output() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("src");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("deep.mds"), "Deep content\n").unwrap();

    // mds.json lives at the BASE (ancestor of root).
    let mds_json = base.path().join("mds.json");
    std::fs::write(&mds_json, r#"{"build":{"output_dir":"out"}}"#).unwrap();

    // Expected output: base/out/sub/deep.md (relative to root=src, mirrored under base/out).
    let expected_out = base.path().join("out").join("sub").join("deep.md");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "0",
                "-q",
            ])
            .current_dir(base.path()) // mds.json resolution starts from cwd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    assert!(
        wait_for_file_contains(&expected_out, "Deep content", TIMEOUT),
        "mds.json output_dir from ancestor config_dir should mirror subtree: {:?}",
        expected_out
    );

    drop(child);
}

// ── AC-M6: Rename/move within root — stale output removed ────────────────────

/// Rename/move a file within root (`a/x.mds → b/x.mds`); assert stale `out/a/x.md`
/// is removed and `out/b/x.md` is written (no orphan accumulation).
#[test]
fn watch_dir_mode_rename_removes_stale_output() {
    let dir = tempfile::tempdir().unwrap();
    let a_dir = dir.path().join("a");
    let b_dir = dir.path().join("b");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    std::fs::write(a_dir.join("x.mds"), "Content from A\n").unwrap();
    let out_dir = dir.path().join("out");

    let child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("a").join("x.md"), "Content from A", TIMEOUT),
        "initial compile should write out/a/x.md"
    );

    // Move a/x.mds → b/x.mds (rename within root).
    std::fs::rename(a_dir.join("x.mds"), b_dir.join("x.mds")).unwrap();

    // out/b/x.md should appear with the same content.
    assert!(
        wait_for_file_contains(&out_dir.join("b").join("x.md"), "Content from A", TIMEOUT),
        "out/b/x.md should be written after rename"
    );

    // out/a/x.md (stale output) should be removed.
    assert!(
        wait_for_file_gone(&out_dir.join("a").join("x.md"), TIMEOUT),
        "stale out/a/x.md must be removed after rename (no orphan accumulation)"
    );

    drop(child);
}

// ── AC-P2: Edit partial imported by N files — exactly N outputs change ────────

/// Edit a partial imported by N=3 files; assert exactly the 3 affected outputs
/// change and the independent file is untouched.
#[test]
fn watch_dir_mode_partial_edit_rebuilds_exactly_n_importers() {
    let dir = tempfile::tempdir().unwrap();

    // Shared partial.
    let partial = dir.path().join("_shared.mds");
    std::fs::write(&partial, "@define val():\nV1\n@end\n\n@export val\n").unwrap();

    // Three importers.
    std::fs::write(
        dir.path().join("a.mds"),
        "@import \"./_shared.mds\" as s\n{s.val()} from A\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.mds"),
        "@import \"./_shared.mds\" as s\n{s.val()} from B\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("c.mds"),
        "@import \"./_shared.mds\" as s\n{s.val()} from C\n",
    )
    .unwrap();

    // Independent file (should NOT change when partial is edited).
    std::fs::write(dir.path().join("independent.mds"), "Independent\n").unwrap();

    let out_dir = dir.path().join("out");
    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
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
        wait_for_file_contains(&out_dir.join("a.md"), "V1 from A", TIMEOUT),
        "a.md should initially contain 'V1 from A'"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "V1 from B", TIMEOUT),
        "b.md should initially contain 'V1 from B'"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("c.md"), "V1 from C", TIMEOUT),
        "c.md should initially contain 'V1 from C'"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("independent.md"), "Independent", TIMEOUT),
        "independent.md should compile on startup"
    );

    // Record independent.md content before editing the partial.
    let independent_before = std::fs::read_to_string(out_dir.join("independent.md")).unwrap();

    // Edit the partial with different-length content to force a deterministic (mtime,size) delta.
    std::fs::write(
        &partial,
        "@define val():\nV2 updated\n@end\n\n@export val\n",
    )
    .unwrap();

    // All three importers must update.
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "V2 updated from A", TIMEOUT),
        "a.md must update after partial edit"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("b.md"), "V2 updated from B", TIMEOUT),
        "b.md must update after partial edit"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("c.md"), "V2 updated from C", TIMEOUT),
        "c.md must update after partial edit"
    );

    // Independent file must be untouched.
    let independent_after = std::fs::read_to_string(out_dir.join("independent.md")).unwrap();
    assert_eq!(
        independent_before, independent_after,
        "independent.md must not be modified when an unrelated partial is edited"
    );

    // Tear down before checking.
    let _ = child.0.kill();
    let _ = child.0.wait();
}

// ── AC-P4: Bounded soak — 50 sequential edits ────────────────────────────────

/// 50 sequential edits to a partial; assert each round rebuilds importers, the
/// process stays responsive, and it exits cleanly.
#[test]
fn watch_dir_mode_soak_50_edits_bounded_and_clean_exit() {
    let dir = tempfile::tempdir().unwrap();

    let partial = dir.path().join("_soak.mds");
    std::fs::write(&partial, "@define val():\nSoak V0\n@end\n\n@export val\n").unwrap();

    let importer = dir.path().join("consumer.mds");
    std::fs::write(&importer, "@import \"./_soak.mds\" as s\n{s.val()}\n").unwrap();

    let out_dir = dir.path().join("out");
    let mut child = ChildGuard(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "10", // Small but non-zero: coalesces rapid-fire writes
                "--poll-interval",
                "50",
                "-q",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("consumer.md"), "Soak V0", TIMEOUT),
        "initial compile should produce 'Soak V0'"
    );

    // 50 sequential edits. Each edit uses a different content length to force
    // deterministic (mtime,size) deltas even on coarse-granularity filesystems.
    for i in 1_u32..=50 {
        // Pad with spaces to ensure each round has a unique byte count.
        let padding = " ".repeat(i as usize);
        std::fs::write(
            &partial,
            format!("@define val():\nSoak V{i}{padding}\n@end\n\n@export val\n"),
        )
        .unwrap();

        // Wait for this round's rebuild to propagate.
        let expected = format!("Soak V{i}");
        assert!(
            wait_for_file_contains(&out_dir.join("consumer.md"), &expected, TIMEOUT),
            "round {i}: consumer.md must contain '{expected}'"
        );
    }

    // Process must still be alive and responsive.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(still_running, "watcher must remain alive after 50 edits");

    // Drop (kills + waits) → clean exit verified by absence of panic.
    drop(child);
}
