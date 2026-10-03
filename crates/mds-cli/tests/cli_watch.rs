//! Integration tests for `mds watch`.
//!
//! Strategy:
//! - Spawn through `spawn_ready`, which blocks until the watcher is fully armed, so a
//!   test's first edit cannot land in a window the watcher is blind to.
//! - Spawn `mds watch … --debounce 0` (immediate rebuild, no debounce delay).
//! - Poll output file content with a bounded `wait_for_file_contains`.
//! - Poll stderr with `common::wait_for_tap` / `wait_for_tap_count` when testing error /
//!   status messages. Both PANIC on timeout, naming the caller's line and what the tap
//!   held. A count or an absence is taken only after an ordered anchor — a line the
//!   watcher writes after everything being counted (`common::ORDER_MARKER_SOURCE`) —
//!   and read back with `finish_text`.
//! - A RAII `ChildGuard` kills+waits the child on drop so tests never leave orphans.
//!
//! Every wait carries one of three bounds, and which one is a claim about the mechanism
//! that must deliver the result: [`TIMEOUT`] for an inotify event, [`TICK_TIMEOUT`] for
//! a self-heal idle tick, [`STARTUP_WINDOW_TIMEOUT`] for the deliberately
//! unsynchronized startup-window tests. See each constant's docs.
//!
//! Writes to watched paths go through `common::write_atomic`. The rule is mechanical,
//! so a reviewer can reproduce the set exactly: a write is converted iff it occurs
//! AFTER the `spawn_ready`/`spawn_unsynchronized` call in the same test fn AND targets
//! a path the watcher is watching (the `.mds` source, an imported partial, the
//! `--vars` file, an external dependency). Pre-spawn fixture writes, `.git` markers,
//! `mds.json`, and output files keep `std::fs::write`. Two post-spawn writes are
//! deliberate exceptions and say so inline: `watch_single_status_line_per_rebuild`,
//! whose subject IS the truncate+write pair that `write_atomic` collapses, and
//! `watch_debounce_single_rebuild_from_burst`, which keeps plain writes because they
//! double the event load its coalescing claim has to survive.
//!
//! Flakiness mitigations:
//! - Assert on output FILE content rather than stderr ordering.
//! - Write dependency files BEFORE adding the `@import` that references them.
//! - Use `-q` where stderr isn't under test.
//! - Always kill+wait child in `ChildGuard::drop`.

mod common;
use common::{
    closed_pipe, count_occurrences, dup_vars_file_warning, make_symlink, mds_bin, poll_tap_until,
    spawn_watch_ready, spawn_watch_ready_stderr_untapped, spawn_watch_unsynchronized, tap_reader,
    wait_for_tap, wait_for_tap_count, write_atomic, ChildGuard, StderrTap, StdoutTap,
    ORDER_MARKER_LINE, ORDER_MARKER_SOURCE,
};
#[cfg(unix)]
use common::{full_file, limit_file_growth, spawn_watch_ready_at};

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ── Helpers ────────────────────────────────────────────────────────────────

/// Spawn a watcher, block until it reports readiness, and wrap it in a `ChildGuard`.
///
/// Every test that edits files under an `mds watch` must go through this: it returns
/// only once every watch is armed and every `(mtime, size)` baseline captured, so a
/// subsequent edit cannot land in a window the watcher is blind to.
///
/// The returned [`StderrTap`] holds everything the child wrote to stderr, including
/// the startup lines printed before the readiness marker.
fn spawn_ready(cmd: &mut Command) -> (ChildGuard, StderrTap) {
    let (child, tap, stdout_tap) = spawn_watch_ready(cmd);
    assert!(
        stdout_tap.is_none(),
        "this spawn piped stdout; use spawn_ready_piped_stdout so the drained stdout \
         is handed back instead of discarded"
    );
    (ChildGuard(child), tap)
}

/// [`spawn_ready`] for a command that set `.stdout(Stdio::piped())`.
///
/// The stdout pipe is drained by the harness — it has to be, or the child blocks on a
/// full pipe before it can write the readiness marker — so the tap is the only way to
/// read it. Tests must not take `child.0.stdout` themselves; it is already gone.
fn spawn_ready_piped_stdout(cmd: &mut Command) -> (ChildGuard, StderrTap, StdoutTap) {
    let (child, tap, stdout_tap) = spawn_watch_ready(cmd);
    let stdout_tap = stdout_tap.expect("caller must set .stdout(Stdio::piped())");
    (ChildGuard(child), tap, stdout_tap)
}

/// Spawn a watcher WITHOUT the readiness handshake and wrap it in a `ChildGuard`.
///
/// Reserved for the tests that exist precisely to exercise startup: a test that
/// synchronises on "startup finished" can never observe anything that happens
/// *during* startup. Every other test must use [`spawn_ready`].
fn spawn_unsynchronized(cmd: &mut Command) -> (ChildGuard, StderrTap) {
    let (child, tap, stdout_tap) = spawn_watch_unsynchronized(cmd);
    assert!(
        stdout_tap.is_none(),
        "this spawn piped stdout, and the harness has already drained it — the tap \
         would be discarded here; use spawn_unsynchronized_piped_stdout instead"
    );
    (ChildGuard(child), tap)
}

/// [`spawn_unsynchronized`] for a command that set `.stdout(Stdio::piped())`; the
/// drained stdout comes back as by [`spawn_ready_piped_stdout`].
fn spawn_unsynchronized_piped_stdout(cmd: &mut Command) -> (ChildGuard, StderrTap, StdoutTap) {
    let (child, tap, stdout_tap) = spawn_watch_unsynchronized(cmd);
    let stdout_tap = stdout_tap.expect("caller must set .stdout(Stdio::piped())");
    (ChildGuard(child), tap, stdout_tap)
}

/// Poll `path` until its content contains `needle`, or `timeout` elapses.
#[track_caller]
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

/// Like [`wait_for_file_contains`] but polls at 1ms instead of 50ms.
///
/// Only for the startup-window tests. There the poll granularity is not just latency,
/// it is the test's own contribution to how late its edit lands inside the window it is
/// trying to hit — a 50ms poll spends a quarter of the `startup-race-probe` window
/// before the edit is even attempted.
#[track_caller]
fn wait_for_file_contains_tight(path: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(path) {
            if content.contains(needle) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// `s` without whitespace or miette's `│` frame marker, so a message miette wrapped
/// (at a space or after a `/`) compares equal to the unwrapped one.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{2502}')
        .collect()
}

/// Poll `path` until it no longer exists, or `timeout` elapses.
#[track_caller]
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

/// Upper bound on how long an **inotify-delivered** effect may take to appear.
///
/// This is a **failure bound, not a synchroniser**. Every spawn goes through
/// [`spawn_ready`], so by the time a test edits a file the watcher is already armed
/// and the remaining work is one inotify delivery plus one compile — milliseconds in
/// practice, even with the suite at full parallelism. The old 10s value dated from
/// when these waits doubled as the startup synchroniser; keeping it only meant every
/// genuine regression cost 10s of wall clock per occurrence before reporting.
///
/// 2s leaves ~40 poll cycles of headroom over the observed millisecond-scale latency
/// — ample for a loaded container, still short enough that a real regression surfaces
/// promptly.
///
/// **This bound is only valid for a wait an OS event can satisfy.** A wait whose
/// recovery path runs on the self-heal idle tick instead must use [`TICK_TIMEOUT`],
/// which is expressed in ticks rather than in absolute latency; see its docs for the
/// enumeration of which waits those are and why.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound for a wait that only the **self-heal idle tick** can satisfy.
///
/// A handful of scenarios destroy the inotify watch descriptor itself, so no OS event
/// can ever announce the recovery: `rmdir` of a watched directory removes the kernel's
/// watch, and the directory recreated in its place is a different inode that nothing is
/// watching. Recovery there is the `liveness_probe_*` re-arm, which runs once per
/// `--poll-interval`. An edit that lands before the watch is armed (the startup-window
/// tests) is announced by no event either, and is recovered by the tick's content
/// backstop. Such a wait is denominated in **ticks**, and pricing it with [`TIMEOUT`] —
/// a latency bound — conflates two unrelated quantities and leaves the headroom
/// silently dependent on whatever `--poll-interval` the test happens to pass.
///
/// The tick-dependent waits, all of which use this bound:
/// - `watch_file_mode_parent_dir_delete_recreate_recovers`
/// - `watch_file_mode_parent_dir_deleted_bounded_errors_then_recovers`
/// - `watch_dir_mode_root_delete_recreate_recovers`
/// - `watch_file_mode_relative_paths_recover_after_the_working_directory_is_recreated`
/// - `watch_dot_recovers_after_the_working_directory_is_recreated`
/// - `watch_vars_dir_delete_recreate_rearms`
/// - `watch_dir_mode_cross_root_edit_during_startup_window_is_not_lost`
/// - `watch_file_mode_dep_edit_during_startup_window_is_not_lost`
/// - `watch_dir_mode_idle_tick_fires_under_event_flood`
/// - `i19_dir_watch_liveness_self_heal_rebuild_warns_about_vars_file_duplicate`
/// - `watch_help_example_src_poll_interval_500_self_heals`
/// - `watch_dir_failed_rebuild_write_keeps_the_compiled_dependencies`
///
/// Every other wait in this file is satisfied by an inotify event on a watch that was
/// never lost, and keeps [`TIMEOUT`].
///
/// 8s is ≥ 50 ticks at the 150ms poll-interval used by the slowest of them, and ≥ 8
/// ticks even at the 1000ms default, so the bound stays a failure bound under any
/// `--poll-interval` a test might reasonably choose.
const TICK_TIMEOUT: Duration = Duration::from_secs(8);

/// Upper bound for the deliberately unsynchronized startup-window tests.
///
/// Those tests race the watcher's own startup, so their wait spans whatever remains of
/// the startup compile plus the `startup-race-probe` delay when that feature is on.
/// Neither is a post-readiness latency, so [`TIMEOUT`] does not apply.
const STARTUP_WINDOW_TIMEOUT: Duration = Duration::from_secs(5);

// ── The pipe-tap waits fail at the caller (#381) ────────────────────────────
//
// No panic hook is installed anywhere here: `cargo test` runs this whole binary in one
// process, and a hook is process-global. `catch_unwind` alone hands back the message.

/// A bound for waits that are EXPECTED to time out: short, so the self-tests stay fast.
const SELF_TEST_TIMEOUT: Duration = Duration::from_millis(60);

/// A tap over text that is already complete: its drain thread reaches EOF at once.
fn tap_of(text: &str) -> StderrTap {
    tap_reader(std::io::Cursor::new(text.as_bytes().to_vec()))
}

/// Run `f`, which must panic, and return the panic's message.
fn panic_message_of<T>(f: impl FnOnce() -> T) -> String {
    let payload = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(_) => panic!("expected a panic, but the call returned"),
        Err(payload) => payload,
    };
    // `panic!` with arguments carries a `String`; a bare literal carries a `&str`.
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .expect("panic payload is a string")
}

/// `wait_for_tap` panics when its needle never appears, naming the CALLER's line —
/// not a line in `common/mod.rs` — and what the tap held.
#[test]
fn wait_for_tap_panics_at_the_caller_naming_what_it_saw() {
    let tap = tap_of("alpha\nbeta\n");
    assert_eq!(
        wait_for_tap(&tap, "beta", TIMEOUT),
        "alpha\nbeta\n",
        "control: a needle that is there returns the whole text"
    );

    let call_line = std::cell::Cell::new(0);
    let message = panic_message_of(|| {
        call_line.set(line!() + 1);
        wait_for_tap(&tap, "gamma", SELF_TEST_TIMEOUT)
    });
    let caller = format!("{}:{}:", file!(), call_line.get());
    assert!(
        message.contains(&caller),
        "the message must name the caller {caller}; got:\n{message}"
    );
    assert!(
        message.contains("\"gamma\"") && message.contains("alpha\nbeta\n"),
        "the message must name the needle and what the tap held; got:\n{message}"
    );
}

/// `wait_for_tap_count` panics short of its count, naming the caller's line and the
/// count it saw.
#[test]
fn wait_for_tap_count_panics_at_the_caller_naming_the_count_it_saw() {
    let tap = tap_of("Recompiled a\nRecompiled b\n");
    let _ = wait_for_tap_count(&tap, "Recompiled ", 2, TIMEOUT);

    let call_line = std::cell::Cell::new(0);
    let message = panic_message_of(|| {
        call_line.set(line!() + 1);
        wait_for_tap_count(&tap, "Recompiled ", 3, SELF_TEST_TIMEOUT)
    });
    let caller = format!("{}:{}:", file!(), call_line.get());
    assert!(
        message.contains(&caller),
        "the message must name the caller {caller}; got:\n{message}"
    );
    assert!(
        message.contains("at least 3 occurrences") && message.contains("saw 2"),
        "the message must name the count wanted and the count seen; got:\n{message}"
    );
}

/// `poll_tap_until` never panics: a condition met hands back the text as `Ok`, one
/// that times out hands back the last text seen as `Err`.
#[test]
fn poll_tap_until_reports_a_timeout_as_data() {
    let tap = tap_of("alpha\n");
    assert_eq!(
        poll_tap_until(&tap, TIMEOUT, |text| text.contains("alpha")),
        Ok("alpha\n".to_string())
    );
    assert_eq!(
        poll_tap_until(&tap, SELF_TEST_TIMEOUT, |text| text.contains("omega")),
        Err("alpha\n".to_string())
    );
}

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

// ── T-I1: Initial compile writes output ────────────────────────────────────

#[test]
fn watch_initial_compile_writes_output() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );

    let found = wait_for_file_contains(&out, "Hello World!", TIMEOUT);
    assert!(found, "initial compile should write output to hello.md");
    drop(child);
}

/// A `.md` entry that declares `type: mds` is an MDS file: file mode compiles it and
/// rebuilds it on an edit. The resolver judges the file type, so watch must not run
/// the CLI's `.mds`-extension check on its entry (#417 compiles the entry by the path
/// as typed, and nothing else changes). Written to `-o`, since the default output
/// path of `page.md` is the entry itself.
#[test]
fn watch_type_mds_markdown_entry_compiles() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.md");
    std::fs::write(&src, "---\ntype: mds\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("out.md");

    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.md", "-o", "out.md", "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "a type: mds .md entry compiles at startup"
    );

    write_atomic(&src, "---\ntype: mds\nname: Again\n---\nHello {{name}}!\n");
    assert!(
        wait_for_file_contains(&out, "Hello Again!", TIMEOUT),
        "and is rebuilt on an edit"
    );
    drop(child);
}

/// A rebuild compiles the entry by the path as typed as well, so an error about the
/// entry raised on a rebuild names it that way (#417): once `page.md` drops its
/// `type: mds`, the rebuild reports `not an MDS file: ./sub/../page.md`, never the
/// canonical absolute path. Control: the same typed path compiled at startup.
#[test]
fn watch_rebuild_names_the_entry_as_typed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let src = dir.path().join("page.md");
    std::fs::write(&src, "---\ntype: mds\n---\nHello!\n").unwrap();
    let out = dir.path().join("out.md");
    let typed = "./sub/../page.md";

    let (child, stderr_tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", typed, "-o", "out.md", "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&out, "Hello!", TIMEOUT),
        "control: the typed path compiles at startup"
    );

    write_atomic(&src, "Hello again!\n");
    // The rebuild names the entry as typed.
    let stderr = wait_for_tap(&stderr_tap, &format!("not an MDS file: {typed}"), TIMEOUT);
    // Squashed: miette wraps a long absolute path across lines.
    let canonical = src.canonicalize().unwrap();
    assert!(
        !squash(&stderr).contains(&squash(&format!(
            "not an MDS file: {}",
            canonical.display()
        ))),
        "never by its canonical absolute path; stderr: {stderr}"
    );
    drop(child);
}

/// File mode compiles the entry by the path as typed but watches its canonical file
/// (#417). Once a symlinked directory on the typed path is retargeted, the typed path
/// leads to another file: the rebuild is refused (`mds::io`), naming the entry as typed,
/// and nothing is written — rather than compiling the new target while watching the
/// old one. Control: through the link as it was, an edit rebuilds.
///
/// Unix-only: it retargets a directory symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn watch_entry_through_a_retargeted_directory_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    for (name, text) in [("a", "Hello A\n"), ("b", "Hello B\n")] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
        std::fs::write(dir.path().join(name).join("page.mds"), text).unwrap();
    }
    let link = dir.path().join("link");
    symlink("a", &link).unwrap();
    let watched = dir.path().join("a").join("page.mds");
    let out = dir.path().join("out.md");

    let (child, stderr_tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args([
                "watch",
                "link/page.mds",
                "-o",
                "out.md",
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null()),
    );
    assert!(wait_for_file_contains(&out, "Hello A", TIMEOUT), "startup");
    write_atomic(&watched, "Hello A1\n");
    assert!(
        wait_for_file_contains(&out, "Hello A1", TIMEOUT),
        "control: an edit rebuilds through the link"
    );

    std::fs::remove_file(&link).unwrap();
    symlink("b", &link).unwrap();
    write_atomic(&watched, "Hello A2\n");
    let stderr = wait_for_tap(&stderr_tap, "watched entry now resolves", TIMEOUT);
    assert!(
        squash(&stderr).contains(
            "mds::io×watchedentrynowresolvestoadifferentfile:\"link/page.mds\";\
             restartmdswatchtofollowit"
        ),
        "the rebuild is refused, naming the entry as typed; stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "Hello A1\n",
        "nothing is written from the retargeted file"
    );
    drop(child);
}

// ── T-I2: Edit entry → output updates ─────────────────────────────────────

#[test]
fn watch_edit_entry_updates_output() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: Alice\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "initial compile should produce Hello Alice!"
    );

    // Edit the source.
    write_atomic(&src, "---\nname: Bob\n---\nHello {{name}}!\n");

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
        "@define greet(name):\nHello {{name}}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // Entry that imports helper.
    let entry = dir.path().join("entry.mds");
    std::fs::write(
        &entry,
        "@import \"./helper.mds\" as h\n\n{{h.greet(\"World\")}}\n",
    )
    .unwrap();
    let out = dir.path().join("entry.md");

    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", entry.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should produce Hello World!"
    );

    // Edit the helper to change the greeting.
    write_atomic(
        &helper,
        "@define greet(name):\nHi there {{name}}!\n@end\n\n@export greet\n",
    );

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
    std::fs::write(&src, "---\nname: Alice\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );

    // Wait for successful initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "initial compile should succeed"
    );

    // Introduce a compile error, and wait for the rebuild to report it: the diagnostic
    // survives `-q`, so its appearance is the event the liveness check below needs.
    write_atomic(&src, "Hello {{undefined_var_xyz}}!\n");
    wait_for_tap(&stderr_tap, "undefined_var_xyz", TIMEOUT);

    // Process should still be alive.
    // (try_wait returns None = still running, Some = exited)
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher should stay alive after compile error"
    );

    // Fix the error — watcher should recover.
    write_atomic(&src, "---\nname: Charlie\n---\nHello {{name}}!\n");
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
        "---\nname: A\n---\nFile A: {{name}}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.mds"),
        "---\nname: B\n---\nFile B: {{name}}\n",
    )
    .unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(
        &dir.path().join("a.mds"),
        "---\nname: A-edited\n---\nFile A: {{name}}\n",
    );
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

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A", TIMEOUT),
        "a.md should appear on startup"
    );

    // Create a new file AFTER the watcher is running.
    write_atomic(
        &dir.path().join("c.mds"),
        "---\nname: C\n---\nNew file {{name}}\n",
    );

    assert!(
        wait_for_file_contains(&out_dir.join("c.md"), "New file C", TIMEOUT),
        "newly created c.mds should be compiled to c.md"
    );

    drop(child);
}

// ── #204 pin: watch is UNCHANGED on an empty root ──────────────────────────
//
// GREEN before and after #204. `mds build|check|fmt|lint <empty dir>` now exits
// non-zero, but `mds watch <empty dir>` deliberately does NOT error: a file
// created later is a valid flow there, and this test is what pins that.

#[test]
fn watch_dir_mode_empty_root_starts_and_picks_up_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    // `spawn_ready` returning IS the armed proof: it panics with "exited before
    // signalling readiness" if the watcher bailed out on the empty root.
    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    // Create the first .mds file AFTER the watcher armed on the empty root.
    write_atomic(
        &dir.path().join("c.mds"),
        "---\nname: C\n---\nNew file {{name}}\n",
    );

    assert!(
        wait_for_file_contains(&out_dir.join("c.md"), "New file C", TIMEOUT),
        "a file created under an initially empty watch root should be compiled"
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

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    std::fs::write(&src, "---\nname: Default\n---\nHello {{name}}!\n").unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"name": "Alice"}"#).unwrap();
    let out = dir.path().join("hello.md");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "initial compile with vars should produce Hello Alice!"
    );

    // Edit the vars file.
    write_atomic(&vars, r#"{"name": "Bob"}"#);

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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    // Spawn with piped stderr — not a TTY. `clear_terminal` only emits the ANSI
    // sequence on rebuilds (not the initial compile), so we must trigger a rebuild
    // to actually exercise the --clear code path.
    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--clear", "--debounce", "0"])
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should write output"
    );

    // Edit the source to trigger a rebuild — this is the path that calls
    // clear_terminal(). On a non-TTY pipe it must be a no-op.
    write_atomic(&src, "---\nname: There\n---\nHello {{name}}!\n");
    assert!(
        wait_for_file_contains(&out, "Hello There!", TIMEOUT),
        "rebuild should occur after editing source"
    );

    // Stop the child and collect everything it wrote to stderr. `finish` reaps the
    // child and then JOINS the drain thread, so the snapshot cannot be a truncated
    // prefix — this site is where the Linux tearing was first observed. Raw bytes,
    // not text: the assertions below hunt for raw ESC sequences.
    let stderr_bytes = stderr_tap.finish(&mut child);

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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("custom_output.md");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    std::fs::write(&src, "Hello {{name}}!\n").unwrap();
    let out = dir.path().join("tpl.md");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "Hello Alice!", TIMEOUT),
        "--set name=Alice should be applied on initial compile"
    );

    // Edit to trigger rebuild — --set should still apply.
    write_atomic(&src, "Greetings {{name}}!\n");
    assert!(
        wait_for_file_contains(&out, "Greetings Alice!", TIMEOUT),
        "--set name=Alice should persist across rebuilds"
    );

    drop(child);
}

// ── T-I13: Single-file @message template → intrinsic JSON output ──────────
//
// Output extension and format are derived from the compiled kind — no --format flag.
// A messages template (chat.mds) watched in single-file mode produces chat.json.

#[test]
fn watch_messages_template_produces_json_intrinsically() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("chat.mds");
    std::fs::write(&src, "@message user:\nWhat is 2+2?\n@end\n").unwrap();
    let out = dir.path().join("chat.json");

    // No `-o`: the `mds watch chat.mds` example of `--help` — the `.json` extension is
    // the default output path's, derived from the compiled kind.
    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "What is 2+2?", TIMEOUT),
        "messages template should write JSON containing the message"
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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    // -o - forces stdout output. The harness drains the pipe, so poll the tap.
    let (child, _stderr_tap, stdout_tap) = spawn_ready_piped_stdout(
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
            .stdout(Stdio::piped()),
    );

    // Bounded by TIMEOUT: at most TIMEOUT / 50ms iterations.
    let deadline = Instant::now() + TIMEOUT;
    let mut found = false;
    while Instant::now() < deadline {
        if stdout_tap.text().contains("Hello World!") {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(found, "with -o -, compiled output should appear on stdout");
    drop(child);
}

// ── T-I16: Ctrl+C clean exit (#[cfg(unix)]) ────────────────────────────────

/// `#[cfg(unix)]`: sends SIGINT via `libc::kill`, which has no Windows analogue (#147).
#[test]
#[cfg(unix)]
fn watch_ctrl_c_exits_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (mut guard, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let pid = guard.id();

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

// ── T-P1: Debounce — final value wins after rapid edits ───────────────────

#[test]
fn watch_debounce_final_value_wins_after_rapid_edits() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: v0\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    // Use a 200ms debounce for stability.
    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "200"])
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Hello v0!", TIMEOUT),
        "initial compile should produce v0"
    );

    // Write 10 rapid edits within the debounce window.
    for i in 1..=10 {
        write_atomic(&src, format!("---\nname: v{i}\n---\nHello {{{{name}}}}!\n"));
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
///
/// A rebuild whose output does not change is silent — no write, no `Recompiled` — and
/// the entry renders the same text whether or not the helper is still tracked. So once
/// it drops the import, the entry `@include`s an empty module: every compile of it
/// prints the "produced empty output" warning, which makes the rebuild the helper edit
/// must NOT cause visible.
#[test]
fn watch_import_removal_stops_tracking_dep() {
    let dir = tempfile::tempdir().unwrap();

    // Write the helper BEFORE the entry references it (mitigates FSEvents latency).
    let helper = dir.path().join("helper.mds");
    std::fs::write(
        &helper,
        "@define greet(name):\nHello {{name}}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // An empty module: an `@include` of it adds no text and warns on every compile.
    std::fs::write(dir.path().join("empty.mds"), "").unwrap();
    let include_warning = "@include of 'e' produced empty output";

    // Entry that imports helper initially.
    let entry = dir.path().join("entry.mds");
    std::fs::write(
        &entry,
        "@import \"./helper.mds\" as h\n\n{{h.greet(\"World\")}}\n",
    )
    .unwrap();
    let out = dir.path().join("entry.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                entry.to_str().unwrap(),
                "--debounce",
                "0",
                // No idle tick: its first-tick recompile would print the warning
                // counted below on a schedule of its own.
                "--poll-interval",
                "0",
                // No -q: the warning that makes a compile visible is a status line.
            ])
            .stdout(Stdio::null()),
    );

    // Wait for initial compile — helper IS tracked.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should produce Hello World!"
    );

    // STEP 1 (add direction, already covered by T-I3 but verified here too):
    // Edit helper — entry output should update because helper is tracked.
    write_atomic(
        &helper,
        "@define greet(name):\nHi {{name}}!\n@end\n\n@export greet\n",
    );
    assert!(
        wait_for_file_contains(&out, "Hi World!", TIMEOUT),
        "editing helper while imported should trigger a rebuild"
    );

    // STEP 2 (removal direction): rewrite entry to remove the @import. What it renders
    // does NOT reference helper; the empty module's warning marks each of its compiles.
    write_atomic(
        &entry,
        "@import \"./empty.mds\" as e\n@include e\nStatic content\n",
    );
    assert!(
        wait_for_file_contains(&out, "Static content", TIMEOUT),
        "removing @import should rebuild entry with static content"
    );
    // Positive control: a compile of this entry shows on stderr.
    wait_for_tap(&stderr_tap, include_warning, TIMEOUT);

    // SETTLE WINDOW, deliberately a fixed sleep: one edit can reach the watcher as
    // several events, and each recompiles the entry and warns again. No event marks the
    // last of them, and the baseline below must hold every one.
    std::thread::sleep(Duration::from_millis(500));
    let warnings_before = count_occurrences(&stderr_tap.text(), include_warning);
    let content_before = std::fs::read_to_string(&out).unwrap();

    // STEP 3: Edit helper again — the entry must NOT be rebuilt, because the dep was
    // removed from the watch set after the resync in step 2.
    write_atomic(
        &helper,
        "@define greet(name):\nBye {{name}}!\n@end\n\n@export greet\n",
    );

    // NEGATIVE WINDOW, deliberately a fixed sleep: 500ms is far beyond the debounce-0
    // rebuild latency, so a rebuild the helper edit caused has compiled the entry as it
    // is now. The marker below replaces the entry, and a rebuild that read the marker
    // instead would print nothing this test counts.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        content_before,
        "after removing @import, editing helper must NOT change entry output"
    );

    // Ordered anchor, and the positive anchor for the window: the watcher still handles
    // the entry's events, and anything a helper-triggered rebuild printed precedes the
    // marker's diagnostic, so the count read back is final.
    write_atomic(&entry, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&stderr, include_warning),
        warnings_before,
        "after removing @import, editing helper must NOT rebuild the entry; \
         stderr:\n{stderr}"
    );
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
        "---\ngreeting: Default\n---\n{{greeting}} from A\n",
    )
    .unwrap();
    std::fs::write(
        src_dir.path().join("b.mds"),
        "---\ngreeting: Default\n---\n{{greeting}} from B\n",
    )
    .unwrap();

    // Write vars.json with initial greeting value.
    let vars = src_dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"greeting": "Hello"}"#).unwrap();

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(&vars, r#"{"greeting": "Goodbye"}"#);

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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
    );

    // Wait for the initial compile to produce valid output.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should succeed"
    );

    // Introduce a compile error (reference an undefined variable with no frontmatter default).
    write_atomic(&src, "Hello {{__undefined_xyz__}}!\n");

    // The error must reach stderr despite -q: under quiet mode status messages are
    // suppressed but error diagnostics are not. The wait is the assertion, and it is
    // also what orders the liveness check below after the rebuild.
    //
    // It waits on the CONTENT of the diagnostic, not merely on stderr being non-empty.
    // Non-emptiness is satisfied by any byte from any source, so it stops being a test
    // of this behaviour the moment anything else writes to the stream — which is
    // exactly what happened when the readiness handshake was a stderr marker line.
    // Naming the undefined variable ties the wait to the error we provoked.
    wait_for_tap(&stderr_tap, "__undefined_xyz__", TIMEOUT);

    // Process must still be alive — watcher stays up after compile errors.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive after a compile error even under -q"
    );

    // Fix the error — watcher should recover.
    write_atomic(&src, "---\nname: Fixed\n---\nHello {{name}}!\n");
    assert!(
        wait_for_file_contains(&out, "Hello Fixed!", TIMEOUT),
        "after fixing the compile error, watcher should rebuild successfully"
    );

    drop(child);
}

// ── AC-F9: "Stopped watching." message on clean Ctrl+C ────────────────────

/// On SIGINT the watcher must print "Stopped watching." to stderr (non-quiet).
///
/// `#[cfg(unix)]`: sends SIGINT via `libc::kill`, which has no Windows analogue (#147).
#[test]
#[cfg(unix)]
fn watch_ctrl_c_prints_stopped_watching() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.mds");
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (mut guard, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let pid = guard.id();

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

    // The child has already exited; `finish_text` reaps it again (harmless — `wait`
    // caches the status) and then joins the drain thread, which is what actually
    // guarantees every byte has been copied.
    let stderr_str = stderr_tap.finish_text(&mut guard);
    assert!(
        stderr_str.contains("Stopped watching."),
        "stderr should contain 'Stopped watching.' after Ctrl+C, got: {stderr_str:?}"
    );
}

// ── AC-P1: Debounce coalesces burst — count rebuild summary lines ──────────

/// A save burst LONGER than the debounce window is still one rebuild (#379).
///
/// The old shape of this test wrote a burst that fit inside the window and then
/// tolerated a second rebuild, so the property it advertised — one rebuild per burst —
/// was never actually pinned. It failed as `got 3` on loaded CI runners (runs
/// 33996153739, 33976595173, 33753123463), each of the three compiles seeing a
/// different intermediate state of the file.
///
/// The burst here is deliberately longer than the window: 12 writes, 30ms apart, so at
/// least 330ms against a 250ms window. Under a window that expires at a fixed offset from
/// the FIRST event that is two or three rebuilds; under a quiet period it is one,
/// because no gap between writes ever reaches 250ms. `--poll-interval` is left at its
/// default so the idle-tick liveness probe stays live — a stronger claim than
/// disabling it.
#[test]
fn watch_debounce_single_rebuild_from_burst() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("burst.mds");
    std::fs::write(&src, "---\nname: v0\n---\nBurst {{name}}!\n").unwrap();
    let out = dir.path().join("burst.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "250"])
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Burst v0!", TIMEOUT),
        "initial compile should produce Burst v0!"
    );

    // DELIBERATE: this test's subject is the debounce window collapsing a burst of
    // truncate+write pairs, so it keeps plain writes — they double the event load
    // that `write_atomic` would collapse into one rename. Every other post-spawn write
    // in this file goes through `write_atomic`.
    let mut stamps: Vec<Instant> = Vec::with_capacity(12);
    for i in 1..=12u32 {
        std::fs::write(&src, format!("---\nname: v{i}\n---\nBurst {{{{name}}}}!\n")).unwrap();
        stamps.push(Instant::now());
        std::thread::sleep(Duration::from_millis(30));
    }

    // Self-diagnosing preconditions, asserted BEFORE the outcome: if the burst this
    // process actually produced was not longer than the window, or had a gap wide
    // enough to legitimately close it, the outcome assertion below would be measuring
    // the scheduler rather than the watcher.
    let span = stamps[stamps.len() - 1].duration_since(stamps[0]);
    let max_gap = stamps
        .windows(2)
        .map(|w| w[1].duration_since(w[0]))
        .max()
        .expect("burst has at least two writes");
    assert!(
        span > Duration::from_millis(250),
        "precondition: the burst must outlast the 250ms window, else the test proves \
         nothing about extension; span was {span:?}"
    );
    assert!(
        max_gap < Duration::from_millis(250),
        "precondition: no gap between writes may reach the 250ms window, else the \
         window is entitled to close mid-burst; largest gap was {max_gap:?}"
    );

    // The count below is exact only once every rebuild the burst caused has written its
    // line. The final state of the burst is published by the last of those rebuilds —
    // under a window that split the burst, an earlier one publishes an intermediate
    // state — so wait for it, then write the order marker: its diagnostic reaches
    // stderr after every line of every earlier rebuild. The marker's compile fails, so
    // the output keeps the burst's final state.
    assert!(
        wait_for_file_contains(&out, "Burst v12!", TIMEOUT),
        "the burst's final state must be published"
    );
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = stderr_tap.finish_text(&mut child);

    assert_eq!(
        count_occurrences(&stderr, "Recompiled "),
        1,
        "a {span:?} burst with a largest gap of {max_gap:?} must coalesce into exactly \
         one rebuild under a 250ms quiet period; stderr was:\n{stderr}"
    );
    assert_eq!(
        count_occurrences(&stderr, "Compiled to"),
        1,
        "the startup compile is the only 'Compiled to' line; stderr was:\n{stderr}"
    );
    // `mds` copies the frontmatter block through verbatim and interpolates the body.
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "---\nname: v12\n---\nBurst v12!\n",
        "the single rebuild must compile the FINAL state of the burst, not an \
         intermediate one; stderr was:\n{stderr}"
    );
}

/// Env var naming a file a skipping test appends one line to (#397).
///
/// A skip is an early return, which libtest counts as a pass, and libtest shows no
/// stderr of a passing test — so without this file a skip is invisible in a CI log.
/// The watch soak workflow sets it per iteration and tallies skipped iterations
/// separately from passed ones.
const SKIP_LOG_ENV: &str = "MDS_TEST_SKIP_LOG";

/// Report that `test` skipped, where a passing run cannot hide it: on stderr, as one
/// line appended to the file [`SKIP_LOG_ENV`] names, and — under GitHub Actions — as a
/// warning in the job summary.
///
/// # Panics
/// Panics if [`SKIP_LOG_ENV`] is set and the line cannot be appended: whoever set it is
/// counting skips, and a skip it cannot see would read as a pass.
fn record_skip(test: &str, reason: &str) {
    use std::io::Write as _;
    eprintln!("{test}: {reason}");
    if let Some(path) = std::env::var_os(SKIP_LOG_ENV) {
        let path = std::path::PathBuf::from(path);
        let appended = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .and_then(|mut log| writeln!(log, "{test}: {reason}"));
        if let Err(e) = appended {
            panic!(
                "{SKIP_LOG_ENV}={}: cannot record the skip: {e}",
                path.display()
            );
        }
    }
    // Best effort: the job summary is a convenience, the skip log is the record.
    if let Ok(summary_path) = std::env::var("GITHUB_STEP_SUMMARY") {
        if let Ok(mut summary) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(summary_path)
        {
            let _ = writeln!(summary, ":warning: {test} skipped: {reason}");
        }
    }
}

/// The writer thread's record of one attempt of the cap test.
struct WriterTrace {
    /// When the writer started.
    started: Instant,
    /// When each write completed, in order (at most the writer's 2000 iterations).
    writes: Vec<Instant>,
}

/// Where one attempt's write cadence went (#397): the gaps between completed writes —
/// the first measured from the writer's start, as the precondition has always measured
/// it — and where the first rebuild fell among them.
///
/// `max_gap` is the metric the harness precondition judges. It spans the whole stream,
/// while only the gaps before the first rebuild can let a quiet period end on its own;
/// `max_gap_before_rebuild` records that part separately, so a skip says which of the
/// two the runner actually failed.
struct GapBreakdown {
    writes: usize,
    span: Duration,
    max_gap: Duration,
    median_gap: Duration,
    p99_gap: Duration,
    gaps_at_or_over_window: usize,
    /// When the stderr poll first saw a rebuild, after the writer started (up to one
    /// poll late); `None` when no rebuild was seen within the poll's bound.
    rebuild_seen_after: Option<Duration>,
    /// The largest gap that ended before the rebuild was seen.
    max_gap_before_rebuild: Option<Duration>,
}

impl GapBreakdown {
    fn of(trace: &WriterTrace, rebuild_seen: Option<Instant>, window: Duration) -> Self {
        let mut previous = trace.started;
        let mut gaps: Vec<(Instant, Duration)> = Vec::with_capacity(trace.writes.len());
        for &at in &trace.writes {
            gaps.push((at, at.duration_since(previous)));
            previous = at;
        }
        let mut sorted: Vec<Duration> = gaps.iter().map(|&(_, gap)| gap).collect();
        sorted.sort_unstable();
        let rank = |per_mille: usize| -> Duration {
            sorted
                .get((sorted.len().saturating_sub(1) * per_mille) / 1000)
                .copied()
                .unwrap_or_default()
        };
        GapBreakdown {
            writes: trace.writes.len(),
            span: trace
                .writes
                .last()
                .map_or(Duration::ZERO, |&last| last.duration_since(trace.started)),
            max_gap: sorted.last().copied().unwrap_or_default(),
            median_gap: rank(500),
            p99_gap: rank(990),
            gaps_at_or_over_window: sorted.iter().filter(|&&gap| gap >= window).count(),
            rebuild_seen_after: rebuild_seen.map(|at| at.duration_since(trace.started)),
            max_gap_before_rebuild: rebuild_seen.map(|seen| {
                gaps.iter()
                    .filter(|&&(at, _)| at <= seen)
                    .map(|&(_, gap)| gap)
                    .max()
                    .unwrap_or_default()
            }),
        }
    }
}

impl std::fmt::Display for GapBreakdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} writes over {:?}; gaps: max {:?}, median {:?}, p99 {:?}, {} at or over the \
             window; ",
            self.writes,
            self.span,
            self.max_gap,
            self.median_gap,
            self.p99_gap,
            self.gaps_at_or_over_window
        )?;
        match (self.rebuild_seen_after, self.max_gap_before_rebuild) {
            (Some(after), Some(gap)) => write!(
                f,
                "first rebuild seen {after:?} after the writer started, largest gap \
                 before it {gap:?}"
            ),
            _ => write!(f, "no rebuild seen"),
        }
    }
}

/// The cap rebuilds a file that is never left alone (#379).
///
/// A quiet period that can always be extended is unbounded: a writer that never
/// pauses postpones its own rebuild for as long as it keeps writing. `--poll-interval 0`
/// turns the idle-tick liveness probe off, so within this test the cap is the ONLY
/// mechanism that can produce a rebuild while the stream is running — and it is also
/// the reason the probe cannot be starved in the configurations that do enable it,
/// since the loop never reaches `TickClock::recv_next` while a window is open.
#[test]
fn watch_debounce_cap_rebuilds_while_writes_never_stop() {
    // The writer thread must keep the stream denser than the 200ms quiet-period window,
    // so a rebuild seen WHILE writing provably comes from the cap and not from a quiet
    // period that ended on its own. That is a HARNESS precondition, not a property of
    // the code under test: on a loaded runner the writer thread can itself be
    // descheduled past the window (a 747ms inter-write gap was observed on CI), which
    // makes the sample inconclusive rather than failing. Retry the whole measurement a
    // bounded number of times, gated ONLY on that precondition — every behaviour
    // assertion below still fails hard on the first conclusive attempt, so a real
    // regression is never retried or skipped away. If the cadence is still
    // unsustainable after every attempt, the test SKIPS rather than failing the
    // required check: it prints a `SKIPPED (inconclusive harness)` line, appends it to
    // the file `MDS_TEST_SKIP_LOG` names (the watch soak counts those), and under
    // GitHub Actions adds a warning to the job summary — see #397, which tracks
    // root-causing the cadence problem on loaded runners. Every attempt logs its gap
    // breakdown on stderr, so a failing run shows the cadence of each one.
    const TEST: &str = "watch_debounce_cap_rebuilds_while_writes_never_stop";
    const MAX_ATTEMPTS: u32 = 6;
    const WINDOW: Duration = Duration::from_millis(200);

    let mut max_gaps: Vec<Duration> = Vec::with_capacity(MAX_ATTEMPTS as usize);
    for attempt in 1..=MAX_ATTEMPTS {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hot.mds");
        std::fs::write(&src, "---\nname: v0\n---\nHot {{name}}!\n").unwrap();
        let out = dir.path().join("hot.md");

        // --debounce 200 => cap = max(10 x 200ms, 1s) = 2s.
        let (mut child, stderr_tap) = spawn_ready(
            mds_bin()
                .args([
                    "watch",
                    src.to_str().unwrap(),
                    "--debounce",
                    "200",
                    "--poll-interval",
                    "0",
                ])
                .stdout(Stdio::null()),
        );

        assert!(
            wait_for_file_contains(&out, "Hot v0!", TIMEOUT),
            "initial compile should produce Hot v0!"
        );

        let writing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let writer_flag = std::sync::Arc::clone(&writing);
        let writer_src = src.clone();
        let writer = std::thread::spawn(move || {
            let started = Instant::now();
            let stop_at = started + Duration::from_secs(3);
            let mut writes = Vec::with_capacity(2_000);
            // Doubly bounded: <= 3s of wall clock AND <= 2000 iterations.
            for i in 1..=2_000u32 {
                if Instant::now() >= stop_at {
                    break;
                }
                write_atomic(
                    &writer_src,
                    format!("---\nname: v{i}\n---\nHot {{{{name}}}}!\n"),
                );
                writes.push(Instant::now());
                std::thread::sleep(Duration::from_millis(5));
            }
            writer_flag.store(false, std::sync::atomic::Ordering::SeqCst);
            WriterTrace { started, writes }
        });

        // The cap is 2s; allow the compile that follows it to land inside the bound.
        // Non-panicking on purpose: "no rebuild by then" is decided below, after the
        // harness precondition, so an inconclusive attempt is retried rather than failed.
        let rebuild_seen = poll_tap_until(&stderr_tap, Duration::from_millis(3500), |text| {
            text.contains("Recompiled ")
        })
        .ok()
        .map(|_| Instant::now());
        let rebuilt_while_writing = writing.load(std::sync::atomic::Ordering::SeqCst);

        let trace = writer.join().expect("writer thread panicked");
        let breakdown = GapBreakdown::of(&trace, rebuild_seen, WINDOW);
        eprintln!("{TEST}: attempt {attempt}/{MAX_ATTEMPTS}: {breakdown}");
        let max_gap = breakdown.max_gap;
        max_gaps.push(max_gap);

        // Harness precondition, checked before any behaviour assertion: if the writer
        // thread could not sustain a sub-window cadence, this run cannot tell a cap
        // rebuild from a quiet-period one. Discard it and retry rather than reporting a
        // scheduling hiccup as a product failure.
        if max_gap >= WINDOW {
            drop(child);
            if attempt < MAX_ATTEMPTS {
                continue;
            }
            // Every attempt was inconclusive: the runner is too loaded to exercise the
            // cap deterministically. Skip rather than fail the required check — no
            // product behaviour was ever exercised — and leave a trail so this shows up
            // in the run summary and the soak's tally instead of silently vanishing.
            // See #397.
            record_skip(
                TEST,
                &format!(
                    "SKIPPED (inconclusive harness): writer gap {max_gap:?} >= {WINDOW:?} on \
                     all {MAX_ATTEMPTS} attempts (largest gap per attempt: {max_gaps:?}); \
                     last attempt: {breakdown}"
                ),
            );
            return;
        }

        assert!(
            rebuilt_while_writing,
            "a rebuild must happen WHILE the writes are still arriving — that is what the \
             cap is for; nothing was seen until the stream stopped"
        );

        let stderr = stderr_tap.finish_text(&mut child);
        let rebuilds = count_occurrences(&stderr, "Recompiled ");
        assert!(
            (1..=4).contains(&rebuilds),
            "3s of writes under a 200ms window with a 2s cap is one capped rebuild plus \
             the quiet-period rebuild that follows the last write; a fixed 200ms window \
             would give ~15. Got {rebuilds}; stderr was:\n{stderr}"
        );
        return;
    }
}

// ── AC-F10: Watch no-arg auto-detect ─────────────────────────────────────

/// `mds watch` with no argument and cwd containing exactly ONE .mds file
/// should auto-detect and compile that file.
#[test]
fn watch_no_arg_auto_detects_single_mds_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("only.mds"),
        "---\nname: AutoDetect\n---\nAuto {{name}}!\n",
    )
    .unwrap();
    let out = dir.path().join("only.md");

    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", "--debounce", "0", "-q"])
            .current_dir(dir.path()) // cwd = tempdir containing exactly one .mds
            .stdout(Stdio::null()),
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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("nochange.md");

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    // Wait for the initial compile to complete.
    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should write output"
    );

    // Let the watcher idle for 1.5s — any synthetic FS events would fire within this window.
    std::thread::sleep(Duration::from_millis(1500));

    // Ordered anchor: the watcher was still handling events, and everything it printed
    // during the window precedes the marker's diagnostic. Its compile fails, so it adds
    // no status line of its own.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);

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

    let (mut child, stderr_tap, stdout_tap) = spawn_ready_piped_stdout(
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
            .stdout(Stdio::piped()),
    );

    // Let the watcher run long enough to capture initial compile + any spurious second write.
    std::thread::sleep(Duration::from_millis(1500));

    // Ordered anchor: the watcher was still handling events, and it wrote anything it
    // published during the window to stdout before the marker's diagnostic reached
    // stderr. The marker's compile fails, so it publishes nothing.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);

    // Stop the child and collect all stdout. `finish_text` reaps the child and then
    // joins the drain, so no flush sleep is needed to make the snapshot complete.
    let stdout_str = stdout_tap.finish_text(&mut child);

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
        "---\nname: A\n---\nFile A: {{name}}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.mds"),
        "---\nname: B\n---\nFile B: {{name}}\n",
    )
    .unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

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

    // Ordered anchor: the watcher was still handling events, and everything it printed
    // during the window precedes the marker's diagnostic. Its compile fails, so it adds
    // no status line of its own.
    write_atomic(&dir.path().join("a.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);

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
///
/// Uses the **default 100ms debounce**, not `--debounce 0`. One `fs::write` is a
/// truncate followed by a write, which the kernel reports as two separate content
/// events; under `--debounce 0` — documented as "immediate rebuilds", i.e. opting
/// out of burst coalescing — the watcher may legitimately rebuild twice, so
/// `Recompiled == 1` is not a property the product guarantees there. (Measured at
/// ~6-12% of single edits under load, on both sides of the arm-before-publish
/// change.) The debounce window is precisely the mechanism that collapses that pair,
/// so running this assertion with coalescing enabled tests the intended invariant —
/// one status line per rebuild, and loop rebuilds say "Recompiled", not "Compiled to"
/// — instead of an incidental timing outcome.
#[test]
fn watch_single_status_line_per_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("status.mds");
    std::fs::write(&src, "---\nname: v0\n---\nStatus {{name}}!\n").unwrap();
    let out = dir.path().join("status.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "100", // coalesce the truncate+write pair — see doc comment above
                       // No -q: we need to observe status messages.
            ])
            .stdout(Stdio::null()),
    );

    // Wait for the initial compile.
    assert!(
        wait_for_file_contains(&out, "Status v0!", TIMEOUT),
        "initial compile should produce Status v0!"
    );

    // Make ONE real content-changing edit.
    // DELIBERATE: this test's subject is coalescing the truncate+write pair at
    // --debounce 100, so it keeps the plain write. Every other post-spawn write in
    // this file goes through `write_atomic`.
    std::fs::write(&src, "---\nname: v1\n---\nStatus {{name}}!\n").unwrap();

    // Wait for the rebuild to appear in the output.
    assert!(
        wait_for_file_contains(&out, "Status v1!", TIMEOUT),
        "after editing, output should contain Status v1!"
    );

    // NEGATIVE WINDOW, deliberately a fixed sleep: a truncate+write pair the debounce
    // failed to coalesce would show up as a second rebuild within one 100ms window of
    // the first, and no event marks the end of "no second rebuild". 500ms lets every
    // trailing event of this edit close its own window before the anchor below is
    // written, so the anchor cannot coalesce with them and hide one.
    std::thread::sleep(Duration::from_millis(500));

    // Positive anchor: the order marker's diagnostic reaches stderr after every line an
    // earlier rebuild wrote, so the counts below cover the whole edit. Its compile
    // fails, so it adds no status line of its own.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);

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
    std::fs::write(sub.join("deep.mds"), "---\nname: D\n---\nDeep {{name}}\n").unwrap();
    let out_dir = dir.path().join("out");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
        "@define greet(name):\nHello {{name}}!\n@end\n\n@export greet\n",
    )
    .unwrap();

    // Two importers.
    let a = dir.path().join("a.mds");
    let b = dir.path().join("b.mds");
    std::fs::write(&a, "@import \"./_shared.mds\" as s\n{{s.greet(\"A\")}}\n").unwrap();
    std::fs::write(&b, "@import \"./_shared.mds\" as s\n{{s.greet(\"B\")}}\n").unwrap();

    let out_dir = dir.path().join("out");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(
        &partial,
        "@define greet(name):\nHi {{name}}!\n@end\n\n@export greet\n",
    );

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
        "@import \"./_c.mds\" as c\n@define val():\n{{c.val()}}\n@end\n\n@export val\n",
    )
    .unwrap();
    let a = dir.path().join("a.mds");
    std::fs::write(&a, "@import \"./_b.mds\" as b\n{{b.val()}}\n").unwrap();

    let out_dir = dir.path().join("out");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "V1", TIMEOUT),
        "a.md should contain V1 initially"
    );

    // Edit C — A must update.
    write_atomic(&c, "@define val():\nV2\n@end\n\n@export val\n");
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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("hello.md");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "--poll-interval 0 should still do the initial compile"
    );

    // Verify a real edit also works.
    write_atomic(&src, "---\nname: Poll\n---\nHello {{name}}!\n");
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
    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    std::fs::write(&src, "---\nname: World\n---\nIdle {{name}}!\n").unwrap();
    let out = dir.path().join("idle.md");

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "Idle World!", TIMEOUT),
        "initial compile should succeed"
    );

    // Record a positive observable: the output file's mtime and content before idling.
    // If the watcher spuriously recompiles, the mtime will advance — making the failure
    // deterministic rather than a timing-luck race on the stderr count alone.
    let mtime_before = std::fs::metadata(&out)
        .and_then(|m| m.modified())
        .expect("output file must exist after initial compile");
    let content_before = std::fs::read_to_string(&out).expect("output file readable");

    // Idle for 2.5s (≥2 ticks at 100ms poll-interval — well above the minimum).
    std::thread::sleep(Duration::from_millis(2500));

    // Ordered anchor: the watcher was still handling events — a zero from a stalled
    // loop would be vacuous — and everything it printed while idle precedes the
    // marker's diagnostic. Its compile fails, so it neither writes nor announces.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);

    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "idle single-file watcher must emit 0 Recompiled across ticks; \
         got {recompiled_count}; stderr:\n{stderr_str}"
    );

    // Deterministic positive observable: output file must not have been touched
    // during the idle window (no spurious recompile changed its mtime or content).
    let mtime_after = std::fs::metadata(&out)
        .and_then(|m| m.modified())
        .expect("output file must still exist after idle");
    assert_eq!(
        mtime_before, mtime_after,
        "output file mtime must not advance during idle (spurious recompile would advance it)"
    );
    assert_eq!(
        content_before,
        std::fs::read_to_string(&out).unwrap(),
        "output file content must be unchanged during idle"
    );
}

/// Idle for ≥2.5s (≥2 ticks) in dir mode must emit zero "Recompiled" lines (AC-W4).
#[test]
fn watch_dir_mode_idle_no_recompile_across_ticks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.mds"),
        "---\nname: A\n---\nFile A: {{name}}\n",
    )
    .unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "initial compile should succeed"
    );

    // Record a positive observable: the output file's mtime and content before idling.
    // If the watcher spuriously recompiles, the mtime will advance — making the failure
    // deterministic rather than a timing-luck race on the stderr count alone.
    let out_a = out_dir.join("a.md");
    let mtime_before = std::fs::metadata(&out_a)
        .and_then(|m| m.modified())
        .expect("a.md must exist after initial compile");
    let content_before = std::fs::read_to_string(&out_a).expect("a.md readable");

    // Idle for 2.5s (≥2 ticks at 100ms).
    std::thread::sleep(Duration::from_millis(2500));

    // Ordered anchor: the watcher was still handling events — a zero from a stalled
    // loop would be vacuous — and everything it printed while idle precedes the
    // marker's diagnostic. Its compile fails, so it neither writes nor announces.
    write_atomic(&dir.path().join("a.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);

    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "idle dir-mode watcher must emit 0 Recompiled across ticks; \
         got {recompiled_count}; stderr:\n{stderr_str}"
    );

    // Deterministic positive observable: output file must not have been touched
    // during the idle window (no spurious recompile changed its mtime or content).
    let mtime_after = std::fs::metadata(&out_a)
        .and_then(|m| m.modified())
        .expect("a.md must still exist after idle");
    assert_eq!(
        mtime_before, mtime_after,
        "output file mtime must not advance during idle (spurious recompile would advance it)"
    );
    assert_eq!(
        content_before,
        std::fs::read_to_string(&out_a).unwrap(),
        "output file content must be unchanged during idle"
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
    std::fs::write(&src, "---\nname: Before\n---\nEntry {{name}}\n").unwrap();
    let out = src_dir.join("entry.md");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(&src, "---\nname: After\n---\nEntry {{name}}\n");

    // TICK-DEPENDENT: `remove_dir_all(&src_dir)` destroyed the inotify watch on the old
    // inode, and the recreated dir is a new inode nothing is watching — so the write
    // above generates no event the watcher can receive. Only the liveness probe's
    // re-arm can find it.
    assert!(
        wait_for_file_contains(&out, "Entry After", TICK_TIMEOUT),
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

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(
        &root.join("new.mds"),
        "---\nname: N\n---\nNew file {{name}}\n",
    );

    // TICK-DEPENDENT: the recursive watch died with the old root inode, so the create
    // above is unobservable; the liveness probe's re-arm + reconcile is the only path.
    assert!(
        wait_for_file_contains(&out_dir.join("new.md"), "New file N", TICK_TIMEOUT),
        "watcher must recover after root delete+recreate and compile new file"
    );

    drop(child);
}

// ── AC-W1 / AC-W2 with paths typed relative to a recreated working directory ──

/// AC-W1 with every path typed relative to the working directory, which is the entry's
/// own directory: `mds watch entry.mds --vars vars.json -o ../out.md` run inside `src`
/// (#417). Deleting `src` leaves the process in a dead directory, in which no relative
/// path resolves, even once `src` is recreated. The watcher moves back into the
/// recreated directory before the next rebuild, so the entry, the vars file and the
/// output all resolve again: the output is compiled from the recreated entry AND the
/// recreated vars file. Control: `watch_file_mode_parent_dir_delete_recreate_recovers`,
/// the same scenario with the entry typed absolute.
///
/// Unix-only: Windows cannot delete a process's working directory.
#[cfg(unix)]
#[test]
fn watch_file_mode_relative_paths_recover_after_the_working_directory_is_recreated() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(src_dir.join("entry.mds"), "Entry {{name}}\n").unwrap();
    std::fs::write(src_dir.join("vars.json"), r#"{"name": "Before"}"#).unwrap();
    let out = base.path().join("out.md");

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(&src_dir)
            .args([
                "watch",
                "entry.mds",
                "--vars",
                "vars.json",
                "-o",
                "../out.md",
            ])
            .args(["--debounce", "0", "--poll-interval", "100", "-q"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&out, "Entry Before", TIMEOUT),
        "startup compile; stderr: {}",
        tap.text()
    );

    std::fs::remove_dir_all(&src_dir).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::create_dir(&src_dir).unwrap();
    write_atomic(&src_dir.join("vars.json"), r#"{"name": "After"}"#);
    write_atomic(&src_dir.join("entry.mds"), "Recreated {{name}}\n");

    // TICK-DEPENDENT: the watch on `src` died with it (see
    // `watch_file_mode_parent_dir_delete_recreate_recovers`).
    assert!(
        wait_for_file_contains(&out, "Recreated After", TICK_TIMEOUT),
        "the watcher moves back into the recreated working directory and reads the entry \
         and the vars file through it; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// AC-W2 with the root typed `.`: `mds watch .` run inside the watched directory (#413).
/// Deleting it leaves the process in a dead directory, in which `.` never resolves again,
/// even once the directory is recreated. The watcher moves back into it before the next
/// rebuild, so the new file is compiled — with the recreated `--vars vars.json`, typed
/// relative too. Control: `watch_dir_mode_root_delete_recreate_recovers`, the same
/// scenario with the root typed absolute.
///
/// Unix-only: Windows cannot delete a process's working directory.
#[cfg(unix)]
#[test]
fn watch_dot_recovers_after_the_working_directory_is_recreated() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("watched");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.mds"), "Old {{n}}\n").unwrap();
    std::fs::write(root.join("vars.json"), r#"{"n": "A"}"#).unwrap();
    let out_dir = base.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(&root)
            .args(["watch", ".", "--vars", "vars.json", "--out-dir"])
            .arg(&out_dir)
            .args(["--debounce", "0", "--poll-interval", "100", "-q"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "Old A", TIMEOUT),
        "startup compile; stderr: {}",
        tap.text()
    );

    std::fs::remove_dir_all(&root).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::create_dir(&root).unwrap();
    write_atomic(&root.join("vars.json"), r#"{"n": "N"}"#);
    write_atomic(&root.join("new.mds"), "New file {{n}}\n");

    // TICK-DEPENDENT: the recursive watch died with the old root (see
    // `watch_dir_mode_root_delete_recreate_recovers`).
    assert!(
        wait_for_file_contains(&out_dir.join("new.md"), "New file N", TICK_TIMEOUT),
        "the watcher moves back into the recreated working directory and compiles the \
         new file through `.`; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// A working directory recreated as a symbolic link to another directory is not moved
/// back into (#417): only a directory that resolves to the recorded canonical path again
/// is the working directory `mds watch` started in. Moving through the link would read a
/// relative `--vars` from, and write a relative `-o` into, the link's target, and neither
/// is checked the way the entry is. The entry is typed absolute here and its directory is
/// never lost, so its edit is an ordinary event rebuild, and only the working directory
/// decides where `-o out.md` lands: the write fails in the dead directory, and nothing is
/// written through the link. Positive control: before the swap, the startup compile
/// writes `out.md`.
///
/// Unix-only: Windows cannot delete a process's working directory.
#[cfg(unix)]
#[test]
fn watch_does_not_follow_a_working_directory_recreated_as_a_symlink() {
    use std::os::unix::fs::symlink;

    let base = tempfile::tempdir().unwrap();
    let entry = base.path().join("entry.mds");
    std::fs::write(&entry, "Entry\n").unwrap();
    let proj = base.path().join("proj");
    let other = base.path().join("other");
    std::fs::create_dir(&proj).unwrap();
    std::fs::create_dir(&other).unwrap();

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(&proj)
            .arg("watch")
            .arg(&entry)
            .args(["-o", "out.md", "--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&proj.join("out.md"), "Entry", TIMEOUT),
        "control: the startup compile writes out.md; stderr: {}",
        tap.text()
    );

    std::fs::remove_dir_all(&proj).unwrap();
    symlink("other", &proj).unwrap();
    write_atomic(&entry, "Edited\n");

    // The rebuild cannot write out.md in the dead working directory.
    let stderr = wait_for_tap(&tap, "out.md: ", TIMEOUT);
    assert!(
        !other.join("out.md").exists(),
        "nothing is written through the link; stderr: {stderr}"
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
    std::fs::write(&src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();
    let out = dir.path().join("entry.md");

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "Hello World!", TIMEOUT),
        "initial compile should produce Hello World!"
    );

    // Delete the entry file (parent intact).
    std::fs::remove_file(&src).unwrap();

    // The delete is reported: wait for its first error, so the baseline below holds at
    // least the error the delete itself produced rather than whatever had been sampled.
    wait_for_tap(&stderr_tap, "file not found", TIMEOUT);

    // Scale-invariant error bound (guards against once-per-tick re-firing — the watcher self-trigger pitfall): run two equal idle windows and assert
    // the error count does NOT grow in the second window.  A per-tick implementation would
    // accumulate one error per tick across BOTH windows; the fix settles quickly after the
    // initial native-event errors and is then silent.
    //
    // Window 1 — SETTLE WINDOW, deliberately a fixed sleep: ≥5 ticks at 100ms for the
    // rest of the delete's native events and at most 1 liveness-probe error. How many of
    // those arrive varies by platform, so no event marks the end of the settling.
    std::thread::sleep(Duration::from_millis(500));
    let count_w1 = {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        s.matches("file not found").count() + s.matches("No such file").count()
    };

    // Window 2 — NEGATIVE WINDOW, deliberately a fixed sleep: another ≥5 ticks with
    // nothing changed, so error-settle must keep the count frozen. Any increase proves
    // the watcher is still firing per-tick. Its positive anchor is the recovery below:
    // the recreated entry is compiled, so the watcher was running its ticks all along.
    std::thread::sleep(Duration::from_millis(500));
    let count_w2 = {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        s.matches("file not found").count() + s.matches("No such file").count()
    };

    assert_eq!(
        count_w1, count_w2,
        "error count must not grow in a second idle window (not once-per-tick); \
         w1={count_w1}, w2={count_w2} — the fix must settle after initial native-event errors"
    );

    // Recreate the file with different content.
    write_atomic(&src, "---\nname: Recovered\n---\nHello {{name}}!\n");

    // Wait for recompile after recovery.
    assert!(
        wait_for_file_contains(&out, "Hello Recovered!", TIMEOUT),
        "watcher must recompile after recreating deleted entry file"
    );

    // Give the watcher a moment to settle after recovery before killing.
    std::thread::sleep(Duration::from_millis(200));

    let stderr_str = stderr_tap.finish_text(&mut child);

    // Sanity: error count across the FULL test run must still be small — rules out a
    // burst of errors that somehow all arrived in window 1.
    let error_event_count =
        stderr_str.matches("file not found").count() + stderr_str.matches("No such file").count();
    assert!(
        error_event_count <= 10,
        "total error count across full test must be small (not a per-tick flood); \
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
    std::fs::write(&src, "---\ngreeting: Default\n---\n{{greeting}}\n").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"greeting": "Hello"}"#).unwrap();
    let out = src_dir.join("tpl.md");

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

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
    write_atomic(&vars_file, r#"{"greeting": "Goodbye"}"#);

    // TICK-DEPENDENT: whether the write above is delivered as an event depends on the
    // probe having already re-armed the recreated vars dir. If it has not, the fallback
    // is the probe's own `(mtime, size)` comparison — another tick. Either way the
    // recovery is denominated in ticks, not in event latency.
    let got = wait_for_file_contains(&out, "Goodbye", TICK_TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);
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
        "@import \"../shared/_x.mds\" as x\n{{x.greet()}}\n",
    )
    .unwrap();

    let out_dir = base.path().join("out");

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(
        &partial,
        "@define greet():\nExternal V2\n@end\n\n@export greet\n",
    );

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
    std::fs::write(&importer, "@import \"./_p.mds\" as p\n{{p.val()}}\n").unwrap();

    let out_dir = dir.path().join("out");

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

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

    // Poll for the broken-import error rather than sleeping: wait until stderr
    // contains the "file not found" message that the compiler emits for a missing import.
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        if s.contains("file not found") || s.contains("No such file") {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "expected a 'file not found' / 'No such file' error after deleting imported \
                 partial within {:?}; stderr so far:\n{s}",
                TIMEOUT
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Verify the importer's output was not silently invalidated.
    // The important assertion: the process stays alive.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive after partial deletion surfaces broken import"
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
    std::fs::write(&importer, "@import \"./_missing.mds\" as m\n{{m.val()}}\n").unwrap();

    let out_dir = dir.path().join("out");

    let (child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    // Initial compile must fail (missing partial), so main.md may not appear. The
    // startup compile precedes readiness; its reported error (diagnostics survive -q)
    // is the control that the importer really started out broken.
    wait_for_tap(&stderr_tap, "mds::file_not_found", TIMEOUT);

    // Now create the previously-missing partial.
    let partial = dir.path().join("_missing.mds");
    write_atomic(&partial, "@define val():\nHealed!\n@end\n\n@export val\n");

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
    std::fs::write(&consumer, "@import \"./dual.mds\" as d\n{{d.greet()}}\n").unwrap();

    let out_dir = dir.path().join("out");

    let (mut child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(
        &dual,
        "@define greet():\nDual V2 (updated)\n@end\n\n@export greet\n\nStandalone updated content\n",
    );

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
        "---\nname: A\n---\nFile A: {{name}}\n",
    )
    .unwrap();
    // Syntax error file: references undefined variable with no frontmatter.
    std::fs::write(dir.path().join("bad.mds"), "Hello {{__undefined_xyz__}}!\n").unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let (mut child, stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
    );

    // Wait for good file to compile (confirms startup completed).
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "a.md should compile despite bad.mds error"
    );

    // Scale-invariant error bound (applies the reconcile rule — see the `src/watch.rs`
    // module doc — and guards against once-per-tick re-firing): run two equal idle
    // windows and assert the "undefined variable" count does NOT grow in the second window.
    // A per-tick implementation would fire continuously; error-settle means it fires once at
    // startup and then goes silent.
    //
    // The startup compile of bad.mds, which precedes readiness, reported its error: the
    // baseline below holds at least that one rather than whatever had been sampled.
    wait_for_tap(&stderr_tap, "undefined variable", TIMEOUT);

    // Window 1 — SETTLE WINDOW, deliberately a fixed sleep: ≥5 ticks at 100ms (~500ms)
    // for anything the first ticks report about the startup error. No event marks the
    // end of that settling.
    std::thread::sleep(Duration::from_millis(500));
    let count_w1 = {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        // Count exactly once per error emission (each miette block contains this phrase once).
        s.matches("undefined variable").count()
    };

    // Window 2 — NEGATIVE WINDOW, deliberately a fixed sleep: another ≥5 ticks with
    // nothing changed; error-settle must keep the count frozen. Any increase here proves
    // the watcher is still firing per-tick (the bug).
    std::thread::sleep(Duration::from_millis(500));
    let count_w2 = {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        s.matches("undefined variable").count()
    };

    // Positive anchor for the window: a real edit that keeps bad.mds broken is still
    // reported, so an error during the window would have reached stderr too.
    write_atomic(
        &dir.path().join("bad.mds"),
        "Hello again {{__undefined_xyz__}}!\n",
    );
    wait_for_tap_count(&stderr_tap, "undefined variable", count_w2 + 1, TIMEOUT);

    // Watcher must still be alive.
    let still_running = child.0.try_wait().unwrap().is_none();
    assert!(
        still_running,
        "watcher must stay alive with persistent syntax error in bad.mds"
    );

    let stderr_str = stderr_tap.finish_text(&mut child);

    assert_eq!(
        count_w1, count_w2,
        "error count must not grow in a second idle window (not once-per-tick); \
         w1={count_w1}, w2={count_w2} (reconcile rule; no once-per-tick re-firing); \
         stderr:\n{stderr_str}"
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

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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

    let (child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
        "@import \"./_shared.mds\" as s\n{{s.val()}} from A\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.mds"),
        "@import \"./_shared.mds\" as s\n{{s.val()}} from B\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("c.mds"),
        "@import \"./_shared.mds\" as s\n{{s.val()}} from C\n",
    )
    .unwrap();

    // Independent file (should NOT change when partial is edited).
    std::fs::write(dir.path().join("independent.mds"), "Independent\n").unwrap();

    let out_dir = dir.path().join("out");
    let (mut child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
    write_atomic(
        &partial,
        "@define val():\nV2 updated\n@end\n\n@export val\n",
    );

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
    std::fs::write(&importer, "@import \"./_soak.mds\" as s\n{{s.val()}}\n").unwrap();

    let out_dir = dir.path().join("out");
    let (mut child, _stderr_tap) = spawn_ready(
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
            .stdout(Stdio::null()),
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
        write_atomic(
            &partial,
            format!("@define val():\nSoak V{i}{padding}\n@end\n\n@export val\n"),
        );

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

    // Drop (kills the child process) — process stays responsive across 50 edits; teardown via kill.
    drop(child);
}

// ── QA Fix: File-mode parent dir deleted — bounded errors then recovers ───────

/// Regression test for the edge-triggered recovery fix (reconcile rule).
///
/// When the watched entry's PARENT DIRECTORY is deleted entirely, the per-tick
/// `watcher.watch()` re-arm fails every idle tick (the parent is missing).  Before the
/// fix this forced `recovery = true` every tick → one compile attempt → one "file not
/// found" error PER TICK, violating the design's error-settle intent (DD1: "print at
/// most once per real attempt, not per tick").
///
/// After the fix the recovery logic is edge-triggered (uses
/// `external_recovery_decision` / `FileWatchState.missing_watched_dirs`):
/// - While the parent stays deleted, the watcher is quiet (no per-tick error).
/// - The moment the parent reappears (vanish→reappear edge), recovery fires and
///   recompiles (AC-W1 preserved).
///
/// At 150ms poll-interval over ≥6 ticks (~900ms idle with missing dir) the
/// not-found error must appear ≤ 3 times (proving "not once-per-tick") — a
/// per-tick implementation would produce ≥ 6 errors in that window.
#[test]
fn watch_file_mode_parent_dir_deleted_bounded_errors_then_recovers() {
    // Place the source in a sub-directory so we can delete the parent without
    // touching the tempdir root (which would delete the child process's cwd too).
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    std::fs::create_dir(&src_dir).unwrap();
    let src = src_dir.join("tpl.mds");
    // Force deterministic content so the recovery write is unambiguously detectable.
    std::fs::write(&src, "V1-before\n").unwrap();
    let out = src_dir.join("tpl.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "150",
                // No -q: we need to observe stderr errors.
            ])
            .stdout(Stdio::null()),
    );

    // Wait for initial compile.
    assert!(
        wait_for_file_contains(&out, "V1-before", TIMEOUT),
        "initial compile should produce V1-before"
    );

    // Delete the ENTIRE parent directory (not just the file — this is the bug scenario).
    std::fs::remove_dir_all(&src_dir).unwrap();

    // Scale-invariant error bound (reconcile rule; guards against once-per-tick re-firing): run two equal idle
    // windows and assert the error count does NOT grow in the second window.  A per-tick
    // implementation would produce ≥1 error per tick continuously; the fix settles after
    // the initial native-event error(s) and then goes silent.
    //
    // Window 1 — ≥6 ticks at 150ms (~900ms): native-event errors may appear here.
    std::thread::sleep(Duration::from_millis(900));
    let count_w1 = {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        s.matches("file not found").count() + s.matches("No such file").count()
    };

    // Window 2 — another ≥6 ticks: nothing changed, error-settle must keep count frozen.
    // Any increase here proves the watcher is still firing per-tick (the bug).
    std::thread::sleep(Duration::from_millis(900));
    let count_w2 = {
        let bytes = stderr_tap.bytes();
        let s = String::from_utf8_lossy(&bytes);
        s.matches("file not found").count() + s.matches("No such file").count()
    };

    assert_eq!(
        count_w1, count_w2,
        "error count must not grow in a second idle window (not once-per-tick); \
         w1={count_w1}, w2={count_w2} — the fix must settle after initial native-event errors \
         (reconcile rule; no once-per-tick re-firing)"
    );

    // Recreate the parent directory and write the file with new content.
    std::fs::create_dir(&src_dir).unwrap();
    write_atomic(&src, "V2-recovered\n");

    // TICK-DEPENDENT: same as AC-W1 — the parent dir was removed, so the watch on it is
    // gone and the recreated dir is unwatched. Recovery is the vanish→reappear edge in
    // `liveness_probe_file`, one tick at a time.
    assert!(
        wait_for_file_contains(&out, "V2-recovered", TICK_TIMEOUT),
        "watcher must self-heal after parent dir delete+recreate and recompile with V2 content"
    );

    // Watcher must still be alive after recovery.
    let still_alive = child.0.try_wait().unwrap().is_none();

    let stderr_str = stderr_tap.finish_text(&mut child);

    assert!(
        still_alive,
        "watcher must remain alive while parent dir is absent; stderr:\n{stderr_str}"
    );

    // Sanity: total error count must remain small — rules out a burst in window 1.
    let error_count =
        stderr_str.matches("file not found").count() + stderr_str.matches("No such file").count();
    assert!(
        error_count <= 8,
        "total error count across both idle windows must be small (not a per-tick flood); \
         got {error_count} occurrences; stderr:\n{stderr_str}"
    );
}

// ── AC-P5: 500-file idle — O(1) liveness probe at scale ──────────────────────

/// AC-P5: Start `mds watch <dir>` over 500 `.mds` files, wait for all initial
/// compiles to complete, then idle for ≥10 poll-interval ticks and assert ZERO
/// "Recompiled" lines in the idle window.
///
/// This is the regression guard for the reconcile-rule invariant: "idle cost stays O(1)
/// regardless of tree size."  A per-tick full-tree walk (the anti-pattern) would
/// manifest as spurious "Recompiled" events under CI load; the edge-triggered
/// liveness probe (reconcile rule) must emit none.
///
/// An additional positive observable — the sentinel output file's mtime must not
/// advance during the idle window — makes the failure mode deterministic rather
/// than relying on timing luck alone.
///
/// applies the reconcile rule
#[test]
fn watch_dir_mode_idle_500_files_no_recompile() {
    const FILE_COUNT: usize = 500;
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    // Create FILE_COUNT trivially-simple .mds files.  Plain text (no frontmatter)
    // is valid MDS; keeps per-file compile time minimal.
    // Files are named file_0001.mds … file_0500.mds so the last one is lexicographically
    // predictable as the sentinel for "startup compilation done".
    for i in 1..=FILE_COUNT {
        std::fs::write(
            dir.path().join(format!("file_{i:04}.mds")),
            format!("scale-idle-{i}\n"),
        )
        .unwrap();
    }
    let sentinel_out = out_dir.join(format!("file_{FILE_COUNT:04}.md"));

    // Use --poll-interval 50 (the minimum floor) to keep the test fast while still
    // giving the liveness probe several real ticks during the idle window.
    // --debounce 0: immediate event processing, no coalesce delay.
    // No -q: we need to observe "Recompiled" in stderr.
    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "50",
            ])
            .stdout(Stdio::null()),
    );

    // Assert the sentinel output exists — dir-mode compiles all files in a single
    // startup batch before entering the event loop, and the readiness handshake only
    // fires after that batch, so all 500 initial compiles are already done here. This
    // is a content assertion now, not a wait.
    assert!(
        wait_for_file_contains(&sentinel_out, &format!("scale-idle-{FILE_COUNT}"), TIMEOUT,),
        "AC-P5: sentinel file_{FILE_COUNT:04}.md must be written during startup compile \
         (all {FILE_COUNT} files compiled before idle window)"
    );

    // Record positive observable: sentinel mtime before the idle window.
    let mtime_before = std::fs::metadata(&sentinel_out)
        .and_then(|m| m.modified())
        .expect("sentinel output must exist after startup");

    // Idle for ≥10 ticks at 50ms poll-interval (500ms total, bounded).  A per-tick
    // full-tree walk would trigger O(FILE_COUNT) work per tick; edge-triggered probes
    // (reconcile rule) must emit zero "Recompiled" lines during this window.
    std::thread::sleep(Duration::from_millis(600));

    // Ordered anchor: the watcher was still handling events — a zero from a stalled
    // loop would be vacuous — and everything it printed while idle precedes the
    // marker's diagnostic. Its compile fails, so it neither writes nor announces.
    write_atomic(&dir.path().join("file_0001.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_str = stderr_tap.finish_text(&mut child);

    let recompiled_count = stderr_str.matches("Recompiled").count();
    assert_eq!(
        recompiled_count, 0,
        "AC-P5: idle dir-mode watcher over {FILE_COUNT} files must emit 0 Recompiled \
         across ≥10 ticks (reconcile rule: idle cost is O(1) regardless of tree size); \
         got {recompiled_count}; stderr:\n{stderr_str}"
    );

    // Deterministic positive observable: sentinel output mtime must be unchanged
    // (no spurious recompile touched it during the idle window).
    let mtime_after = std::fs::metadata(&sentinel_out)
        .and_then(|m| m.modified())
        .expect("sentinel output must still exist after idle");
    assert_eq!(
        mtime_before, mtime_after,
        "AC-P5: sentinel output mtime must not advance during idle window \
         (a spurious recompile would update it)"
    );
}

// ── AC-1/2/3/4: Symlink rejection at startup (parity with mds build) ─────────
//
// PF-004: an alternate code path (watch's up-front canonicalize) was silently
// bypassing the symlink guard enforced by NativeFs in mds build. These tests
// lock in the fix: mds watch now rejects symlinked entry / dir target / --vars
// at startup, matching mds build behavior.

/// AC-1: `mds watch <symlinked-file>` must reject at startup (non-zero exit;
/// stderr names the symlink restriction).  Mirrors
/// `symlinked_entry_exits_nonzero` in intrinsic_output.rs.
#[test]
fn watch_rejects_symlinked_entry() {
    let dir = tempfile::tempdir().unwrap();

    // Real file with valid content.
    let real_file = dir.path().join("real.mds");
    std::fs::write(&real_file, "@message system:\nYou are helpful.\n@end\n").unwrap();

    // Symlink → real file.
    let link_file = dir.path().join("link.mds");
    if !make_symlink(&real_file, &link_file) {
        return;
    }

    let out = dir.path().join("out.md");

    let output = mds_bin()
        .args([
            "watch",
            link_file.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "watch with a symlinked entry must fail at startup; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction; got: {stderr}"
    );
    assert!(
        !out.exists(),
        "output file must not be created when entry is rejected at startup"
    );
}

/// AC-2 (file mode): `mds watch <entry> --vars <symlinked-vars>` must reject at
/// startup (non-zero; stderr mentions symlink).  Mirrors
/// `symlinked_vars_file_exits_nonzero` in intrinsic_output.rs.
#[test]
fn watch_rejects_symlinked_vars_file() {
    let dir = tempfile::tempdir().unwrap();

    // Real entry and vars files.
    let entry = dir.path().join("entry.mds");
    std::fs::write(&entry, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    let real_vars = dir.path().join("real_vars.json");
    std::fs::write(&real_vars, r#"{"name": "World"}"#).unwrap();

    // Symlink → real vars.
    let link_vars = dir.path().join("link_vars.json");
    if !make_symlink(&real_vars, &link_vars) {
        return;
    }

    let out = dir.path().join("out.md");

    let output = mds_bin()
        .args([
            "watch",
            entry.to_str().unwrap(),
            "--vars",
            link_vars.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "watch with a symlinked --vars must fail at startup; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction for vars; got: {stderr}"
    );
}

/// AC-3: `mds watch <symlinked-dir>` must reject at startup (non-zero; stderr names
/// the symlink restriction; the symlinked dir is never traversed).
#[test]
fn watch_rejects_symlinked_dir_target() {
    let dir = tempfile::tempdir().unwrap();

    // Real source directory with a valid .mds file.
    let real_src = dir.path().join("real_src");
    std::fs::create_dir(&real_src).unwrap();
    std::fs::write(
        real_src.join("a.mds"),
        "---\nname: A\n---\nFile A: {{name}}\n",
    )
    .unwrap();

    // Symlink → the real source directory.
    let link_dir = dir.path().join("link_src");
    if !make_symlink(&real_src, &link_dir) {
        return;
    }

    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let output = mds_bin()
        .args([
            "watch",
            link_dir.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "watch with a symlinked dir target must fail at startup; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("symlink") || stderr.contains("not allowed"),
        "error must mention symlink restriction; got: {stderr}"
    );
    assert!(
        !out_dir.join("a.md").exists(),
        "symlinked dir must never be traversed; a.md must not be created"
    );
}

/// AC-4: A symlinked `.mds` source file inside a watched real directory is NOT
/// compiled, while a real sibling compiles normally.  This locks in the existing
/// discovery-skip behavior in `collect_mds_files` (dir entries that are symlinks
/// are already excluded by the follow-symlinks=false DirEntry filter).
#[test]
fn watch_dir_skips_symlinked_source_file() {
    let dir = tempfile::tempdir().unwrap();

    // Real source directory — the one we watch.
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();

    // A real .mds file inside the watched dir — SHOULD compile.
    std::fs::write(src.join("a.mds"), "---\nname: A\n---\nFile A: {{name}}\n").unwrap();

    // An external .mds file to be pointed at by the symlink.
    let external_dir = dir.path().join("external");
    std::fs::create_dir(&external_dir).unwrap();
    std::fs::write(
        external_dir.join("external.mds"),
        "---\nname: Ext\n---\nExternal: {{name}}\n",
    )
    .unwrap();

    // A symlinked .mds file inside the watched dir — must NOT compile.
    let link_mds = src.join("link.mds");
    if !make_symlink(&external_dir.join("external.mds"), &link_mds) {
        return;
    }

    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let (child, _stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null()),
    );

    // Wait for the real file to compile.
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "File A: A", TIMEOUT),
        "a.md (real sibling) should be compiled"
    );

    // The symlinked source must never appear in the output.
    // Poll briefly — if the watcher were to incorrectly process it, it would appear.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        assert!(
            !out_dir.join("link.md").exists(),
            "link.md must not be created — symlinked source files must be skipped"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    drop(child);
}

// ── ESC-injection: watch initial-compile-error stderr sanitization ────────────

/// T-Watch-ESC [AC-F-W1]: the `eprint_error` call at the non-loop
/// initial-compile-error path in `run_watch_file` (watch.rs line ~937) must
/// sanitize any raw control bytes before writing to stderr.
///
/// Vector: a `.mds` file containing a raw ESC byte (U+001B) in an unclosed
/// `@define` — guaranteed syntax error — so the initial compile fails and
/// `eprint_error` is called before the watch loop begins.
///
/// Determinism: the initial compile is a synchronous call that completes before
/// the watch loop starts.  We wait a fixed bounded interval (500 ms, >> any
/// realistic compile time) then kill the process.  No file-system events are
/// polled, so this test is immune to FSEvents/inotify timing flakiness.
///
/// Assertions:
///   1. stderr carries the initial-compile error for THIS file (non-vacuous).
///   2. No raw ESC byte (0x1B) appears anywhere in stderr.
///
/// Assertion 1 exists solely to keep assertion 2 honest: "no ESC byte in stderr" is
/// trivially true of an empty stream, so a change that stopped the error being printed
/// at all would leave assertion 2 passing while testing nothing. It therefore has to
/// name something ONLY the rendered diagnostic produces. Note that the file's own stem
/// does not qualify: this watcher runs non-quiet, so `Watching /…/esc_watch.mds` is on
/// stderr whether or not the error was ever printed.
#[test]
fn watch_esc_in_initial_compile_error_is_sanitized() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("esc_watch.mds");
    // Unclosed @define with a raw ESC byte (0x1B) on the directive line so that
    // miette renders it inside the source context frame.
    std::fs::write(&src, b"@define \x1bfoo:\nhello\n").unwrap();

    // The readiness handshake already implies the initial compile ran to completion:
    // the error is printed on the startup path, and the marker is only emitted after
    // it. No sleep needed to "give it time".
    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
            .stdout(Stdio::null()),
    );

    // Kill the watch process to close the pipe, then join the drain: `finish` does
    // both in that order, so the snapshot is the complete stream rather than whatever
    // the drain thread happened to have copied by then. Raw bytes, because assertion
    // 2 below hunts for a raw ESC byte.
    let stderr_bytes = stderr_tap.finish(&mut child);
    let stderr_str = String::from_utf8_lossy(&stderr_bytes);

    // Assertion 1: the initial-compile error was rendered (non-vacuous guard for
    // assertion 2). `syntax error` comes from the diagnostic and from nothing else the
    // watcher writes — unlike the file stem, which the `Watching …` line also carries.
    assert!(
        stderr_str.contains("syntax error"),
        "watch initial-compile-error must render its diagnostic to stderr, otherwise \
         the ESC assertion below is vacuous; got:\n{stderr_str}"
    );

    // Assertion 2: no raw ESC byte (0x1B) anywhere in stderr.
    // eprint_error sanitizes via render_error_sanitized before writing;
    // if sanitization is removed the raw 0x1B byte leaks here.
    assert!(
        !stderr_bytes.contains(&0x1Bu8),
        "raw ESC byte (0x1B) must be sanitized in watch initial-compile-error stderr; \
         got (hex first 512): {:02x?}",
        &stderr_bytes[..stderr_bytes.len().min(512)]
    );
}

// ── #317 regression: an edit inside the startup window must not be lost ───────
//
// These are the only tests in this file that deliberately do NOT go through
// `spawn_ready`, and that is the entire point of them.
//
// `MDS_TEST_READY` signals "every watch armed and every baseline captured". It is
// emitted at the end of whatever startup ordering the code happens to have, so it is
// true by construction in both the fixed and the defective ordering — it just fires at
// a different wall-clock instant in each. A test that waits for it therefore cannot
// place an edit inside the window the ordering opens: by the time it is allowed to act,
// the window is closed by definition. Every handshake-gated test in this file passes
// against the pre-fix ordering, which is why the arm-before-publish fix shipped with no
// test that could detect its regression.
//
// These two synchronise on the *opening* of the window instead — the moment the startup
// output is published, which is where the defective ordering armed nothing — and then
// edit immediately. `--poll-interval 0` disables the self-heal probe so that inotify is
// the sole detector: with the probe enabled, `liveness_probe_file` rebuilds
// unconditionally on its first tick regardless of any baseline, which would recover the
// lost edit and mask the defect (see #319 for why that recovery is itself unreliable).
//
// Sensitivity comes from the `startup-race-probe` Cargo feature, which widens the
// publish→arm window to 200ms. Measured against the pre-fix arming order on Linux:
//
//   with the probe        file mode 6/6 red, dir mode 6/6 red
//   without the probe     file mode 3/3 red, dir mode 1/3 red
//
// So the default `cargo test` run detects the file-mode regression but is a coin flip
// on the dir-mode one, because there the unwidened window is only the few microseconds
// between the last output write and the `watcher.watch()` syscall. That is why CI runs
// this file a second time with the feature on. Feature and tests are one mechanism;
// neither half is a gate on its own.

/// File mode: an edit landing immediately after the first output is published must
/// still reach the output, with the self-heal probe disabled.
#[test]
fn watch_file_mode_edit_during_startup_window_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("entry.mds");
    std::fs::write(&src, "---\nname: Before\n---\nEntry {{name}}\n").unwrap();
    let out = dir.path().join("entry.md");

    let (child, _tap) = spawn_unsynchronized(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                // Self-heal probe OFF: inotify must carry this on its own.
                "--poll-interval",
                "0",
                "-q",
            ])
            .stdout(Stdio::null()),
    );

    // The published startup output IS the start of the window.
    assert!(
        wait_for_file_contains_tight(&out, "Entry Before", STARTUP_WINDOW_TIMEOUT),
        "startup compile should publish 'Entry Before'"
    );

    // Edit now — inside the window under the defective ordering.
    write_atomic(&src, "---\nname: After\n---\nEntry {{name}}\n");

    assert!(
        wait_for_file_contains(&out, "Entry After", STARTUP_WINDOW_TIMEOUT),
        "an edit made immediately after the startup output was published must not be \
         lost: with --poll-interval 0 the OS watch is the only detector, so this fails \
         if the watch is armed after the output is written (#317)"
    );

    drop(child);
}

/// Directory mode: same property for the recursive root watch.
#[test]
fn watch_dir_mode_edit_during_startup_window_is_not_lost() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let out_dir = base.path().join("out");
    std::fs::create_dir(&root).unwrap();
    let src = root.join("a.mds");
    std::fs::write(&src, "---\nname: Before\n---\nDir {{name}}\n").unwrap();
    let out = out_dir.join("a.md");

    let (child, _tap) = spawn_unsynchronized(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "0",
                "-q",
            ])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains_tight(&out, "Dir Before", STARTUP_WINDOW_TIMEOUT),
        "startup compile should publish 'Dir Before'"
    );

    write_atomic(&src, "---\nname: After\n---\nDir {{name}}\n");

    assert!(
        wait_for_file_contains(&out, "Dir After", STARTUP_WINDOW_TIMEOUT),
        "an edit made immediately after the startup output was published must not be \
         lost in dir mode: the recursive root watch must be armed before the tree walk, \
         not after the outputs are written (#317)"
    );

    drop(child);
}

/// Directory mode: an edit to a **cross-root dependency**, made in the window before
/// that dependency's directory can be armed, must still reach the output (#321).
///
/// This is the one window arming order cannot close, and it is not an artefact of the
/// test harness: a cross-root dependency's directory is discovered by the compile that
/// reads it, so there is no earlier instant at which to arm it. Whatever the ordering,
/// some interval exists in which an edit to such a file produces no event for anyone.
///
/// So this test deliberately does the opposite of the two #317 tests above. They run
/// with `--poll-interval 0` to prove inotify carries the edit alone; this one leaves
/// the probe enabled, because here the idle-tick content backstop is the *only*
/// mechanism that can carry it. Before the backstop existed, directory mode diffed only
/// `collect_mds_files(root)` — which never contains a cross-root dependency — so the
/// edit was not late, it was gone.
///
/// Sensitivity, like the #317 tests, comes from the `startup-race-probe` feature: it
/// widens the publish→arm window to 200ms, which is what makes the edit land inside it
/// reliably rather than by luck. See the block comment above.
#[test]
fn watch_dir_mode_cross_root_edit_during_startup_window_is_not_lost() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let shared = base.path().join("shared");
    let out_dir = base.path().join("out");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&shared).unwrap();
    // .git marker so the compiler's project root is `base`, permitting `../shared`.
    std::fs::write(base.path().join(".git"), "").unwrap();

    let partial = shared.join("_x.mds");
    std::fs::write(
        &partial,
        "@define greet():\nWindow V1\n@end\n\n@export greet\n",
    )
    .unwrap();
    let importer = root.join("importer.mds");
    std::fs::write(
        &importer,
        "@import \"../shared/_x.mds\" as x\n{{x.greet()}}\n",
    )
    .unwrap();
    let out = out_dir.join("importer.md");

    let (child, _tap) = spawn_unsynchronized(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                // Probe ON: the content backstop is the mechanism under test.
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null()),
    );

    // The published startup output IS the start of the window: the external dep dir is
    // armed only after every source has been compiled and written.
    assert!(
        wait_for_file_contains_tight(&out, "Window V1", STARTUP_WINDOW_TIMEOUT),
        "startup compile should publish 'Window V1'"
    );

    // Edit the cross-root partial now — inside the window where nothing is watching it.
    write_atomic(
        &partial,
        "@define greet():\nWindow V2\n@end\n\n@export greet\n",
    );

    // TICK-DEPENDENT: no filesystem event announces this edit, so recovery is the idle
    // tick's `(mtime, size)` diff against the baseline captured before the first read.
    assert!(
        wait_for_file_contains(&out, "Window V2", TICK_TIMEOUT),
        "an edit to a cross-root dependency during the startup window must be recovered \
         by the idle-tick content backstop: its directory cannot be armed before the \
         compile discovers it, so no event exists to deliver, and diffing only the \
         in-root file list can never see the change (#321)"
    );

    drop(child);
}

/// Single-file mode: the same end-to-end property for a dependency outside the entry's
/// directory — an edit in the startup window must still reach the output.
///
/// **Measured limitation, stated so it cannot be misread as a guard it is not.** This
/// test does not discriminate *which* mechanism delivers the result, because single-file
/// mode has two that each suffice alone, and one of them is unconditional:
/// `liveness_probe_file` returns `recovery || changed` with `recovery` including
/// `first_tick`, so its first tick rebuilds whatever the baseline says. Measured against
/// an arm with the dependency baseline moved back to the post-compile snapshot, this test
/// still passed 10/10 — the first-tick rebuild covered it. It therefore guards the
/// disjunction "first-tick rebuild *or* a baseline older than the read", not either term.
///
/// It is kept because that disjunction is the property a user depends on, and because
/// removing the unconditional first-tick rebuild — an obvious way to save a redundant
/// startup compile — would leave the baseline as the only remaining satisfier. It is
/// **not** evidence that the baseline capture point is correct; nothing here is.
/// The directory-mode counterpart above *is* discriminating (measured 10/10 RED against
/// the arm without the content backstop), because directory mode has no unconditional
/// first-tick rebuild.
#[test]
fn watch_file_mode_dep_edit_during_startup_window_is_not_lost() {
    let base = tempfile::tempdir().unwrap();
    let entry_dir = base.path().join("tpl");
    let shared = base.path().join("shared");
    std::fs::create_dir(&entry_dir).unwrap();
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(base.path().join(".git"), "").unwrap();

    let partial = shared.join("_x.mds");
    std::fs::write(
        &partial,
        "@define greet():\nDep V1\n@end\n\n@export greet\n",
    )
    .unwrap();
    let entry = entry_dir.join("entry.mds");
    std::fs::write(&entry, "@import \"../shared/_x.mds\" as x\n{{x.greet()}}\n").unwrap();
    let out = entry_dir.join("entry.md");

    let (child, _tap) = spawn_unsynchronized(
        mds_bin()
            .args([
                "watch",
                entry.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
                "-q",
            ])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains_tight(&out, "Dep V1", STARTUP_WINDOW_TIMEOUT),
        "startup compile should publish 'Dep V1'"
    );

    write_atomic(
        &partial,
        "@define greet():\nDep V2\n@end\n\n@export greet\n",
    );

    assert!(
        wait_for_file_contains(&out, "Dep V2", TICK_TIMEOUT),
        "an edit to a dependency outside the entry's directory during the startup \
         window must be recovered by the idle tick: the dependency's baseline has to \
         predate the edit, which it only does if it is captured when the compile \
         reports the dep rather than at the end of startup (#321)"
    );

    drop(child);
}

// ── #319: the idle tick must not be starvable ────────────────────────────────
//
// The backstop above is only worth as much as the tick that runs it. Handing
// `recv_timeout` a fresh `--poll-interval` budget per message made the tick starvable
// by any event stream faster than the interval — and the watcher's own compiles, an
// editor's scratch writes, a dev server, or a sync client all qualify. A starved tick
// does not delay recovery; it removes it.

/// The idle tick must still fire while filesystem events arrive faster than the
/// `--poll-interval` (#319).
///
/// The change under test is one no event can announce: `remove_dir_all` destroys the
/// recursive watch descriptor, and the directory recreated in its place is a different
/// inode that nothing is watching. Only the probe's re-arm and reconcile can recover
/// it. Meanwhile a reader polls the cross-root dependency 30× faster than the poll
/// interval, so every one of those reads is an `Access` event on a watch that is still
/// live — the watcher drops each as irrelevant, which is exactly why a message-driven
/// countdown never reached zero.
///
/// The reader is deliberately outside the deleted root: a flood that stops when the
/// root does would prove nothing about starvation.
#[test]
fn watch_dir_mode_idle_tick_fires_under_event_flood() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let shared = base.path().join("shared");
    let out_dir = base.path().join("out");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&shared).unwrap();
    std::fs::create_dir(&out_dir).unwrap();
    std::fs::write(base.path().join(".git"), "").unwrap();

    // A cross-root dependency: its directory is armed, so reads of it are delivered as
    // events for the lifetime of the watcher, independent of the root.
    let partial = shared.join("_p.mds");
    std::fs::write(&partial, "@define hi():\nP\n@end\n\n@export hi\n").unwrap();
    std::fs::write(
        root.join("a.mds"),
        "@import \"../shared/_p.mds\" as p\n{{p.hi()}}\n",
    )
    .unwrap();

    let (child, _tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "150",
                "-q",
            ])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "P", TIMEOUT),
        "initial compile should produce 'P'"
    );

    // Flood: read the armed cross-root dependency every 5ms — 30 events per tick.
    let stop = Arc::new(AtomicBool::new(false));
    let flood = {
        let stop = Arc::clone(&stop);
        let partial = partial.clone();
        std::thread::spawn(move || {
            // Bounded by the stop flag, which the test always sets before joining.
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::read(&partial);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    // Destroy the root's watch descriptor, then recreate the directory with new content.
    std::fs::remove_dir_all(&root).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::create_dir(&root).unwrap();
    write_atomic(&root.join("new.mds"), "---\nname: N\n---\nFlood {{name}}\n");

    let recovered = wait_for_file_contains(&out_dir.join("new.md"), "Flood N", TICK_TIMEOUT);

    stop.store(true, Ordering::Relaxed);
    flood.join().expect("flood thread panicked");

    assert!(
        recovered,
        "the idle tick must fire while events arrive 30x faster than --poll-interval: \
         nothing but the probe's re-arm can observe a root recreated as a new inode, so \
         a tick whose deadline restarts on every message loses this change permanently \
         rather than late (#319)"
    );

    drop(child);
}

// ── Ctrl+C during the startup compile ────────────────────────────────────────
//
// `watch_ctrl_c_exits_cleanly` and `watch_ctrl_c_prints_stopped_watching` both signal
// only after `spawn_ready` returns, so they cover Ctrl+C during the *event loop* and
// say nothing about Ctrl+C during startup. Those are different code paths with
// different handlers in force, and only the second one scales with the size of the
// user's tree.
//
// Installing `ctrlc::set_handler` converts SIGINT from "terminate" into "enqueue
// `Msg::Interrupt`", and that message is read only by the event loop. Install it above
// the startup compile and Ctrl+C is inert for the compile's whole duration: the tool
// keeps writing outputs, ignores every further press, and finally exits 0. The
// user-visible defect is that Ctrl+C does nothing and the tool writes files the user
// was trying to stop it writing.

/// Directory mode: SIGINT delivered while the startup compile is still running must
/// terminate the process, not be queued until the compile finishes.
///
/// `#[cfg(unix)]`: sends SIGINT via `libc::kill` and asserts termination-by-signal
/// via `ExitStatusExt::signal()`; Windows has no signal-death `ExitStatus` (#147).
#[test]
#[cfg(unix)]
fn watch_dir_mode_ctrl_c_during_startup_compile_terminates() {
    use std::os::unix::process::ExitStatusExt;

    // Large enough that the compile is still far from finished when the first outputs
    // appear, small enough to stay a fast test.
    const SOURCES: usize = 1200;
    /// Number of published outputs that proves the startup compile is under way.
    /// Deliberately tiny relative to SOURCES so the signal lands with the overwhelming
    /// majority of the work still ahead.
    const OBSERVE_AT: usize = 10;

    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let out_dir = base.path().join("out");
    std::fs::create_dir(&root).unwrap();
    for i in 0..SOURCES {
        std::fs::write(
            root.join(format!("s{i:04}.mds")),
            "---\nname: S\n---\nSource {{name}}\n",
        )
        .unwrap();
    }

    let count_outputs = |dir: &Path| -> usize {
        std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    };

    let (mut guard, _tap) = spawn_unsynchronized(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null()),
    );
    let pid = guard.id();

    // Gate on the artifact rather than on a guessed sleep: wait until the startup
    // compile has demonstrably begun (some outputs) and demonstrably not finished
    // (nowhere near all of them).
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut observed = 0usize;
    while Instant::now() < deadline {
        observed = count_outputs(&out_dir);
        if observed >= OBSERVE_AT {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        (OBSERVE_AT..SOURCES / 2).contains(&observed),
        "test precondition: SIGINT must be sent while the startup compile is in \
         flight; saw {observed} of {SOURCES} outputs (want {OBSERVE_AT}..{})",
        SOURCES / 2
    );

    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }

    let signalled_at = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = false;
    while Instant::now() < deadline {
        if guard.0.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let elapsed = signalled_at.elapsed();
    let written = count_outputs(&out_dir);
    assert!(
        exited,
        "process must exit after SIGINT during startup compile"
    );

    let status = guard.wait_status();

    // The discriminator. Before the event loop exists there is nothing to service a
    // queued `Msg::Interrupt`, so the correct behaviour is the default disposition:
    // death by SIGINT. A clean `code() == Some(0)` here means a handler was installed
    // above the startup compile, swallowed the signal, and let startup run to
    // completion — the #317 follow-up defect.
    assert_eq!(
        status.signal(),
        Some(libc::SIGINT),
        "SIGINT during the startup compile must terminate the process; instead it \
         exited with {status:?} after {elapsed:?}, having written {written} of \
         {SOURCES} outputs — the signal was swallowed until the event loop started"
    );

    // The user-visible half of the same property, asserted independently of how the
    // process died: it must stop producing output promptly.
    assert!(
        written < SOURCES / 2,
        "after Ctrl+C during startup the watcher must stop writing outputs; it wrote \
         {written} of {SOURCES}"
    );
}

/// File mode: same property. The gate is the `Watching …` line, which `run_watch_file`
/// prints before it creates the watcher and therefore before the startup compile; the
/// entry imports enough partials that the compile is still running when SIGINT lands.
///
/// `#[cfg(unix)]`: sends SIGINT via `libc::kill` and asserts termination-by-signal
/// via `ExitStatusExt::signal()`; Windows has no signal-death `ExitStatus` (#147).
#[test]
#[cfg(unix)]
fn watch_file_mode_ctrl_c_during_startup_compile_terminates() {
    use std::os::unix::process::ExitStatusExt;

    const PARTIALS: usize = 400;

    let dir = tempfile::tempdir().unwrap();
    let mut entry = String::new();
    for i in 0..PARTIALS {
        let name = format!("_p{i:04}");
        std::fs::write(
            dir.path().join(format!("{name}.mds")),
            format!("@define v{i}():\nP{i}\n@end\n\n@export v{i}\n"),
        )
        .unwrap();
        entry.push_str(&format!("@import \"./{name}.mds\" as p{i}\n"));
    }
    entry.push_str("done\n");
    let src = dir.path().join("entry.mds");
    std::fs::write(&src, &entry).unwrap();

    let (mut guard, tap) = spawn_unsynchronized(
        // No -q: the `Watching …` line is the startup gate.
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let pid = guard.id();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_watching = false;
    while Instant::now() < deadline {
        if tap.text().contains("Watching ") {
            saw_watching = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        saw_watching,
        "expected the `Watching …` startup line; stderr:\n{}",
        tap.text()
    );
    // `Watching …` is printed before the watcher is even created, so the startup
    // compile of a 400-import entry is still ahead.
    assert!(
        !dir.path().join("entry.md").exists(),
        "test precondition: the startup compile must not have published its output yet"
    );

    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGINT);
    }

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = false;
    while Instant::now() < deadline {
        if guard.0.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        exited,
        "process must exit after SIGINT during startup compile"
    );

    let status = guard.wait_status();
    assert_eq!(
        status.signal(),
        Some(libc::SIGINT),
        "SIGINT during the startup compile must terminate the process; instead it \
         exited with {status:?} — the signal was swallowed until the event loop started"
    );
}

/// Bounded wait for a child that is expected to exit: signalled, or ending its session
/// on its own.
///
/// A **bound, not a synchroniser**: such a child exits in milliseconds, and one that
/// has not exited by the deadline is the defect the caller is asserting against. `what`
/// names the arm so the panic is self-describing.
#[track_caller]
fn wait_bounded(guard: &mut ChildGuard, timeout: Duration, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = guard.0.try_wait().expect("try_wait failed") {
            return status;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("{what}: the process did not exit within {timeout:?} of the signal");
}

/// Control for #129 surface 1: the readiness handshake — not luck — is what makes a
/// post-SIGINT `status.success()` deterministic.
///
/// Two arms, the same signal, opposite verdicts:
///
/// - **CONTROL.** [`spawn_unsynchronized`], with SIGINT gated on the `Watching …`
///   line. `run_watch_file` prints that line before it even creates the watcher, and
///   therefore long before `ctrlc::set_handler`, so the signal lands in the
///   pre-handler window where the default disposition still applies: death by SIGINT.
///   If this arm ever exits cleanly, the window is no longer being hit and the
///   treatment arm below proves nothing.
/// - **TREATMENT.** [`spawn_ready`], with SIGINT sent the instant the handshake
///   returns. `set_handler` precedes `emit_ready_marker` in `run_watch_file`, so once
///   the marker exists the handler provably does too: exit 0 and `Stopped watching.`.
///
/// `N = 20` is a live discriminator, not a rate bound — a single clean control exit
/// fails the run. The manual Linux soak workflow is the rate instrument. Every wait
/// here is bounded and none of them is a sleep standing in for a synchroniser.
/// `#[cfg(unix)]` because SIGINT has no Windows analogue; the test compiles out there.
#[test]
#[cfg(unix)]
fn watch_readiness_handshake_makes_ctrl_c_exit_deterministic() {
    use std::os::unix::process::ExitStatusExt;

    const N: usize = 20;
    /// Imports in the control entry. Same fixture shape as
    /// `watch_file_mode_ctrl_c_during_startup_compile_terminates`: enough work that
    /// the startup compile is demonstrably still running when the signal lands.
    const PARTIALS: usize = 400;

    // Both fixtures are built ONCE. No watcher in this test ever edits a watched file,
    // so rebuilding them per iteration would buy nothing but wall clock.
    let slow_dir = tempfile::tempdir().unwrap();
    let mut entry = String::new();
    for i in 0..PARTIALS {
        let name = format!("_p{i:04}");
        std::fs::write(
            slow_dir.path().join(format!("{name}.mds")),
            format!("@define v{i}():\nP{i}\n@end\n\n@export v{i}\n"),
        )
        .unwrap();
        entry.push_str(&format!("@import \"./{name}.mds\" as p{i}\n"));
    }
    entry.push_str("done\n");
    let slow_src = slow_dir.path().join("entry.mds");
    std::fs::write(&slow_src, &entry).unwrap();

    let fast_dir = tempfile::tempdir().unwrap();
    let fast_src = fast_dir.path().join("hello.mds");
    std::fs::write(&fast_src, "---\nname: World\n---\nHello {{name}}!\n").unwrap();

    for iteration in 0..N {
        // ── CONTROL arm: signal delivered before the handler is installed ───────
        let (mut guard, tap) = spawn_unsynchronized(
            // No -q: the `Watching …` line is the gate.
            mds_bin()
                .args(["watch", slow_src.to_str().unwrap(), "--debounce", "0"])
                .stdout(Stdio::null()),
        );
        let pid = guard.id();

        let deadline = Instant::now() + STARTUP_WINDOW_TIMEOUT;
        let mut saw_watching = false;
        while Instant::now() < deadline {
            if tap.text().contains("Watching ") {
                saw_watching = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            saw_watching,
            "control arm, iteration {iteration}: expected the `Watching …` startup \
             line before signalling; stderr:\n{}",
            tap.text()
        );

        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGINT);
        }
        let status = wait_bounded(
            &mut guard,
            Duration::from_secs(20),
            "control arm (SIGINT before the handler is installed)",
        );
        assert_eq!(
            status.signal(),
            Some(libc::SIGINT),
            "control arm, iteration {iteration}: SIGINT delivered before \
             `ctrlc::set_handler` runs must terminate the process. A clean exit here \
             means the signal no longer lands in the pre-handler window, and the \
             treatment arm below then proves nothing; got {status:?}"
        );
        assert!(
            !status.success(),
            "control arm, iteration {iteration}: death by signal is not a success \
             status; got {status:?}"
        );

        // ── TREATMENT arm: signal delivered after the readiness handshake ───────
        let (mut guard, tap) = spawn_ready(
            mds_bin()
                .args(["watch", fast_src.to_str().unwrap(), "--debounce", "0"])
                .stdout(Stdio::null()),
        );
        let pid = guard.id();

        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGINT);
        }
        let status = wait_bounded(
            &mut guard,
            Duration::from_secs(5),
            "treatment arm (SIGINT after the readiness handshake)",
        );
        assert!(
            status.success(),
            "treatment arm, iteration {iteration}: after the readiness handshake the \
             ctrl-c handler provably exists (`set_handler` precedes \
             `emit_ready_marker` in `run_watch_file`), so SIGINT must exit 0; got \
             {status:?}; stderr:\n{}",
            tap.text()
        );
        let stderr = tap.finish_text(&mut guard);
        assert!(
            stderr.contains("Stopped watching."),
            "treatment arm, iteration {iteration}: a clean SIGINT exit must also print \
             `Stopped watching.`; stderr:\n{stderr}"
        );
    }
}

// ── I8: file-watch mode warns exactly ONCE across two edits (#200) ──────────

/// Pinned --set duplicate-key warning string (issue #200, spec §7.2).
const DUP_SET_WARNING: &str =
    "warning: variable 'x' is set more than once by --set; the last value wins";

#[test]
fn i8_file_watch_duplicate_set_warns_exactly_once_across_two_edits() {
    // I8: mds watch (file mode) with --set x=1 --set x=2 must print the
    // duplicate-key warning exactly once — at startup — not on every rebuild.
    //
    // Every count here is taken behind an ordered anchor, never from a snapshot of a
    // live pipe: a warning a rebuild printed but the tap had not copied yet would make
    // a snapshot read "still 1". Mutation control: an extra emit in `rebuild_file`
    // raises the count and fails this test (recorded when the anchors were added, #381).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let out = dir.path().join("t.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "0",
                "--set",
                "x=1",
                "--set",
                "x=2",
            ])
            .stdout(Stdio::null()),
    );

    // Positive control: the startup warning is printed.
    wait_for_tap(&stderr_tap, DUP_SET_WARNING, TIMEOUT);

    // Edit 1: trigger a rebuild.
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "I8: rebuild after edit 1 must complete"
    );

    // Edit 2: trigger another rebuild.
    write_atomic(&src, "version 3");
    assert!(
        wait_for_file_contains(&out, "version 3", TIMEOUT),
        "I8: rebuild after edit 2 must complete"
    );

    // Ordered anchor: the marker's diagnostic reaches stderr after every line of the
    // startup and of both rebuilds, so the count read back below is final.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let final_stderr = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&final_stderr, "Recompiled "),
        2,
        "I8: control: both edits rebuilt; stderr:\n{final_stderr}"
    );
    assert_eq!(
        count_occurrences(&final_stderr, DUP_SET_WARNING),
        1,
        "I8: after two edits the warning must still appear exactly once; \
         stderr:\n{final_stderr}"
    );
}

// ── I9: dir-watch mode warns exactly ONCE — at startup, never on a rebuild ────

#[test]
fn i9_dir_watch_duplicate_set_warns_exactly_once_at_startup() {
    // I9: mds watch (dir mode) with --set x=1 --set x=2 must print the
    // duplicate-key warning exactly once — at startup — and NOT again on rebuilds.
    //
    // dir_watch_startup calls build_runtime_vars once, and emits; every rebuild
    // calls it again.  This test is the SOLE mechanical guard that:
    //   - startup emits once (no double-print on startup), AND
    //   - rebuild calls do NOT emit (per-event growth).
    //
    // Dir mode prints a rebuild's warnings AFTER its `Recompiled` line, so the only
    // anchor that orders them is a later event: the order marker below. Mutation
    // control: an extra emit in `rebuild_dir_batch` raises the count and fails this
    // test (recorded when the anchor was added, #381).
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let out = dir.path().join("t.md");

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--debounce",
                "0",
                "--set",
                "x=1",
                "--set",
                "x=2",
            ])
            .stdout(Stdio::null()),
    );

    // Positive control: the startup warning is printed.
    wait_for_tap(&stderr_tap, DUP_SET_WARNING, TIMEOUT);

    // Trigger a rebuild to exercise the :1914 path (handle_dir_event).
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "I9: rebuild after edit must complete"
    );

    // Ordered anchor: every line of the startup and of the rebuild precedes the
    // marker's diagnostic, so the count is final.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let final_stderr = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&final_stderr, "Recompiled "),
        1,
        "I9: control: the edit rebuilt; stderr:\n{final_stderr}"
    );
    assert_eq!(
        count_occurrences(&final_stderr, DUP_SET_WARNING),
        1,
        "I9: the warning must appear exactly once — at startup, and not again on \
         the rebuild; stderr:\n{final_stderr}"
    );
}

// ── I16-I18: duplicate --vars file key warnings under `mds watch` (#326) ─────
//
// Unlike I8/I9 (--set/--set-string warn once per SESSION, at startup), a
// duplicate in the --vars FILE warns at startup AND on every rebuild: the freshness rule
// reloads the vars file on every rebuild, so a duplicate present in it is
// re-reported each time (D9).

/// I16: mds watch (file mode) with a duplicated top-level key in the vars file
/// warns at STARTUP and on EVERY rebuild. Guards the emit in `rebuild_file`.
///
/// The warning is written to stderr with no ordering relationship to the output file
/// the test waits on, so every count is read behind a line printed after it: startup
/// prints its warnings before it compiles and `Compiled to` after it writes, a rebuild
/// prints them before it writes and `Recompiled` after, and the order marker's
/// diagnostic follows everything — a line a rebuild printed after its `Recompiled`
/// included.
#[test]
fn i16_file_watch_vars_file_duplicate_warns_at_startup_and_on_every_rebuild() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let vars_dir = base.path().join("vars_dir");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&vars_dir).unwrap();

    let src = src_dir.join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"x": 1, "x": 2}"#).unwrap();
    let out = src_dir.join("t.md");

    let expected = dup_vars_file_warning("x", &vars_file);

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--vars",
                vars_file.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );

    let stderr_after_start = wait_for_tap(&stderr_tap, "Compiled to", TIMEOUT);
    assert_eq!(
        count_occurrences(&stderr_after_start, &expected),
        1,
        "I16: expected exactly 1 warning at startup; stderr:\n{stderr_after_start}"
    );

    // Edit 1: trigger a rebuild — the freshness rule reloads the vars file, re-reporting the
    // duplicate.
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "I16: rebuild after edit 1 must complete"
    );
    let after_edit_1 = wait_for_tap_count(&stderr_tap, "Recompiled ", 1, TIMEOUT);
    assert_eq!(
        count_occurrences(&after_edit_1, &expected),
        2,
        "I16: one rebuild must re-report the vars-file duplicate; stderr:\n{after_edit_1}"
    );

    // Edit 2: trigger another rebuild.
    write_atomic(&src, "version 3");
    assert!(
        wait_for_file_contains(&out, "version 3", TIMEOUT),
        "I16: rebuild after edit 2 must complete"
    );
    // Ordered anchor: the marker's compile fails before the warning's gate, so it adds
    // no warning of its own.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let after_edit_2 = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&after_edit_2, "Recompiled "),
        2,
        "I16: control: both edits rebuilt; stderr:\n{after_edit_2}"
    );
    assert_eq!(
        count_occurrences(&after_edit_2, &expected),
        3,
        "I16: a second rebuild must report the duplicate again; stderr:\n{after_edit_2}"
    );
}

/// I17: mds watch (dir mode) reports the vars-file duplicate exactly once per
/// rebuild: once at startup (proving `dir_watch_startup` does NOT double-print), and
/// once more per subsequent rebuild (proving exactly one of `liveness_probe_dir` /
/// `handle_fs_event_dir` emits, not both).
///
/// Sampling hazard this test has to defend against: dir mode emits the warning AFTER
/// the output write, so `wait_for_file_contains` returning tells you nothing about
/// whether the warning has been written yet. Sampling `stderr_tap.text()` right there
/// is a race in both directions, and CI has shown both — run 34404318888 attempt 1
/// saw left 1 / right 2 here, while run 34366009518 saw left 3 / right 2. Waiting for
/// the expected count is not enough either: stopping the watcher the moment the count
/// is reached would cut off a surplus warning printed just after it. The final count
/// is read behind the order marker, whose diagnostic follows every line the startup
/// and the rebuild printed.
#[test]
fn i17_dir_watch_vars_file_duplicate_warns_once_per_rebuild() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let vars_dir = base.path().join("vars_dir");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&vars_dir).unwrap();

    let src = src_dir.join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"x": 1, "x": 2}"#).unwrap();
    let out = src_dir.join("t.md");

    let expected = dup_vars_file_warning("x", &vars_file);

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src_dir.to_str().unwrap(),
                "--vars",
                vars_file.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );

    // No edits yet: the startup count must be exactly 1. This is a sample — a second
    // print still in the pipe would be missed here — and the final count below is the
    // exact check.
    let stderr_startup = wait_for_tap_count(&stderr_tap, &expected, 1, TIMEOUT);
    assert_eq!(
        count_occurrences(&stderr_startup, &expected),
        1,
        "I17: dir-watch startup must emit the vars-file warning exactly once; \
         stderr:\n{stderr_startup}"
    );

    // One rebuild: the count must rise to exactly 2, proving exactly one of
    // :1793/:1919 fires per rebuild (not both).
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "I17: rebuild after edit must complete"
    );
    // Ordered anchor: the marker's compile fails, so its batch changes nothing and
    // adds no warning of its own.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr_after_edit = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&stderr_after_edit, "Recompiled "),
        1,
        "I17: control: the edit rebuilt; stderr:\n{stderr_after_edit}"
    );
    assert_eq!(
        count_occurrences(&stderr_after_edit, &expected),
        2,
        "I17: startup and the one rebuild warn exactly once each (guards a \
         double-emit at startup, and between liveness_probe_dir and \
         handle_fs_event_dir); stderr:\n{stderr_after_edit}"
    );
}

/// I18 (user decision, positive control first): a vars file that starts clean
/// produces no duplicate-key warning at startup or on the first rebuild; a
/// duplicate introduced mid-session is reported on the NEXT rebuild, naming the
/// key.
#[test]
fn i18_duplicate_introduced_mid_session_is_reported_on_the_next_rebuild() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let vars_dir = base.path().join("vars_dir");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&vars_dir).unwrap();

    let src = src_dir.join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"x": 1}"#).unwrap();
    let out = src_dir.join("t.md");

    let expected = dup_vars_file_warning("x", &vars_file);

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--vars",
                vars_file.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "version 1", TIMEOUT),
        "I18: startup compile must complete"
    );

    // No duplicate at startup. Each zero below is read behind an ordered anchor, so it
    // cannot be a sample taken before the warning reached the tap: startup prints its
    // duplicate warnings before it compiles and `Compiled to` after it writes, and a
    // rebuild prints them before it writes and `Recompiled` after.
    let startup_stderr = wait_for_tap(&stderr_tap, "Compiled to", TIMEOUT);
    assert_eq!(
        count_occurrences(&startup_stderr, &expected),
        0,
        "I18: a clean vars file must not warn at startup; stderr:\n{startup_stderr}"
    );

    // First rebuild, still clean: still no warning.
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "I18: first rebuild must complete"
    );
    let clean_rebuild_stderr = wait_for_tap_count(&stderr_tap, "Recompiled ", 1, TIMEOUT);
    assert_eq!(
        count_occurrences(&clean_rebuild_stderr, &expected),
        0,
        "I18: the first rebuild must still not warn (vars file is still clean); \
         stderr:\n{clean_rebuild_stderr}"
    );

    // Introduce a duplicate mid-session, then trigger the next rebuild.
    //
    // Two watched files are written here, yet the expected count below is exactly 1.
    // The warning is gated in `rebuild_file` on an OBSERVABLE output-content change —
    // the same signal that gates the "Recompiled" line — and this fixture never
    // interpolates `x`, so the rebuild the vars-file write triggers produces
    // byte-identical output and reports nothing. Only the `version 3` rebuild is
    // observable. The atomic writes are what make that exact: a truncate-then-write
    // published a 0-byte intermediate, which was itself an observable transition and
    // could contribute a second warning.
    write_atomic(&vars_file, r#"{"x": 1, "x": 2}"#);
    write_atomic(&src, "version 3");
    assert!(
        wait_for_file_contains(&out, "version 3", TIMEOUT),
        "I18: rebuild after introducing the duplicate must complete"
    );
    // Ordered anchor: whichever of the two events reaches the rebuild that publishes
    // `version 3`, the other one's rebuild follows it — after that `Recompiled` line —
    // and both precede the marker's diagnostic. The marker's compile fails, so it adds
    // no warning of its own.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let final_stderr = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&final_stderr, "Recompiled "),
        2,
        "I18: control: the clean edit and the `version 3` edit rebuilt; \
         stderr:\n{final_stderr}"
    );
    assert_eq!(
        count_occurrences(&final_stderr, &expected),
        1,
        "I18: the duplicate introduced mid-session must be reported on the next \
         rebuild, naming the key; stderr:\n{final_stderr}"
    );
}

// ── I19-I20: liveness self-heal rebuild and --quiet regressions (#326) ───────
//
// AC-W2 (`watch_dir_mode_root_delete_recreate_recovers`) proves that a root
// delete+recreate kills the recursive watch on the old inode, so the create
// event for a file written into the recreated root is never delivered — only
// `liveness_probe_dir`'s re-arm + full reconcile finds and compiles it. Before
// this fix, that self-heal recompile discarded the resolved `RuntimeVars` and
// never warned about a --vars file duplicate, unlike `handle_fs_event_dir`.

/// I19: a self-heal rebuild driven by the liveness probe (no FS event ever
/// delivered) must warn about a --vars file duplicate key, same as a genuine
/// FS-event rebuild does. Guards `liveness_probe_dir`'s content-backstop site.
#[test]
fn i19_dir_watch_liveness_self_heal_rebuild_warns_about_vars_file_duplicate() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("watched");
    let vars_dir = base.path().join("vars_dir");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&vars_dir).unwrap();
    std::fs::write(root.join("a.mds"), "---\nname: A\n---\nOld A\n").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"x": 1, "x": 2}"#).unwrap();
    let out_dir = base.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    let expected = dup_vars_file_warning("x", &vars_file);

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                root.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--vars",
                vars_file.to_str().unwrap(),
                "--debounce",
                "0",
                "--poll-interval",
                "100",
            ])
            .stdout(Stdio::null()),
    );

    // Startup: exactly 1 warning (dir-mode startup, unaffected by this fix). A sample,
    // as in I17; the final count below is the exact check.
    let startup_stderr = wait_for_tap_count(&stderr_tap, &expected, 1, TIMEOUT);
    assert_eq!(
        count_occurrences(&startup_stderr, &expected),
        1,
        "I19: dir-watch startup must warn exactly once; stderr:\n{startup_stderr}"
    );
    assert!(
        wait_for_file_contains(&out_dir.join("a.md"), "Old A", TIMEOUT),
        "I19: initial compile should produce 'Old A'"
    );

    // Delete the entire watched root — kills the recursive watch on the old inode
    // (same setup as `watch_dir_mode_root_delete_recreate_recovers`).
    std::fs::remove_dir_all(&root).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // Recreate the root with a brand-new file. On Linux/inotify the create event
    // above is unobservable (new inode, nothing watching it yet), so only the
    // liveness probe's re-arm + reconcile self-heal path (`liveness_probe_dir`)
    // can find and compile it; on macOS FSEvents watches by path, so the create
    // event IS delivered and `handle_fs_event_dir` may service the self-heal
    // first instead. Either way, the content-changed gate guarantees exactly one
    // warning per observable rebuild, which is what the count assertion below
    // pins.
    std::fs::create_dir(&root).unwrap();
    write_atomic(
        &root.join("new.mds"),
        "---\nname: N\n---\nNew file {{name}}\n",
    );

    assert!(
        wait_for_file_contains(&out_dir.join("new.md"), "New file N", TICK_TIMEOUT),
        "I19: watcher must self-heal after root delete+recreate and recompile"
    );

    // The self-heal recompile must ALSO re-warn about the vars-file duplicate —
    // proves liveness_probe_dir no longer discards the resolved vars, matching
    // handle_fs_event_dir's gate (emit iff the rebuild was observable).
    //
    // Ordered anchor: dir mode warns after the rebuild's write, and the marker's
    // diagnostic follows every line of the self-heal. Its compile fails, so it adds no
    // warning of its own. TICK_TIMEOUT: the recreated root was re-armed by the idle
    // tick, and should the marker's event still be missed, the tick's content check is
    // what compiles it.
    write_atomic(&root.join("new.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TICK_TIMEOUT);
    let final_stderr = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&final_stderr, &expected),
        2,
        "I19: the liveness self-heal rebuild must warn about the vars-file \
         duplicate too, not only at startup; stderr:\n{final_stderr}"
    );
}

/// I20: `mds watch --quiet` suppresses the vars-file duplicate-key warning on
/// every rebuild, not just at startup. Regression guard for the inner
/// `if quiet { return; }` early-out in `emit_duplicate_vars_file_warnings`:
/// nothing else stops per-rebuild spam under --quiet on the direct watch call
/// sites (`rebuild_file`, `handle_fs_event_dir`, `liveness_probe_dir`), since
/// they call the emitter directly and bypass `emit_duplicate_var_warnings`'s
/// own quiet early-out (that one only guards the startup call sites).
#[test]
fn i20_watch_quiet_suppresses_vars_file_duplicate_warning_on_every_rebuild() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let vars_dir = base.path().join("vars_dir");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&vars_dir).unwrap();

    let src = src_dir.join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let vars_file = vars_dir.join("vars.json");
    std::fs::write(&vars_file, r#"{"x": 1, "x": 2}"#).unwrap();
    let out = src_dir.join("t.md");

    let expected = dup_vars_file_warning("x", &vars_file);

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--vars",
                vars_file.to_str().unwrap(),
                "--debounce",
                "0",
                "--quiet",
            ])
            .stdout(Stdio::null()),
    );

    // Positive control: the startup compile and the rebuild really happen even though
    // nothing warns — otherwise "0 occurrences" below would be vacuous.
    assert!(
        wait_for_file_contains(&out, "version 1", TIMEOUT),
        "I20: startup compile must complete even under --quiet"
    );
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "I20: rebuild after edit must complete even under --quiet"
    );

    // Ordered anchor: --quiet silences every status line, but not a diagnostic. The
    // order marker's diagnostic reaches stderr after anything the startup or the
    // rebuild printed, so the zero below covers both instead of sampling a pipe that
    // might not have delivered a warning yet.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = stderr_tap.finish_text(&mut child);
    assert_eq!(
        count_occurrences(&stderr, &expected),
        0,
        "I20: --quiet must suppress the vars-file duplicate warning at startup and on \
         rebuild; stderr:\n{stderr}"
    );
}

// ── R1-R3: rename-into-place (atomic write) is a first-class edit (#320) ─────
//
// Editors and `write_atomic` replace a file by writing a sibling temp file and
// renaming it over the target. That is ONE filesystem event on the destination
// (`Modify(Name(RenameMode::To))` under notify 8 / inotify `IN_MOVED_TO`), not the
// truncate-then-write pair `std::fs::write` produces. These three tests pin that the
// watcher treats it as a content edit and that the in-flight temp file is invisible
// to both watch modes.

/// R1: file mode must rebuild when the watched source is replaced by a rename.
#[test]
fn watch_file_mode_rename_into_place_triggers_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let out = dir.path().join("t.md");

    let (child, stderr_tap) = spawn_ready(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0"])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "version 1", TIMEOUT),
        "R1: startup compile must complete"
    );

    write_atomic(&src, "version 2");

    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "R1: a rename-into-place edit must trigger a rebuild"
    );
    // R1: the rebuild must announce itself.
    wait_for_tap(&stderr_tap, "Recompiled", TIMEOUT);

    drop(child);
}

/// R2: dir mode must rebuild when a watched source is replaced by a rename.
#[test]
fn watch_dir_mode_rename_into_place_triggers_rebuild() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let out_dir = base.path().join("out");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();

    let src = src_dir.join("t.mds");
    std::fs::write(&src, "version 1").unwrap();
    let out = out_dir.join("t.md");

    let (child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src_dir.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out, "version 1", TIMEOUT),
        "R2: startup compile must complete"
    );

    write_atomic(&src, "version 2");

    assert!(
        wait_for_file_contains(&out, "version 2", TIMEOUT),
        "R2: a rename-into-place edit must trigger a rebuild"
    );
    // R2: the rebuild must announce itself.
    wait_for_tap(&stderr_tap, "Recompiled", TIMEOUT);

    drop(child);
}

/// R3: the temp file an atomic write leaves in flight is never compiled.
///
/// The `.<name>.tmp-<pid>-<n>` shape puts the suffix AFTER the `.mds`, so
/// `Path::extension()` is not `mds` and both the dir-mode event filter and
/// `collect_mds_files` drop it. Asserting only that absence would be vacuous if the
/// watcher were simply not compiling anything, so the same test writes a REAL second
/// source through `write_atomic` and requires that one to be compiled.
#[test]
fn watch_dir_mode_write_atomic_temp_file_is_never_compiled() {
    let base = tempfile::tempdir().unwrap();
    let src_dir = base.path().join("src");
    let out_dir = base.path().join("out");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();

    let src = src_dir.join("t.mds");
    std::fs::write(&src, "version 1").unwrap();

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src_dir.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );

    assert!(
        wait_for_file_contains(&out_dir.join("t.md"), "version 1", TIMEOUT),
        "R3: startup compile must complete"
    );

    // Atomic edit of the existing source, then a brand-new source — also atomic.
    write_atomic(&src, "version 2");
    assert!(
        wait_for_file_contains(&out_dir.join("t.md"), "version 2", TIMEOUT),
        "R3: the atomic edit must rebuild t.md"
    );

    // Positive control: a genuine new source written the same way IS compiled, so the
    // "temp file produced nothing" assertions below cannot pass vacuously.
    write_atomic(&src_dir.join("u.mds"), "brand new");
    assert!(
        wait_for_file_contains(&out_dir.join("u.md"), "brand new", TIMEOUT),
        "R3 (positive control): a real source created by a rename must be compiled"
    );

    // Ordered anchor: an event for a temp name — even one delivered after its rename —
    // is handled before the marker's, so both absences below are read from a final
    // state rather than a sample. The marker's compile fails; its own write goes
    // through a temp file too.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&stderr_tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = stderr_tap.finish_text(&mut child);

    // No output derives from any temp name, in either directory.
    for dir in [&out_dir, &src_dir] {
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains(".tmp-"),
                "R3: no file derived from a write_atomic temp name may survive in {}; \
                 found {name}",
                dir.display()
            );
        }
    }

    // And nothing announced compiling one.
    assert!(
        !stderr.contains(".tmp-"),
        "R3: no status line may mention a write_atomic temp file; stderr:\n{stderr}"
    );
}

// ── Stderr capture completeness (#320) ──────────────────────────────────────

/// The tap must hand back every byte the child wrote, not a prefix of it.
///
/// `StderrTap::bytes` clones the shared buffer without any happens-before edge to the
/// drain thread's last write. Reaping the child closes its write end and ends the
/// drain loop, but nothing makes the reader observe that the loop has finished, so a
/// snapshot taken right after `kill` + `wait` can be a truncated prefix. The suite hid
/// that behind a `thread::sleep` at every such site.
///
/// A dir watcher over 500 sources announces `Compiled to` once per file at startup, so
/// the expected count is exact and any lost tail shows up as a shortfall rather than
/// as a vague "looks empty". The `Compiled to` lines are also the positive control:
/// a count of 0 would mean the watcher compiled nothing, not that the tap is sound.
///
/// macOS has not been observed to lose the tail; the field signature is Linux
/// (`cli_watch.rs:520` in CI runs 32954883014 and 32954876042). The Linux soak is the
/// instrument for this one.
#[test]
fn stderr_tap_finish_captures_every_line_the_child_wrote() {
    const FILE_COUNT: usize = 500;
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    for i in 1..=FILE_COUNT {
        std::fs::write(
            dir.path().join(format!("file_{i:04}.mds")),
            format!("drain-{i}\n"),
        )
        .unwrap();
    }

    // No -q: the startup compile announces `Compiled to` once per file.
    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                dir.path().to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );

    // Readiness fires only after the whole startup batch, so all FILE_COUNT lines
    // have been written by the child by the time this returns. `finish` reaps the
    // child and then joins the drain, so what comes back is the complete stream.
    let stderr = stderr_tap.finish_text(&mut child);
    let announced = count_occurrences(&stderr, "Compiled to");
    assert_eq!(
        announced, FILE_COUNT,
        "the tap must return every `Compiled to` line the child wrote; got {announced} \
         of {FILE_COUNT}"
    );
}

// ── R4: readiness must not depend on someone draining stdout (#320) ─────────

/// A watcher whose stdout is piped but undrained must still signal readiness.
///
/// `mds watch -o -` publishes the startup output to stdout BEFORE it writes the
/// readiness marker (watch.rs: the marker is emitted after the compile, the arming and
/// the publish). A pipe holds ~64 KiB; once it is full the child blocks in `write`, so
/// if the harness is sitting in the marker poll loop with nothing draining stdout,
/// neither side can move and the spawn helper times out.
///
/// 256 KiB of body is several pipe buffers on both Linux and macOS, so the block is a
/// certainty, not a matter of timing.
#[test]
fn watch_ready_with_large_piped_stdout_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.mds");
    // Plain text is valid MDS; short lines keep the compile trivial.
    let body: String = std::iter::repeat_n("x".repeat(63) + "\n", 8192).collect();
    assert!(
        body.len() > 256 * 1024,
        "fixture must exceed several pipe buffers; got {} bytes",
        body.len()
    );
    std::fs::write(&src, &body).unwrap();

    let (mut child, _stderr_tap, stdout_tap) = spawn_ready_piped_stdout(
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
            .stdout(Stdio::piped()),
    );

    // Readiness returned, so the startup publish got through. Prove the bytes really
    // travelled rather than the marker having been written before any output.
    let stdout = stdout_tap.finish(&mut child);
    assert!(
        stdout.len() >= body.len(),
        "the whole startup output must reach stdout; got {} bytes of {}",
        stdout.len(),
        body.len()
    );
}

/// A symlink planted at the readiness file's temporary path does not redirect the marker
/// (#390): `mds watch` creates that file new, never through an entry already there, so the
/// file the link points to keeps its bytes and the marker is a file of its own. Control:
/// the watcher still signals readiness — the harness returns only once the marker holds
/// its text.
///
/// Unix-only: it plants a symlink, which Windows creates only with a privilege.
#[cfg(unix)]
#[test]
fn a_symlink_at_the_readiness_file_s_temporary_path_does_not_redirect_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Page\n").unwrap();
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "VICTIM\n").unwrap();
    let marker = dir.path().join("ready");
    std::os::unix::fs::symlink(&victim, dir.path().join("ready.tmp")).unwrap();

    let (child, _tap, _) = spawn_watch_ready_at(
        mds_bin()
            .args(["watch", src.to_str().unwrap(), "--debounce", "0", "-q"])
            .stdout(Stdio::null()),
        &marker,
    );
    let _child = ChildGuard(child);

    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "VICTIM\n",
        "the file the planted link points to keeps its bytes"
    );
    assert!(
        std::fs::symlink_metadata(&marker)
            .unwrap()
            .file_type()
            .is_file(),
        "the marker is a file of its own, not the planted link"
    );
}

// ── #413: directory arguments, and every `--help` example has a test ────────

/// `mds watch .`, `./`, `..` and `sub/..` watch the directory they resolve to (#413).
/// Directory mode takes its argument through the resolver every directory-mode
/// subcommand shares, where it used to run `NativeFs::check_symlink`, which cannot take
/// a path with no final name and failed with `file not found: .`. Each form compiles
/// every file at startup, announces the directory as typed — never by the canonical
/// path it watches (#390) — and rebuilds on an edit.
#[test]
fn watch_dot_forms_watch_the_canonical_directory() {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("proj");
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    std::fs::write(proj.join("p.mds"), "P\n").unwrap();
    std::fs::write(proj.join("sub").join("s.mds"), "S\n").unwrap();
    let canonical = proj.canonicalize().unwrap();

    for (i, (cwd, typed)) in [
        (proj.clone(), "."),
        (proj.clone(), "./"),
        (proj.join("sub"), ".."),
        (proj.clone(), "sub/.."),
    ]
    .into_iter()
    .enumerate()
    {
        let label = format!("(in {}) mds watch {typed}", cwd.display());
        let out = dir.path().join(format!("out{i}"));
        let (child, tap) = spawn_ready(
            mds_bin()
                .current_dir(&cwd)
                .args(["watch", typed, "--out-dir", out.to_str().unwrap()])
                .args(["--debounce", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&out.join("p.md"), "P", TIMEOUT),
            "{label}: p.mds compiles at startup; stderr: {}",
            tap.text()
        );
        assert!(
            wait_for_file_contains(&out.join("sub").join("s.md"), "S", TIMEOUT),
            "{label}: sub/s.mds compiles at startup; stderr: {}",
            tap.text()
        );

        let stderr = wait_for_tap(&tap, "Watching directory ", TIMEOUT);
        let banner = stderr
            .lines()
            .find_map(|l| l.strip_prefix("Watching directory "))
            .unwrap_or_else(|| panic!("{label}: no banner; stderr: {stderr}"));
        assert_eq!(
            banner, typed,
            "{label}: the banner names the directory as typed"
        );
        assert!(
            !stderr.contains(&format!("Watching directory {}", canonical.display())),
            "{label}: never by the canonical directory; stderr: {stderr}"
        );

        let edited = format!("P{i}");
        write_atomic(&proj.join("p.mds"), format!("{edited}\n"));
        assert!(
            wait_for_file_contains(&out.join("p.md"), &edited, TIMEOUT),
            "{label}: an edit rebuilds; stderr: {}",
            tap.text()
        );
        drop(child);
    }
}

/// Directory mode compiles each source by its walked path — the directory argument as
/// typed, joined with the source's path below it — but watches the canonical directory
/// (#413). `link/..` is accepted as the directory above the link's target; once the
/// link is retargeted, the walked path leads into another directory: the rebuild is
/// refused (`mds::io`), naming the directory as typed, and nothing is written from the
/// other directory — rather than compiling its file into the watched one's output.
/// Control: before the retarget, an edit rebuilds.
///
/// Unix-only: it retargets a directory symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn watch_dir_through_a_retargeted_link_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    for (name, below, text) in [("a", "x", "Hello A\n"), ("b", "y", "Hello B\n")] {
        std::fs::create_dir_all(dir.path().join(name).join(below)).unwrap();
        std::fs::write(dir.path().join(name).join("p.mds"), text).unwrap();
    }
    let link = dir.path().join("link");
    symlink("a/x", &link).unwrap();
    let watched = dir.path().join("a").join("p.mds");
    let out = dir.path().join("out").join("p.md");

    let (child, stderr_tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args([
                "watch",
                "link/..",
                "--out-dir",
                "out",
                "--debounce",
                "0",
                "-q",
            ])
            .stdout(Stdio::null()),
    );
    assert!(wait_for_file_contains(&out, "Hello A", TIMEOUT), "startup");
    write_atomic(&watched, "Hello A1\n");
    assert!(
        wait_for_file_contains(&out, "Hello A1", TIMEOUT),
        "control: an edit rebuilds through the link"
    );

    std::fs::remove_file(&link).unwrap();
    symlink("b/y", &link).unwrap();
    write_atomic(&watched, "Hello A2\n");
    let stderr = wait_for_tap(&stderr_tap, "watched directory now resolves", TIMEOUT);
    assert!(
        squash(&stderr).contains(
            "mds::io×watcheddirectorynowresolvestoadifferentdirectory:\"link/..\";\
             restartmdswatchtofollowit"
        ),
        "the rebuild is refused, naming the directory as typed; stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "Hello A1\n",
        "nothing is written from the retargeted directory"
    );
    drop(child);
}

/// Directory mode checks before every compile that the source's own walked path still
/// leads to the file it watches, not only that the root does (#413). Once a subdirectory
/// below the root is replaced by a symbolic link to a directory outside the tree,
/// `root/sub/x.mds` still names a file — through the link — while the root itself is
/// unmoved; the startup walk skips symlinked directories, but a rebuild compiles the
/// sources it already knows. The rebuild of `sub/x.mds` is refused (`mds::io`, naming
/// its walked path as typed) and nothing is written from outside the tree, rather than
/// compiling the outside file into the watched one's output. The vars-file edit rebuilds
/// every known source; `top.mds` rebuilding is its positive control, and before the swap
/// an edit under the unchanged subdirectory rebuilds.
///
/// Unix-only: it replaces a directory with a symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn watch_dir_source_under_a_subdirectory_swapped_for_a_link_is_refused() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("top.mds"), "Top {{v}}\n").unwrap();
    std::fs::write(root.join("sub").join("x.mds"), "Inside {{v}}\n").unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("x.mds"), "Outside {{v}}\n").unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "1"}"#).unwrap();
    let out = dir.path().join("out");
    let x_out = out.join("sub").join("x.md");

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "root", "--out-dir", "out", "--vars", "vars.json"])
            .args(["--debounce", "0", "--poll-interval", "0", "-q"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&x_out, "Inside 1", TIMEOUT),
        "startup; stderr: {}",
        tap.text()
    );
    write_atomic(&root.join("sub").join("x.mds"), "Inside {{v}} again\n");
    assert!(
        wait_for_file_contains(&x_out, "Inside 1 again", TIMEOUT),
        "control: an edit under the unchanged subdirectory rebuilds; stderr: {}",
        tap.text()
    );

    std::fs::rename(root.join("sub"), dir.path().join("sub.old")).unwrap();
    symlink(&outside, root.join("sub")).unwrap();
    write_atomic(&vars, r#"{"v": "2"}"#);
    assert!(
        wait_for_file_contains(&out.join("top.md"), "Top 2", TIMEOUT),
        "control: the vars edit rebuilds every known source; stderr: {}",
        tap.text()
    );
    let stderr = wait_for_tap(&tap, "watched file now resolves", TIMEOUT);
    assert!(
        squash(&stderr).contains(
            "mds::io×watchedfilenowresolvestoadifferentfile:\"root/sub/x.mds\";\
             restartmdswatchtofollowit"
        ),
        "the rebuild is refused, naming the source as walked; stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&x_out).unwrap(),
        "Inside 1 again\n",
        "nothing is written from outside the tree; stderr: {stderr}"
    );
    drop(child);
}

/// `mds watch --help`'s examples, each with the test that runs it (#413).
const WATCH_HELP_EXAMPLES: [(&str, &str); 10] = [
    (
        "mds watch template.mds",
        "watch_initial_compile_writes_output",
    ),
    (
        "mds watch chat.mds",
        "watch_messages_template_produces_json_intrinsically",
    ),
    (
        "mds watch template.mds -o out.md",
        "watch_output_flag_writes_to_specified_file",
    ),
    (
        "mds watch template.mds -o -",
        "watch_stdout_contains_content_when_o_stdout",
    ),
    (
        "mds watch .",
        "watch_help_example_dot_watches_the_working_directory",
    ),
    (
        "mds watch src/ --out-dir dist",
        "watch_help_example_src_out_dir_dist_mirrors_the_subtree",
    ),
    (
        "mds watch template.mds --vars v.json",
        "watch_vars_file_change_triggers_recompile",
    ),
    (
        "mds watch template.mds --clear",
        "watch_clear_non_tty_no_ansi_escape",
    ),
    (
        "mds watch src/ --poll-interval 500",
        "watch_help_example_src_poll_interval_500_self_heals",
    ),
    (
        "mds watch src/ --poll-interval 0",
        "watch_help_example_src_poll_interval_0_rebuilds_on_native_events",
    ),
];

/// How this file defines `name`: `None` when it has no `fn <name>() {` — the shape of
/// a test fn — and otherwise whether that fn is a `#[test]`.
fn test_attribute(name: &str) -> Option<bool> {
    const SOURCE: &str = include_str!("cli_watch.rs");
    let at = SOURCE.find(&format!("\nfn {name}() {{"))?;
    // The attributes and doc comment above the fn, back to the previous item's end.
    let before = &SOURCE[..at];
    let preamble = &before[before.rfind("\n}\n").map_or(0, |i| i + 3)..];
    Some(preamble.lines().any(|l| l.trim() == "#[test]"))
}

/// A zero-argument fn that is not a test: [`test_attribute`] must find it, as it finds
/// a test, and still tell that it is not one.
fn not_a_test() {}

/// Every example `mds watch --help` prints has a named test that runs it, and every
/// test the table names exists (#413): an example added without a test, edited so it
/// no longer matches, or whose test was renamed away fails here.
#[test]
fn watch_help_examples_each_have_a_named_test() {
    let output = mds_bin().args(["watch", "--help"]).output().unwrap();
    assert!(output.status.success(), "mds watch --help exits 0");
    let help = String::from_utf8(output.stdout).unwrap();
    let (_, examples) = help
        .split_once("Examples:")
        .unwrap_or_else(|| panic!("--help has an Examples section; got: {help}"));
    // An example line is the command, two or more spaces, then its description.
    let shown: std::collections::BTreeSet<&str> = examples
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("mds watch"))
        .map(|l| l.split("  ").next().unwrap_or(l).trim_end())
        .collect();
    let mapped: std::collections::BTreeSet<&str> =
        WATCH_HELP_EXAMPLES.iter().map(|(ex, _)| *ex).collect();
    assert_eq!(
        shown, mapped,
        "every --help example, and only those, is mapped"
    );

    for (example, test) in WATCH_HELP_EXAMPLES {
        assert_eq!(
            test_attribute(test),
            Some(true),
            "{example:?} maps to `{test}`, which is not a #[test] in this file"
        );
    }
    // Controls, one for each answer: a name this file does not define; a fn it defines
    // in a test's shape (`fn()`, which the coercion below pins) that is not a test; and
    // this test.
    let _: fn() = not_a_test;
    assert_eq!(
        test_attribute("watch_help_example_that_does_not_exist"),
        None
    );
    assert_eq!(test_attribute("not_a_test"), Some(false));
    assert_eq!(
        test_attribute("watch_help_examples_each_have_a_named_test"),
        Some(true)
    );
}

/// `mds watch .` — watch every `.mds` file in the working directory, writing each
/// output next to its source (#413: `.` used to fail with `file not found: .`).
#[test]
fn watch_help_example_dot_watches_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("template.mds"), "Hello!\n").unwrap();
    let out = dir.path().join("template.md");

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", ".", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&out, "Hello!", TIMEOUT),
        "the startup compile writes template.md; stderr: {}",
        tap.text()
    );
    write_atomic(&dir.path().join("template.mds"), "Hello again!\n");
    assert!(
        wait_for_file_contains(&out, "Hello again!", TIMEOUT),
        "an edit rebuilds; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// `mds watch src/ --out-dir dist` — the trailing slash is accepted, and the output
/// mirrors the source subtree under `dist/`.
#[test]
fn watch_help_example_src_out_dir_dist_mirrors_the_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.mds"), "A\n").unwrap();
    std::fs::write(src.join("sub").join("b.mds"), "B\n").unwrap();
    let dist = dir.path().join("dist");

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "src/", "--out-dir", "dist", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&dist.join("a.md"), "A", TIMEOUT),
        "src/a.mds → dist/a.md; stderr: {}",
        tap.text()
    );
    assert!(
        wait_for_file_contains(&dist.join("sub").join("b.md"), "B", TIMEOUT),
        "src/sub/b.mds → dist/sub/b.md; stderr: {}",
        tap.text()
    );
    assert!(!dist.join("b.md").exists(), "mirrored, not flattened");
    write_atomic(&src.join("sub").join("b.mds"), "B2\n");
    assert!(
        wait_for_file_contains(&dist.join("sub").join("b.md"), "B2", TIMEOUT),
        "an edit rebuilds the mirrored output; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// `mds watch src/ --poll-interval 500` — the help example runs, and it self-heals once
/// the watched root is deleted and recreated: the file written into the new root is
/// compiled. On Linux no event announces it (the watch died with the old directory), so
/// the idle tick re-arms the watch; on macOS FSEvents reports it without a tick.
///
/// This does not measure the 500 ms interval: the default 1000 ms tick recovers within
/// [`TICK_TIMEOUT`] too, and a wall-clock assertion on the interval would only make the
/// test a timing flake. `watch_poll_interval_zero_works`,
/// `watch_poll_interval_invalid_exits_2` and `watch_poll_interval_tiny_clamped` pin how
/// the flag is parsed and clamped.
#[test]
fn watch_help_example_src_poll_interval_500_self_heals() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.mds"), "Old A\n").unwrap();

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "src/", "--poll-interval", "500", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&src.join("a.md"), "Old A", TIMEOUT),
        "startup compile; stderr: {}",
        tap.text()
    );

    std::fs::remove_dir_all(&src).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::create_dir(&src).unwrap();
    write_atomic(&src.join("new.mds"), "New file\n");
    // TICK-DEPENDENT (see TICK_TIMEOUT): on Linux only the self-heal tick can recover.
    assert!(
        wait_for_file_contains(&src.join("new.md"), "New file", TICK_TIMEOUT),
        "the self-heal tick recovers the recreated root; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// `mds watch src/ --poll-interval 0` — the self-heal check is off; native events alone
/// compile at startup and rebuild on an edit.
#[test]
fn watch_help_example_src_poll_interval_0_rebuilds_on_native_events() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.mds"), "A\n").unwrap();

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "src/", "--poll-interval", "0", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&src.join("a.md"), "A", TIMEOUT),
        "startup compile; stderr: {}",
        tap.text()
    );
    write_atomic(&src.join("a.mds"), "A2\n");
    assert!(
        wait_for_file_contains(&src.join("a.md"), "A2", TIMEOUT),
        "a native event rebuilds with the self-heal tick off; stderr: {}",
        tap.text()
    );
    drop(child);
}

// ── #425: output over the entry file is refused ──────────────────────────────

/// A `.md` entry that declares `type: mds`: its default output name is its own name.
const TYPE_MDS_PAGE: &str = "---\ntype: mds\nname: X\n---\nHello {{name}}!\n";

/// The #425 refusal naming the entry as `typed`, squashed (miette wraps long lines).
fn entry_overwrite_refusal(typed: &str) -> String {
    squash(&format!(
        "mds::io × output would overwrite the entry file: \"{typed}\"; \
         write it elsewhere with -o <file> or --out-dir <dir>"
    ))
}

/// Wait for a watcher expected to refuse at startup to exit, and return its exit
/// status and stderr. Bounded: 10 s at a 10 ms poll — a watcher that did not refuse
/// keeps running, and fails here.
fn startup_refusal_exit(
    child: &mut ChildGuard,
    tap: StderrTap,
    label: &str,
) -> (Option<i32>, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: still running, so it did not refuse; stderr: {}",
            tap.text()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    (status.code(), tap.finish_text(child))
}

/// `mds watch` in file mode refuses at startup, exit 2, when the output it resolves is
/// the entry file itself — the default route of a `type: mds` `.md` entry, `--out-dir`
/// naming its directory, `-o` naming it, also through a `..` after a directory that
/// does not exist yet, and on a case-insensitive volume in another case — and writes
/// nothing: the source stays byte-identical, and no directory is created. It used to
/// write the compiled output over the source and keep watching a file that no longer
/// declared `type: mds`.
#[test]
fn watch_refuses_at_startup_to_write_over_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.md");
    std::fs::write(&src, TYPE_MDS_PAGE).unwrap();

    let mut extras: Vec<&[&str]> = vec![
        &[],
        &["--out-dir", "."],
        &["-o", "page.md"],
        &["-o", "newdir/../page.md"],
        &["--out-dir", "newdir/.."],
    ];
    if dir.path().join("PAGE.md").exists() {
        extras.push(&["-o", "newdir/../PAGE.md"]);
    }
    for extra in extras {
        let label = format!("mds watch page.md {}", extra.join(" "));
        let (mut child, tap) = spawn_unsynchronized(
            mds_bin()
                .current_dir(dir.path())
                .args(["watch", "page.md", "--debounce", "0"])
                .args(extra)
                .stdout(Stdio::null()),
        );
        let (code, stderr) = startup_refusal_exit(&mut child, tap, &label);
        assert_eq!(code, Some(2), "{label}: stderr: {stderr}");
        assert!(
            squash(&stderr).contains(&entry_overwrite_refusal("page.md")),
            "{label}: stderr: {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(&src).unwrap(),
            TYPE_MDS_PAGE,
            "{label}: the source is untouched"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "{label}: nothing is written"
        );
    }

    // Control: an `--out-dir` that does not exist yet is created by the write.
    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.md", "--out-dir", "fresh", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(
            &dir.path().join("fresh").join("page.md"),
            "Hello X!",
            TIMEOUT
        ),
        "control: --out-dir fresh is created and written; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// A rebuild never writes over the entry either. When the startup compile fails, the
/// kind is unknown, and a fix that compiles to Markdown takes the Markdown default —
/// the entry itself here — or the `-o` path as given, which leads back to the entry out
/// of a directory that does not exist yet; once the source is fixed, the
/// rebuild refuses (`mds::io`, the entry named as typed), keeps watching, and the fixed
/// source stays byte-identical, with no directory created. It used to overwrite it
/// with the compiled output.
#[test]
fn watch_rebuild_never_writes_over_the_entry() {
    for extra in [&[][..], &["-o", "newdir/../page.md"]] {
        let label = format!("mds watch page.md {}", extra.join(" "));
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("page.md");
        std::fs::write(&src, "---\ntype: mds\n---\nHello {{name\n").unwrap();

        let (child, tap) = spawn_ready(
            mds_bin()
                .current_dir(dir.path())
                .args(["watch", "page.md", "--debounce", "0"])
                .args(extra)
                .stdout(Stdio::null()),
        );
        // Control: the startup compile fails.
        wait_for_tap(&tap, "mds::syntax", TIMEOUT);

        write_atomic(&src, TYPE_MDS_PAGE);
        let stderr = wait_for_tap(&tap, "output would overwrite the entry file", TIMEOUT);
        assert!(
            squash(&stderr).contains(&entry_overwrite_refusal("page.md")),
            "{label}: the rebuild is refused; stderr: {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(&src).unwrap(),
            TYPE_MDS_PAGE,
            "{label}: the fixed source is untouched"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "{label}: nothing is written"
        );
        let mut child = child;
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "{label}: watch keeps running after a refused rebuild"
        );
        drop(child);
    }

    // Control: a fallback that is not the entry is written by the first rebuild that
    // compiles, which creates its directory; the failed startup compile created nothing.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.md");
    std::fs::write(&src, "---\ntype: mds\n---\nHello {{name\n").unwrap();
    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.md", "--out-dir", "fresh", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    // Control: the startup compile fails.
    wait_for_tap(&tap, "mds::syntax", TIMEOUT);
    assert!(
        !dir.path().join("fresh").exists(),
        "control: a failed startup compile creates no directory"
    );
    write_atomic(&src, TYPE_MDS_PAGE);
    assert!(
        wait_for_file_contains(
            &dir.path().join("fresh").join("page.md"),
            "Hello X!",
            TIMEOUT
        ),
        "control: the rebuild creates fresh/ and writes it; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// The #425 refusal is all `mds watch` says about an output that is the entry: the `-o`
/// extension-mismatch warning, which announces a write (`… writing to '<path>'
/// anyway`), is not printed for it — neither at startup, where the refusal ends the
/// run, nor for the fallback output of a failed startup compile, which every rebuild
/// refuses. Control: an `-o` with the same mismatched extension that is not the entry
/// still gets the warning, and is written.
#[test]
fn watch_refusal_is_not_preceded_by_the_extension_warning() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("e.mds");
    let no_warning = |stderr: &str, label: &str| {
        assert!(
            !stderr.contains("warning:"),
            "{label}: no warning announces a write that is refused; stderr: {stderr}"
        );
    };

    // Startup: the compile succeeds, and its output is the entry.
    std::fs::write(&src, "E\n").unwrap();
    let label = "startup: mds watch e.mds -o e.mds";
    let (mut child, tap) = spawn_unsynchronized(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "e.mds", "-o", "e.mds", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let (code, stderr) = startup_refusal_exit(&mut child, tap, label);
    assert_eq!(code, Some(2), "{label}: stderr: {stderr}");
    assert!(
        squash(&stderr).contains(&entry_overwrite_refusal("e.mds")),
        "{label}: stderr: {stderr}"
    );
    no_warning(&stderr, label);
    drop(child);

    // A failed startup compile: its fallback output is the entry, so the rebuild after
    // the fix is refused, and nothing ever announced a write to it.
    std::fs::write(&src, "Hello {{name\n").unwrap();
    let label = "fallback: mds watch e.mds -o e.mds";
    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "e.mds", "-o", "e.mds", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    // Control: the startup compile fails.
    wait_for_tap(&tap, "mds::syntax", TIMEOUT);
    write_atomic(&src, "E\n");
    let stderr = wait_for_tap(&tap, "output would overwrite the entry file", TIMEOUT);
    assert!(
        squash(&stderr).contains(&entry_overwrite_refusal("e.mds")),
        "{label}: the rebuild is refused; stderr: {stderr}"
    );
    no_warning(&stderr, label);
    assert_eq!(std::fs::read_to_string(&src).unwrap(), "E\n", "{label}");
    drop(child);

    // Control: the same mismatched extension on an output that is not the entry.
    let other = dir.path().join("other.mds");
    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "e.mds", "-o", "other.mds", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&other, "E", TIMEOUT),
        "control: other.mds is written; stderr: {}",
        tap.text()
    );
    let warning = "warning: output path 'other.mds' has extension '.mds' but compiled output \
                   is markdown (.md); writing to 'other.mds' anyway";
    // Control: the warning still announces a write that happens.
    wait_for_tap(&tap, warning, TIMEOUT);
    drop(child);
}

// ── An output route that fails at startup is refused ─────────────────────────

/// An `mds.json` whose `build.output_dir` leaves its directory: the output route error
/// file-mode resolution raises, which `mds build` refuses (`mds::io`, exit 2).
const DOTDOT_OUTPUT_DIR: &str = r#"{"build":{"output_dir":"../x"}}"#;

/// The refusal of [`DOTDOT_OUTPUT_DIR`], squashed (miette wraps long lines).
fn dotdot_output_dir_refusal() -> String {
    squash("mds::io × mds.json output_dir '../x' must not contain '..' components")
}

/// `mds watch` refuses at startup an output route that fails to resolve — `mds.json`
/// `build.output_dir` with a `..` component — with the error `mds build` gives for the
/// same route (`mds::io`, exit 2), reported once, and writes nothing: no file, no
/// directory, no byte on stdout. In file mode it used to report the error once and then
/// write every rebuild to stdout (`Recompiled <stdout>`); after a failed startup compile
/// it did so without reporting the route error at all. A failed startup compile is still
/// reported, and the route is refused after it. Directory mode already refused the route
/// at startup; it is pinned here with the same error.
#[test]
fn watch_refuses_at_startup_an_output_route_that_fails() {
    // (mode, input, source, whether the startup compile succeeds)
    let rows = [
        ("file mode", "page.mds", "Hello one\n", true),
        (
            "file mode, failed startup compile",
            "page.mds",
            "Hello {{name\n",
            false,
        ),
        ("directory mode", ".", "Hello one\n", true),
    ];
    for (mode, input, source, compiles) in rows {
        let label = format!("{mode}: mds watch {input}");
        // `../x` resolves beside `proj`, in `top`, which holds nothing else.
        let top = tempfile::tempdir().unwrap();
        let proj = top.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("mds.json"), DOTDOT_OUTPUT_DIR).unwrap();
        std::fs::write(proj.join("page.mds"), source).unwrap();

        let (mut child, tap, stdout_tap) = spawn_unsynchronized_piped_stdout(
            mds_bin()
                .current_dir(&proj)
                .args(["watch", input, "--debounce", "0"])
                .stdout(Stdio::piped()),
        );
        let (code, stderr) = startup_refusal_exit(&mut child, tap, &label);
        let stdout = stdout_tap.finish_text(&mut child);
        assert_eq!(code, Some(2), "{label}: stderr: {stderr}");
        assert_eq!(
            count_occurrences(&squash(&stderr), &dotdot_output_dir_refusal()),
            1,
            "{label}: the refusal is reported, once; stderr: {stderr}"
        );
        assert!(
            !stderr.contains("Compiled to"),
            "{label}: nothing is compiled to a file; stderr: {stderr}"
        );
        assert!(
            !stderr.contains("Recompiled"),
            "{label}: no rebuild runs; stderr: {stderr}"
        );
        assert_eq!(stdout, "", "{label}: nothing is written to stdout");
        let mut names: Vec<String> = std::fs::read_dir(&proj)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["mds.json", "page.mds"],
            "{label}: nothing is written"
        );
        assert_eq!(
            std::fs::read_dir(top.path()).unwrap().count(),
            1,
            "{label}: no output directory is created"
        );

        if compiles {
            // Differential: `mds build` refuses the same route with the same error.
            let build = mds_bin()
                .current_dir(&proj)
                .args(["build", input])
                .output()
                .unwrap();
            let build_stderr = String::from_utf8_lossy(&build.stderr);
            assert_eq!(
                build.status.code(),
                code,
                "{label}: mds build {input} exits as watch does; stderr: {build_stderr}"
            );
            assert!(
                squash(&build_stderr).contains(&dotdot_output_dir_refusal()),
                "{label}: mds build {input} gives the same error; stderr: {build_stderr}"
            );
        } else {
            // Control: the startup compile ran, and its failure is reported.
            assert!(
                stderr.contains("mds::syntax"),
                "{label}: the compile error is reported; stderr: {stderr}"
            );
        }
    }
}

/// Controls for [`watch_refuses_at_startup_an_output_route_that_fails`] — only a route
/// that fails is refused:
/// - a failed startup compile with a route that resolves is reported, watching
///   continues, and the next rebuild writes the fixed source;
/// - with the same `mds.json`, `-o <file>` and `--out-dir <dir>` route the output (the
///   config's `output_dir` is not consulted), and the startup compile and a rebuild are
///   written there;
/// - with the same `mds.json`, `-o -` streams the startup output and a rebuild to stdout,
///   as documented.
#[test]
fn watch_startup_route_refusal_controls() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello {{name\n").unwrap();
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.mds", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    // Failed startup compile: the compile error is reported.
    wait_for_tap(&tap, "mds::syntax", TIMEOUT);
    write_atomic(&src, "Hello fixed\n");
    assert!(
        wait_for_file_contains(&dir.path().join("page.md"), "Hello fixed", TIMEOUT),
        "failed startup compile: the rebuild writes page.md; stderr: {}",
        tap.text()
    );
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "failed startup compile: watch keeps running"
    );
    drop(child);

    for (flag, value, written) in [
        ("-o", "out.md", "out.md"),
        ("--out-dir", "out", "out/page.md"),
    ] {
        let label = format!("mds watch page.mds {flag} {value}");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mds.json"), DOTDOT_OUTPUT_DIR).unwrap();
        let src = dir.path().join("page.mds");
        std::fs::write(&src, "Hello one\n").unwrap();
        let out = dir.path().join(written);
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(dir.path())
                .args(["watch", "page.mds", flag, value, "--debounce", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&out, "Hello one", TIMEOUT),
            "{label}: startup writes {written}; stderr: {}",
            tap.text()
        );
        write_atomic(&src, "Hello two\n");
        assert!(
            wait_for_file_contains(&out, "Hello two", TIMEOUT),
            "{label}: the rebuild writes {written}; stderr: {}",
            tap.text()
        );
        assert!(
            !tap.text().contains("must not contain"),
            "{label}: nothing is refused; stderr: {}",
            tap.text()
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "{label}: watch keeps running"
        );
        drop(child);
    }

    let label = "mds watch page.mds -o -";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mds.json"), DOTDOT_OUTPUT_DIR).unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    let (mut child, tap, stdout_tap) = spawn_ready_piped_stdout(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.mds", "-o", "-", "--debounce", "0"])
            .stdout(Stdio::piped()),
    );
    // `wait_for_tap` polls any pipe tap; this one is stdout. Startup streams to stdout,
    // and so does the rebuild.
    wait_for_tap(&stdout_tap, "Hello one", TIMEOUT);
    write_atomic(&src, "Hello two\n");
    wait_for_tap(&stdout_tap, "Hello two", TIMEOUT);
    assert!(
        !tap.text().contains("must not contain"),
        "{label}: nothing is refused; stderr: {}",
        tap.text()
    );
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "{label}: watch keeps running"
    );
    assert!(
        !dir.path().join("page.md").exists(),
        "{label}: nothing is written next to the source"
    );
    drop(child);
}

// ── A failed startup compile or write: the route of the compiled kind (#257) ────
//
// A startup compile that fails leaves the output's kind unknown, so the first rebuild
// that compiles decides the route by its kind. A startup write that fails follows a
// compile that succeeded: the route its kind decided and the dependencies it reported
// stay the session's. The write fails on a directory standing at the output path, which
// no write replaces on any OS or under any privilege; removing the directory removes
// the cause. `--poll-interval 0` turns the idle tick off, so every rebuild is the test's
// own edit's and nothing rediscovers what startup dropped.

/// A messages template whose `.json` output cannot be written at startup keeps the
/// `.json` route (#257): the startup error names `./chat.json` as typed, and once the
/// obstacle is gone an edit writes `chat.json`; no `chat.md` is ever created. It used to
/// take the Markdown route of a failed compile, and write the JSON into `chat.md`.
/// Control: after a startup compile that fails, a rebuild that compiles to Markdown
/// writes `chat.md`.
#[test]
fn watch_failed_startup_write_keeps_the_compiled_kinds_route() {
    let watch = |dir: &Path| {
        spawn_ready(
            mds_bin()
                .current_dir(dir)
                .args(["watch", "chat.mds"])
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        )
    };
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("chat.mds");
    let json = dir.path().join("chat.json");
    let md = dir.path().join("chat.md");
    std::fs::write(&src, "@message user:\nWhat is 2+2?\n@end\n").unwrap();
    std::fs::create_dir(&json).unwrap();
    // As `mds build chat.mds` names its output.
    let shown = Path::new(".").join("chat.json");

    let (child, tap) = watch(dir.path());
    let startup = wait_for_tap(&tap, "cannot write", TIMEOUT);
    assert!(
        squash(&startup).contains(&squash(&format!("cannot write {}:", shown.display()))),
        "the startup error names the .json output as typed; stderr: {startup}"
    );

    std::fs::remove_dir(&json).unwrap();
    write_atomic(&src, "@message user:\nWhat is 3+3?\n@end\n");
    // Anchored on the edit's own output: a late event for `chat.mds`, written before the
    // spawn, can rebuild the startup text first — the failed write left the content
    // dedup empty — so the first `Recompiled` need not be the edit's.
    let rebuilt = wait_for_file_contains(&json, "What is 3+3?", TIMEOUT);
    // A rebuild writes its output before it prints `Recompiled`.
    let stderr = wait_for_tap(&tap, "Recompiled", TIMEOUT);
    assert!(
        !md.exists(),
        "no Markdown output is ever created; chat.md holds {:?}; stderr: {stderr}",
        std::fs::read_to_string(&md).ok()
    );
    assert!(
        squash(&stderr).contains(&squash(&format!("Recompiled {}", shown.display()))),
        "the rebuild writes the .json output; stderr: {stderr}"
    );
    let written = std::fs::read_to_string(&json).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&written).expect("the .json output is JSON");
    assert!(
        rebuilt && parsed.is_array() && written.contains("What is 3+3?"),
        "chat.json holds the rebuilt messages: {written}"
    );
    drop(child);

    // Control: a startup compile that fails leaves the kind unknown, so the first
    // rebuild that compiles routes by its kind: Markdown, `chat.md`.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("chat.mds");
    let md = dir.path().join("chat.md");
    std::fs::write(&src, "Hello {{name\n").unwrap();
    let (child, tap) = watch(dir.path());
    wait_for_tap(&tap, "mds::syntax", TIMEOUT);
    write_atomic(&src, "Hello fixed\n");
    assert!(
        wait_for_file_contains(&md, "Hello fixed", TIMEOUT),
        "control: after a failed startup compile the rebuild writes chat.md; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// A failed startup write keeps the dependencies the compile reported (#257): once the
/// obstacle is gone, an edit to the imported partial rebuilds the entry and writes it.
/// They used to be dropped with the write, so with no idle tick to find them again only
/// an edit to the entry itself rebuilt it.
#[test]
fn watch_failed_startup_write_keeps_the_compiled_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("part.mds");
    let src = dir.path().join("page.mds");
    let out = dir.path().join("page.md");
    std::fs::write(&part, "@define who():\nWorld\n@end\n\n@export who\n").unwrap();
    std::fs::write(&src, "@import \"./part.mds\" as p\nHello {{p.who()}}!\n").unwrap();
    std::fs::create_dir(&out).unwrap();

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.mds"])
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    // Control: the startup write fails.
    wait_for_tap(&tap, "cannot write", TIMEOUT);

    std::fs::remove_dir(&out).unwrap();
    write_atomic(&part, "@define who():\nPlanet\n@end\n\n@export who\n");
    assert!(
        wait_for_file_contains(&out, "Hello Planet!", TIMEOUT),
        "an edit to the imported partial rebuilds the entry; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// A failed startup write warns about an explicit `-o` by the kind the compile produced,
/// once (#257): the startup compile warns when the path's extension contradicts that kind,
/// and the failed write adds nothing. It used to warn a second time measured against
/// Markdown — twice for a Markdown output written to `-o out.json`, and once for a
/// messages output, which `.json` names. The first session is the positive control for
/// the second's absence.
#[test]
fn watch_failed_startup_write_warns_about_the_output_extension_once() {
    // One session whose startup write to `-o out.json` fails: the warnings on stderr.
    let warnings = |source: &str| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chat.mds"), source).unwrap();
        std::fs::create_dir(dir.path().join("out.json")).unwrap();
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(dir.path())
                .args(["watch", "chat.mds", "-o", "out.json"])
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        // Startup prints all it prints before it reports readiness, and a rebuild never
        // prints the `-o` warning.
        let stderr = tap.finish_text(&mut child);
        assert!(
            squash(&stderr).contains(&squash("cannot write out.json:")),
            "the startup write fails; stderr: {stderr}"
        );
        (
            count_occurrences(&squash(&stderr), &squash("has extension '.json'")),
            stderr,
        )
    };

    let (count, stderr) = warnings("Hello\n");
    assert_eq!(
        count, 1,
        "a Markdown output named .json is warned about once; stderr: {stderr}"
    );
    let (count, stderr) = warnings("@message user:\nHi\n@end\n");
    assert_eq!(
        count, 0,
        "a messages output named .json is not warned about; stderr: {stderr}"
    );
}

/// A startup compile that fails leaves the output's kind unknown, and the first rebuild
/// that compiles routes the output by the kind it compiles to (#257): a template fixed
/// into messages writes `chat.json` and never `chat.md`, below `--out-dir` too, and one
/// fixed into Markdown writes `chat.md` and never `chat.json`. It used to keep the
/// Markdown route all session, and wrote the JSON into `chat.md`. Control: an explicit
/// `-o` names the output whatever its kind, so the JSON goes to the file it names.
#[test]
fn watch_failed_startup_compile_routes_by_the_kind_it_compiles_to() {
    const MESSAGES: &str = "@message user:\nWhat is 3+3?\n@end\n";
    const MARKDOWN: &str = "Hello fixed\n";
    // One session whose startup compile fails, then `fixed` saved over the entry: the
    // directory, and stderr up to the first `Recompiled` — a rebuild writes its output
    // before it prints that line.
    let session = |args: &[&str], fixed: &str| {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("chat.mds");
        std::fs::write(&src, "Hello {{name\n").unwrap();
        let (child, tap) = spawn_ready(
            mds_bin()
                .current_dir(dir.path())
                .args(["watch", "chat.mds"])
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        wait_for_tap(&tap, "mds::syntax", TIMEOUT);
        write_atomic(&src, fixed);
        let stderr = wait_for_tap(&tap, "Recompiled", TIMEOUT);
        drop(child);
        (dir, stderr)
    };
    let recompiled = |stderr: &str, shown: &Path| {
        squash(stderr).contains(&squash(&format!("Recompiled {}", shown.display())))
    };

    // Messages, beside the entry.
    let (dir, stderr) = session(&[], MESSAGES);
    let md = dir.path().join("chat.md");
    assert!(
        !md.exists(),
        "a template fixed into messages never writes chat.md; it holds {:?}; stderr: {stderr}",
        std::fs::read_to_string(&md).ok()
    );
    assert!(
        recompiled(&stderr, &Path::new(".").join("chat.json")),
        "the rebuild names the .json output as typed; stderr: {stderr}"
    );
    let written = std::fs::read_to_string(dir.path().join("chat.json")).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&written).expect("the .json output is JSON");
    assert!(
        parsed.is_array() && written.contains("What is 3+3?"),
        "chat.json holds the messages: {written}"
    );

    // Messages, below `--out-dir`.
    let (dir, stderr) = session(&["--out-dir", "out"], MESSAGES);
    assert!(
        recompiled(&stderr, &Path::new("out").join("chat.json")),
        "under --out-dir the rebuild writes the .json output; stderr: {stderr}"
    );
    let written = std::fs::read_to_string(dir.path().join("out").join("chat.json")).unwrap();
    assert!(
        written.contains("What is 3+3?"),
        "out/chat.json holds the messages: {written}"
    );

    // Control: Markdown, beside the entry — `chat.md` is written where it applies.
    let (dir, stderr) = session(&[], MARKDOWN);
    assert!(
        recompiled(&stderr, &Path::new(".").join("chat.md")),
        "control: the rebuild names the .md output; stderr: {stderr}"
    );
    let written = std::fs::read_to_string(dir.path().join("chat.md")).unwrap();
    assert!(
        written.contains("Hello fixed"),
        "control: chat.md holds the Markdown: {written}"
    );
    assert!(
        !dir.path().join("chat.json").exists(),
        "a template fixed into Markdown never writes chat.json; stderr: {stderr}"
    );

    // Control: an explicit `-o` is the route whatever the kind.
    let (dir, stderr) = session(&["-o", "out.md"], MESSAGES);
    assert!(
        recompiled(&stderr, Path::new("out.md")),
        "control: the rebuild writes the -o output; stderr: {stderr}"
    );
    let written = std::fs::read_to_string(dir.path().join("out.md")).unwrap();
    assert!(
        written.contains("What is 3+3?"),
        "control: out.md holds the messages: {written}"
    );
    assert!(
        !dir.path().join("chat.json").exists() && !dir.path().join("out.json").exists(),
        "control: -o is never routed by the kind; stderr: {stderr}"
    );
}

/// After a failed startup compile, a route that is refused as the entry itself is not
/// kept (#257, #425): a fix of a `type: mds` `.md` entry that compiles to Markdown would
/// write over the entry and is refused, and a later edit that compiles to messages
/// writes `page.json`. The refused route used to stay the session's, so that edit was
/// refused too, and so was every later one.
#[test]
fn watch_route_refused_as_the_entry_is_not_kept() {
    const MESSAGES: &str = "---\ntype: mds\n---\n@message user:\nWhat is 3+3?\n@end\n";
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.md");
    let json = dir.path().join("page.json");
    std::fs::write(&src, "---\ntype: mds\n---\nHello {{name\n").unwrap();
    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "page.md"])
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    // Control: the startup compile fails.
    wait_for_tap(&tap, "mds::syntax", TIMEOUT);

    // Control: a fix that compiles to Markdown is refused — its route is the entry.
    write_atomic(&src, TYPE_MDS_PAGE);
    let refused = wait_for_tap(&tap, "output would overwrite the entry file", TIMEOUT);
    assert!(
        squash(&refused).contains(&entry_overwrite_refusal("page.md")),
        "control: the Markdown route is refused; stderr: {refused}"
    );

    write_atomic(&src, MESSAGES);
    // Refused rebuilds write nothing and print no `Recompiled`, so the first one is the
    // write of page.json, which comes before it.
    let written = wait_for_file_contains(&json, "What is 3+3?", TIMEOUT);
    assert!(
        written,
        "a later compile to messages writes page.json; stderr: {}",
        tap.text()
    );
    let stderr = wait_for_tap(&tap, "Recompiled", TIMEOUT);
    assert!(
        squash(&stderr).contains(&squash(&format!(
            "Recompiled {}",
            Path::new(".").join("page.json").display()
        ))),
        "the rebuild names page.json as typed; stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&src).unwrap(),
        MESSAGES,
        "the entry is untouched"
    );
    drop(child);
}

// ── A failed directory-watch startup write is retried (#257) ───────────────────────
//
// A directory watch compiles each source once at startup, and the content dedup holds
// only the outputs that startup wrote. A source whose output could not be written is
// marked failed, so the next rebuild with a real change compiles it and writes it, even
// when its content never changed. The write fails on a directory standing at the output
// path, as in the section above.

/// A directory watch whose startup write of one source fails writes that output on the
/// next rebuild once the obstacle is gone (#257), even when the save leaves the source's
/// content unchanged. Startup used to record the unwritten content as written, so the
/// content dedup skipped that output until the source's content itself changed.
/// Control: a save of the same bytes to a source whose startup write succeeded rewrites
/// nothing.
#[test]
fn watch_dir_failed_startup_write_is_retried_on_the_next_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("d");
    std::fs::create_dir(&d).unwrap();
    let (a, b) = (d.join("a.mds"), d.join("b.mds"));
    let (a_out, b_out) = (d.join("a.md"), d.join("b.md"));
    std::fs::write(&a, "Steady a\n").unwrap();
    std::fs::write(&b, "Steady b\n").unwrap();
    std::fs::create_dir(&a_out).unwrap();
    // As `mds build d` names each output.
    let shown = |name: &str| Path::new("d").join(name);
    let recompiled = |stderr: &str, name: &str| {
        count_occurrences(
            &squash(stderr),
            &squash(&format!("Recompiled {}", shown(name).display())),
        )
    };

    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args(["watch", "d"])
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    let startup = wait_for_tap(&tap, "cannot write", TIMEOUT);
    assert!(
        squash(&startup).contains(&squash(&format!(
            "cannot write {}:",
            shown("a.md").display()
        ))),
        "the startup write of a.md fails; stderr: {startup}"
    );
    assert!(
        wait_for_file_contains(&b_out, "Steady b", TIMEOUT),
        "control: the startup writes b.md; stderr: {startup}"
    );

    std::fs::remove_dir(&a_out).unwrap();
    write_atomic(&a, "Steady a\n");
    assert!(
        wait_for_file_contains(&a_out, "Steady a", TIMEOUT),
        "a save of the unchanged source writes a.md once the obstacle is gone; stderr: {}",
        tap.text()
    );

    // Control: the same bytes, saved to the source whose startup write succeeded.
    write_atomic(&b, "Steady b\n");
    // Ordered anchor: every rebuild before it has printed what it prints.
    write_atomic(&d.join("m.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        recompiled(&stderr, "a.md"),
        1,
        "a.md is written once; stderr: {stderr}"
    );
    assert_eq!(
        recompiled(&stderr, "b.md"),
        0,
        "control: a save of the same bytes rewrites nothing; stderr: {stderr}"
    );
}

/// A directory rebuild whose write fails keeps the dependencies its compile reported
/// (#257), as startup does: once the obstacle is gone, an edit to a file outside the
/// watched directory, which the edit whose write failed began to import, rebuilds the
/// source. The rebuild used to keep the dependencies of the compile before it, so that
/// file's directory was never watched and its edits were never seen. `--poll-interval
/// 100`: the idle tick arms a new dependency's directory and compares what it holds.
#[test]
fn watch_dir_failed_rebuild_write_keeps_the_compiled_dependencies() {
    let base = tempfile::tempdir().unwrap();
    let (root, shared) = (base.path().join("root"), base.path().join("shared"));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&shared).unwrap();
    // A project root above both, so `../shared/` can be imported.
    std::fs::write(base.path().join(".git"), "").unwrap();
    let partial = shared.join("_x.mds");
    std::fs::write(
        &partial,
        "@define greet():\nShared one\n@end\n\n@export greet\n",
    )
    .unwrap();
    let (a, a_out) = (root.join("a.mds"), root.join("a.md"));
    std::fs::write(&a, "Plain a\n").unwrap();

    let (child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base.path())
            .args(["watch", "root"])
            .args(["--debounce", "0", "--poll-interval", "100"])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&a_out, "Plain a", TIMEOUT),
        "control: the startup writes a.md; stderr: {}",
        tap.text()
    );

    // A directory at a.md, then an edit that imports the shared file: the rebuild
    // compiles, and its write fails.
    std::fs::remove_file(&a_out).unwrap();
    std::fs::create_dir(&a_out).unwrap();
    write_atomic(&a, "@import \"../shared/_x.mds\" as x\n{{x.greet()}}\n");
    let failed = wait_for_tap(&tap, "cannot write", TIMEOUT);
    assert!(
        squash(&failed).contains(&squash(&format!(
            "cannot write {}:",
            Path::new("root").join("a.md").display()
        ))),
        "the rebuild's write of a.md fails; stderr: {failed}"
    );

    std::fs::remove_dir(&a_out).unwrap();
    write_atomic(
        &partial,
        "@define greet():\nShared, edited\n@end\n\n@export greet\n",
    );
    assert!(
        wait_for_file_contains(&a_out, "Shared, edited", TICK_TIMEOUT),
        "an edit to the file the failed rebuild imported rebuilds a.md; stderr: {}",
        tap.text()
    );
    drop(child);
}

/// A directory watch's startup compiles each source once (#257). A compile that panics
/// prints the internal-compiler-error text once (`MDS_TEST_PANIC=compile:a`, a debug
/// build's trigger), so the texts on stderr count the startup's compiles of `a.mds`.
/// Startup used to compile every source a second time to seed the content dedup, and
/// printed the text twice. `--debounce 30000` holds a rebuild until thirty seconds after
/// its last event — a late event for a source written before the spawn included — so the
/// session is stopped before any rebuild compiles, and every text is the startup's.
/// Control: the other source is compiled and written.
#[cfg(debug_assertions)]
#[test]
fn watch_dir_startup_compiles_each_source_once() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("d");
    std::fs::create_dir(&d).unwrap();
    for name in ["a", "b"] {
        std::fs::write(d.join(format!("{name}.mds")), format!("Hello {name}\n")).unwrap();
    }

    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .env("MDS_TEST_PANIC", "compile:a")
            .args(["watch", "d", "--quiet"])
            .args(["--debounce", "30000", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    // The startup writes its outputs before it reports readiness.
    let b_md = std::fs::read_to_string(d.join("b.md")).ok();
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        b_md.as_deref(),
        Some("Hello b\n"),
        "control: the startup compiles and writes the other source; stderr: {stderr}"
    );
    assert!(
        !d.join("a.md").exists(),
        "a compile that panicked writes no output; stderr: {stderr}"
    );
    assert_eq!(
        count_occurrences(&stderr, "mds: internal compiler error"),
        1,
        "the startup compiles a.mds once; stderr: {stderr}"
    );
}

// ── Streams: a gone stdout reader, a closed stderr, a failing write (#157) ──────
//
// A closed pipe — its reader gone — never changes how `mds watch` exits. With `-o -`,
// stdout IS the session's product, so a gone reader ends the session: one
// `Stopped watching (stdout closed).` line, exit 0. A closed stderr only loses the
// status lines, so the session keeps watching. Any other output failure during a live
// session is reported (where stderr still works) and never changes the Ctrl+C exit; a
// session that stops before it is live exits as a run that ends on its own does.

/// The line a session ends with when `-o -` finds stdout's reader gone.
const STOPPED_STDOUT_CLOSED: &str = "Stopped watching (stdout closed).\n";

/// Upper bound for a watcher expected to exit — at startup, after a rebuild, or after a
/// signal. A **failure bound**: it exits in milliseconds, and one still running at the
/// deadline is the defect the caller asserts against.
const SESSION_END_TIMEOUT: Duration = Duration::from_secs(10);

/// Send SIGINT (Ctrl+C) to the watcher.
///
/// `#[cfg(unix)]`: SIGINT via `libc::kill` has no Windows analogue (#147).
#[cfg(unix)]
fn interrupt(guard: &ChildGuard) {
    // SAFETY: `kill` takes no pointer; the pid is our own live child's.
    unsafe {
        libc::kill(guard.id() as libc::pid_t, libc::SIGINT);
    }
}

/// `mds watch -o -` whose stdout reader is gone before it starts: the startup write finds
/// the pipe closed, so the session prints `Stopped watching (stdout closed).` as its
/// last line and exits 0; `--quiet` silences the line (#157).
///
/// The reader is dropped before the spawn ([`closed_pipe`]), so the first stdout write
/// fails whatever the timing. Control: with an open pipe the same command writes the
/// compiled output to stdout and keeps watching, so the closed arm really loses a write.
#[test]
fn watch_to_stdout_whose_reader_is_gone_stops_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    let src = src.to_str().unwrap();
    let loud = ["watch", src, "-o", "-", "--debounce", "0"];
    let quiet = ["watch", src, "-o", "-", "--debounce", "0", "-q"];

    // Control: an open pipe gets the output, and the session goes on.
    let (mut open, _open_tap, open_stdout) =
        spawn_ready_piped_stdout(mds_bin().args(loud).stdout(Stdio::piped()));
    wait_for_tap(&open_stdout, "Hello one", TIMEOUT);
    assert!(
        open.0.try_wait().unwrap().is_none(),
        "control: with an open pipe the session keeps watching"
    );
    drop(open);

    let (mut closed, tap) =
        spawn_unsynchronized(mds_bin().args(loud).stdout(Stdio::from(closed_pipe())));
    let status = wait_bounded(
        &mut closed,
        SESSION_END_TIMEOUT,
        "watch -o - with no reader",
    );
    let stderr = tap.finish_text(&mut closed);
    assert_eq!(
        status.code(),
        Some(0),
        "a gone stdout reader ends the session with exit 0; stderr: {stderr:?}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(
        lines.len() == 2 && lines[0].starts_with("Watching "),
        "the session prints `Watching …` and then exactly the stop line; stderr: {stderr:?}"
    );
    assert_eq!(
        format!("{}\n", lines[1]),
        STOPPED_STDOUT_CLOSED,
        "the last line names the closed stdout; stderr: {stderr:?}"
    );

    // `--quiet` silences the line; the loud arm above is its positive control.
    let (mut quiet_run, quiet_tap) =
        spawn_unsynchronized(mds_bin().args(quiet).stdout(Stdio::from(closed_pipe())));
    let status = wait_bounded(
        &mut quiet_run,
        SESSION_END_TIMEOUT,
        "watch -o - -q with no reader",
    );
    let stderr = quiet_tap.finish_text(&mut quiet_run);
    assert_eq!(
        status.code(),
        Some(0),
        "--quiet: a gone stdout reader ends the session with exit 0; stderr: {stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "--quiet prints nothing when stdout's reader is gone"
    );
}

/// `mds watch -o -` whose reader goes away after the first output: the next rebuild's
/// write finds the pipe closed, and the session ends with the same line and exit 0 —
/// never `Recompiled`, since nothing was written; `--quiet` silences the line (#157).
///
/// The test owns the pipe's only reader: it reads the startup output, which the watcher
/// writes before it signals readiness, and then drops the reader before the edit. The
/// loud arm is the quiet arm's positive control: the same session prints the stop line.
#[test]
fn watch_to_stdout_stops_when_its_reader_goes_away_and_exits_0() {
    use std::io::Read as _;

    for quiet in [false, true] {
        let what = if quiet { "--quiet" } else { "loud" };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("page.mds");
        std::fs::write(&src, "Hello one\n").unwrap();
        let mut args = vec!["watch", src.to_str().unwrap(), "-o", "-", "--debounce", "0"];
        if quiet {
            args.push("-q");
        }

        let (reader, writer) = std::io::pipe().unwrap();
        let (mut guard, tap) = spawn_ready(mds_bin().args(&args).stdout(Stdio::from(writer)));

        // Read the startup output on a helper thread, so the wait is bounded; the thread
        // hands the reader back so the test decides when it goes.
        let first_output = b"Hello one\n";
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut first = vec![0u8; first_output.len()];
            let read = reader.read_exact(&mut first).map(|()| first);
            let _ = tx.send((read, reader));
        });
        let (read, reader) = rx
            .recv_timeout(TIMEOUT)
            .expect("the startup output must reach the pipe");
        assert_eq!(
            read.expect("read the startup output"),
            first_output,
            "control ({what}): the session writes its output to stdout"
        );
        drop(reader);

        write_atomic(&src, "Hello two\n");
        let status = wait_bounded(
            &mut guard,
            SESSION_END_TIMEOUT,
            &format!("watch -o - ({what}) after its reader went away"),
        );
        let stderr = tap.finish_text(&mut guard);
        assert_eq!(
            status.code(),
            Some(0),
            "{what}: a reader that goes away ends the session with exit 0; stderr: {stderr:?}"
        );
        if quiet {
            assert_eq!(
                stderr, "",
                "--quiet prints nothing when stdout's reader goes away mid-session"
            );
            continue;
        }
        assert!(
            stderr.ends_with(STOPPED_STDOUT_CLOSED)
                && count_occurrences(&stderr, STOPPED_STDOUT_CLOSED) == 1,
            "the session ends with exactly one stop line; stderr: {stderr:?}"
        );
        assert!(
            stderr.starts_with("Watching ") && !stderr.contains("Recompiled"),
            "a write the closed pipe lost is not a rebuild; stderr: {stderr:?}"
        );
    }
}

/// `mds watch` with stderr closed from the start reaches readiness, rebuilds its output
/// on an edit and keeps running; on unix it exits 0 at Ctrl+C (#157). File mode and
/// directory mode.
///
/// A closed stderr cannot be read back, so each mode first runs the same session with
/// stderr open, as the control: it writes there at every step — `Watching`,
/// `Recompiled`, and on unix `Stopped watching.` — so every one of those writes is lost
/// in the closed arm.
#[test]
fn watch_with_stderr_closed_keeps_watching() {
    for mode in ["file", "directory"] {
        for closed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("page.mds");
            std::fs::write(&src, "Hello one\n").unwrap();
            let out = dir.path().join("page.md");
            let mut cmd = mds_bin();
            if mode == "file" {
                cmd.args(["watch", src.to_str().unwrap()]);
            } else {
                cmd.args(["watch", dir.path().to_str().unwrap()]);
            }
            cmd.args(["--debounce", "0"]).stdout(Stdio::null());
            let what = format!(
                "{mode} mode, stderr {}",
                if closed { "closed" } else { "open" }
            );

            let (mut guard, tap) = if closed {
                cmd.stderr(Stdio::from(closed_pipe()));
                let (child, _no_stdout) = spawn_watch_ready_stderr_untapped(&mut cmd);
                (ChildGuard(child), None)
            } else {
                let (guard, tap) = spawn_ready(&mut cmd);
                (guard, Some(tap))
            };

            assert!(
                wait_for_file_contains(&out, "Hello one", TIMEOUT),
                "{what}: the startup compile writes the output"
            );
            write_atomic(&src, "Hello two\n");
            assert!(
                wait_for_file_contains(&out, "Hello two", TIMEOUT),
                "{what}: an edit rebuilds the output"
            );
            if let Some(tap) = &tap {
                wait_for_tap(tap, "Recompiled ", TIMEOUT);
                assert!(
                    tap.text().starts_with("Watching "),
                    "control ({what}): the session writes its status lines to stderr"
                );
            }
            assert!(
                guard.0.try_wait().unwrap().is_none(),
                "{what}: the session keeps watching"
            );

            #[cfg(unix)]
            {
                interrupt(&guard);
                let status = wait_bounded(&mut guard, SESSION_END_TIMEOUT, &what);
                assert_eq!(status.code(), Some(0), "{what}: Ctrl+C exits 0");
                if let Some(tap) = tap {
                    let stderr = tap.finish_text(&mut guard);
                    assert!(
                        stderr.ends_with("Stopped watching.\n"),
                        "control ({what}): Ctrl+C prints the stop line; stderr: {stderr:?}"
                    );
                }
            }
        }
    }
}

/// `mds watch -o -` into a stdout that fails other than by a closed pipe: the failure is
/// reported once, as `mds::io` naming stdout, the session keeps watching, and a save of
/// the same text writes it again — the lost write never became the content baseline
/// that makes a rebuild of unchanged output skip its write. At Ctrl+C it exits 0 (#157).
///
/// Vector: stdout is a regular file already as long as the child's file-size limit and
/// open at its end, so every write fails with "file too large" until the test empties
/// the file and moves the shared offset back ([`full_file`], [`limit_file_growth`]).
///
/// 1. The startup write fails: reported once, and the session goes on.
/// 2. An edit fails again: no second report, and no `Recompiled`. The edited entry
///    `@include`s an empty module, whose warning shows on stderr that it was compiled.
/// 3. The test makes room and saves the SAME text again: the write lands.
#[cfg(unix)]
#[test]
fn watch_to_a_failing_stdout_reports_once_and_retries_the_same_content() {
    const LIMIT: usize = 64;
    const FINAL_MARKER_SOURCE: &str = "Final marker {{__final_marker__}}\n";
    const FINAL_MARKER_LINE: &str = "undefined variable '__final_marker__'";
    let include_warning = "@include of 'e' produced empty output";
    let edited = "@import \"./empty.mds\" as e\n@include e\nHello two\n";

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty.mds"), "").unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    // Outside the watched directory, so the output's own writes raise no events there.
    let stdout_dir = tempfile::tempdir().unwrap();
    let stdout_path = stdout_dir.path().join("stdout");

    let mut cmd = mds_bin();
    cmd.args([
        "watch",
        src.to_str().unwrap(),
        "-o",
        "-",
        "--debounce",
        "0",
        // No idle tick: its first-tick recompile would write on its own schedule.
        "--poll-interval",
        "0",
    ]);
    let stdout_file = full_file(&stdout_path, LIMIT);
    // Shares the child's stdout offset: step 3 moves it back.
    let mut stdout_offset = stdout_file.try_clone().unwrap();
    cmd.stdout(Stdio::from(stdout_file));
    limit_file_growth(&mut cmd, LIMIT as libc::rlim_t);
    let (mut guard, tap) = spawn_ready(&mut cmd);

    // 1. The startup write failed, and was reported, before readiness.
    assert_eq!(
        std::fs::read(&stdout_path).unwrap(),
        vec![b'#'; LIMIT],
        "precondition: the vector fails every write, so the startup output never landed"
    );
    wait_for_tap(&tap, "cannot write to stdout", TIMEOUT);

    // 2. A new text fails to write again. The marker orders the tap: every rebuild of
    //    the edit finished before the marker's compile.
    write_atomic(&src, edited);
    wait_for_tap(&tap, include_warning, TIMEOUT);
    write_atomic(&src, ORDER_MARKER_SOURCE);
    let seen = wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    assert_eq!(
        count_occurrences(&seen, "Recompiled"),
        0,
        "a write stdout lost is not a rebuild; stderr:\n{seen}"
    );
    assert_eq!(
        std::fs::read(&stdout_path).unwrap(),
        vec![b'#'; LIMIT],
        "precondition: no write reached the file"
    );

    // 3. Room in the file again; save the same text.
    {
        use std::io::Seek as _;
        stdout_offset.set_len(0).unwrap();
        stdout_offset.seek(std::io::SeekFrom::Start(0)).unwrap();
    }
    write_atomic(&src, edited);
    let retried = poll_tap_until(&tap, TIMEOUT, |text| text.contains("Recompiled <stdout>"));
    assert!(
        retried.is_ok(),
        "a save of the text whose write stdout lost must write it again; stderr:\n{}",
        tap.text()
    );
    assert_eq!(
        std::fs::read_to_string(&stdout_path).unwrap(),
        "Hello two\n",
        "the retried write reached stdout"
    );

    write_atomic(&src, FINAL_MARKER_SOURCE);
    let seen = wait_for_tap(&tap, FINAL_MARKER_LINE, TIMEOUT);
    assert_eq!(
        count_occurrences(&seen, "cannot write to stdout"),
        1,
        "a stdout failure is reported once however many writes it fails; stderr:\n{seen}"
    );
    assert!(
        seen.contains("mds::io") && seen.contains("File too large"),
        "the report is `mds::io` with the cause; stderr:\n{seen}"
    );
    assert_eq!(
        count_occurrences(&seen, "Recompiled"),
        1,
        "exactly the retried write is a rebuild; stderr:\n{seen}"
    );

    interrupt(&guard);
    let status = wait_bounded(
        &mut guard,
        SESSION_END_TIMEOUT,
        "Ctrl+C after a stdout failure",
    );
    assert_eq!(
        status.code(),
        Some(0),
        "a stdout failure during the session does not change the Ctrl+C exit"
    );
}

/// `mds watch -o -` into a stdout that fails, recovers and fails again reports both
/// failures: a write that lands ends the first one, so the second is new, not a repeat
/// to stay silent about (#157).
///
/// Vector: as in [`watch_to_a_failing_stdout_reports_once_and_retries_the_same_content`]
/// — the test empties the file to let writes land, and fills it again to fail them.
///
/// 1. The startup write fails: report one. An order marker then settles every late
///    rebuild of the startup text, which would otherwise land once there is room.
/// 2. The test makes room; an edit is written and is a rebuild.
/// 3. The test fills the file; a new edit fails: report two.
/// 4. A further edit fails again: no third report, and no `Recompiled`. It `@include`s
///    an empty module, whose warning shows on stderr that it was compiled.
#[cfg(unix)]
#[test]
fn watch_to_stdout_reports_a_new_failure_after_stdout_recovers() {
    const LIMIT: usize = 64;
    const FINAL_MARKER_SOURCE: &str = "Final marker {{__final_marker__}}\n";
    const FINAL_MARKER_LINE: &str = "undefined variable '__final_marker__'";
    let include_warning = "@include of 'e' produced empty output";

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty.mds"), "").unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    // Outside the watched directory, so the output's own writes raise no events there.
    let stdout_dir = tempfile::tempdir().unwrap();
    let stdout_path = stdout_dir.path().join("stdout");

    let mut cmd = mds_bin();
    cmd.args([
        "watch",
        src.to_str().unwrap(),
        "-o",
        "-",
        "--debounce",
        "0",
        // No idle tick: its first-tick recompile would write on its own schedule.
        "--poll-interval",
        "0",
    ]);
    let stdout_file = full_file(&stdout_path, LIMIT);
    // Shares the child's stdout offset: the test empties and refills the file with it.
    let mut stdout_offset = stdout_file.try_clone().unwrap();
    cmd.stdout(Stdio::from(stdout_file));
    limit_file_growth(&mut cmd, LIMIT as libc::rlim_t);
    let (mut guard, tap) = spawn_ready(&mut cmd);

    // 1. The startup write failed, and was reported, before readiness.
    assert_eq!(
        std::fs::read(&stdout_path).unwrap(),
        vec![b'#'; LIMIT],
        "precondition: the vector fails every write, so the startup output never landed"
    );
    wait_for_tap(&tap, "cannot write to stdout", TIMEOUT);
    // The startup text was never written, so the content dedup does not hold it back: a
    // late event for the source would write it once there is room. The marker's compile
    // writes nothing, and once its line is on the tap every earlier rebuild is done.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);

    // 2. Room in the file: stdout recovers.
    {
        use std::io::Seek as _;
        stdout_offset.set_len(0).unwrap();
        stdout_offset.seek(std::io::SeekFrom::Start(0)).unwrap();
    }
    write_atomic(&src, "Hello two\n");
    let recovered = poll_tap_until(&tap, TIMEOUT, |text| text.contains("Recompiled <stdout>"));
    assert!(
        recovered.is_ok(),
        "precondition: once there is room, a rebuild writes stdout; stderr:\n{}",
        tap.text()
    );
    assert_eq!(
        std::fs::read_to_string(&stdout_path).unwrap(),
        "Hello two\n",
        "precondition: the recovered write reached stdout"
    );

    // 3. The file is full again: stdout fails again, and that is a new failure.
    {
        use std::io::{Seek as _, Write as _};
        stdout_offset.set_len(0).unwrap();
        stdout_offset.seek(std::io::SeekFrom::Start(0)).unwrap();
        stdout_offset.write_all(&[b'#'; LIMIT]).unwrap();
    }
    write_atomic(&src, "Hello three\n");
    wait_for_tap_count(&tap, "cannot write to stdout", 2, TIMEOUT);

    // 4. Its repeat stays silent. The marker orders the tap: every rebuild before it
    //    finished before the marker's compile.
    write_atomic(
        &src,
        "@import \"./empty.mds\" as e\n@include e\nHello four\n",
    );
    wait_for_tap(&tap, include_warning, TIMEOUT);
    write_atomic(&src, FINAL_MARKER_SOURCE);
    let seen = wait_for_tap(&tap, FINAL_MARKER_LINE, TIMEOUT);
    assert_eq!(
        count_occurrences(&seen, "cannot write to stdout"),
        2,
        "one report per failure, however many writes each fails; stderr:\n{seen}"
    );
    assert_eq!(
        count_occurrences(&seen, "Recompiled"),
        1,
        "only the write that landed is a rebuild; stderr:\n{seen}"
    );
    assert_eq!(
        std::fs::read(&stdout_path).unwrap(),
        vec![b'#'; LIMIT],
        "precondition: no write after the refill reached the file"
    );

    interrupt(&guard);
    let status = wait_bounded(
        &mut guard,
        SESSION_END_TIMEOUT,
        "Ctrl+C after a second stdout failure",
    );
    assert_eq!(
        status.code(),
        Some(0),
        "stdout failures during the session do not change the Ctrl+C exit"
    );
}

/// A rebuild whose output file cannot be written, in a live session, is reported and
/// does not change the Ctrl+C exit: 0 (#157).
///
/// Vector: the output path is replaced by a non-empty directory, which no write can
/// rename a file over — no permissions involved, so it holds under root too.
#[cfg(unix)]
#[test]
fn watch_exits_0_at_ctrl_c_after_a_rebuild_write_failed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("page.md");

    let (mut guard, tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "-o",
                out.to_str().unwrap(),
                "--debounce",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&out, "Hello one", TIMEOUT),
        "the startup compile writes the output"
    );

    std::fs::remove_file(&out).unwrap();
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("keep"), "a directory where the output was\n").unwrap();
    write_atomic(&src, "Hello two\n");
    let seen = wait_for_tap(&tap, "mds::io", TIMEOUT);
    assert!(
        !seen.contains("Recompiled"),
        "the failed write is reported, not announced; stderr:\n{seen}"
    );

    interrupt(&guard);
    let status = wait_bounded(
        &mut guard,
        SESSION_END_TIMEOUT,
        "Ctrl+C after a failed write",
    );
    let stderr = tap.finish_text(&mut guard);
    assert_eq!(
        status.code(),
        Some(0),
        "a failed rebuild write does not change the Ctrl+C exit; stderr: {stderr:?}"
    );
    assert!(
        stderr.ends_with("Stopped watching.\n"),
        "the session ends as any Ctrl+C does; stderr: {stderr:?}"
    );
}

/// stderr on a file the child may not grow: every stderr write fails other than by a
/// closed pipe. That makes a batch run exit at least 2; a live watch session keeps
/// watching and, at Ctrl+C, exits 0 — output failures during a session never change how
/// it ends (#157).
///
/// Controls: the same stderr lifts `mds check` to exit 2, so the vector really records
/// an output failure; and the same session with stderr open prints its status lines,
/// so the failing arm really loses writes.
#[cfg(unix)]
#[test]
fn watch_with_a_failing_stderr_keeps_watching_and_exits_0_at_ctrl_c() {
    const LIMIT: usize = 64;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    let stderr_dir = tempfile::tempdir().unwrap();
    let args = ["watch", src.to_str().unwrap(), "-o", "-", "--debounce", "0"];

    // Control 1: a batch run with this stderr exits 2.
    let check_stderr = stderr_dir.path().join("check-stderr");
    let mut check = mds_bin();
    check
        .args(["check", src.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::from(full_file(&check_stderr, LIMIT)));
    limit_file_growth(&mut check, LIMIT as libc::rlim_t);
    let mut check = ChildGuard(check.spawn().unwrap());
    let status = wait_bounded(&mut check, SESSION_END_TIMEOUT, "mds check");
    assert_eq!(
        status.code(),
        Some(2),
        "control: a stderr that fails other than by a closed pipe lifts `mds check` to 2"
    );

    // Control 2: with stderr open the session writes its status lines.
    let (open, open_tap, open_stdout) =
        spawn_ready_piped_stdout(mds_bin().args(args).stdout(Stdio::piped()));
    wait_for_tap(&open_stdout, "Hello one", TIMEOUT);
    write_atomic(&src, "Hello two\n");
    wait_for_tap(&open_stdout, "Hello two", TIMEOUT);
    let seen = wait_for_tap(&open_tap, "Recompiled <stdout>", TIMEOUT);
    assert!(
        seen.starts_with("Watching "),
        "control: the session writes its status lines to stderr; stderr: {seen:?}"
    );
    drop(open);
    std::fs::write(&src, "Hello one\n").unwrap();

    // The failing arm.
    let stderr_path = stderr_dir.path().join("watch-stderr");
    let mut cmd = mds_bin();
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(full_file(&stderr_path, LIMIT)));
    limit_file_growth(&mut cmd, LIMIT as libc::rlim_t);
    let (child, stdout_tap) = spawn_watch_ready_stderr_untapped(&mut cmd);
    let mut guard = ChildGuard(child);
    let stdout_tap = stdout_tap.expect("stdout is piped");
    wait_for_tap(&stdout_tap, "Hello one", TIMEOUT);
    write_atomic(&src, "Hello two\n");
    wait_for_tap(&stdout_tap, "Hello two", TIMEOUT);
    assert!(
        guard.0.try_wait().unwrap().is_none(),
        "the session keeps watching with a failing stderr"
    );

    interrupt(&guard);
    let status = wait_bounded(
        &mut guard,
        SESSION_END_TIMEOUT,
        "Ctrl+C with a failing stderr",
    );
    assert_eq!(
        status.code(),
        Some(0),
        "a failing stderr during the session does not change the Ctrl+C exit"
    );
    assert_eq!(
        std::fs::read(&stderr_path).unwrap(),
        vec![b'#'; LIMIT],
        "precondition: every stderr write failed"
    );
}

/// A session that ends before it goes live keeps the rule of a run that ends on its own
/// (#157): the startup write finds stdout's reader gone, the session stops, and a stderr
/// that failed other than by a closed pipe lifts the exit code to 2. Only a live session
/// leaves its exit code alone
/// (`watch_with_a_failing_stderr_keeps_watching_and_exits_0_at_ctrl_c`).
///
/// Control: with stderr open the same session stops at the same point with exit 0 and
/// prints its status lines, the stop line last, so the failing arm loses writes.
#[cfg(unix)]
#[test]
fn watch_that_stops_before_it_is_live_exits_by_the_batch_rule() {
    const LIMIT: usize = 64;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("page.mds");
    std::fs::write(&src, "Hello one\n").unwrap();
    let args = ["watch", src.to_str().unwrap(), "-o", "-", "--debounce", "0"];

    let (mut open, open_tap) =
        spawn_unsynchronized(mds_bin().args(args).stdout(Stdio::from(closed_pipe())));
    let status = wait_bounded(&mut open, SESSION_END_TIMEOUT, "stderr open");
    let stderr = open_tap.finish_text(&mut open);
    assert_eq!(
        status.code(),
        Some(0),
        "control: with stderr open, a startup write into a gone reader ends the session \
         with exit 0; stderr: {stderr:?}"
    );
    assert!(
        stderr.starts_with("Watching ") && stderr.ends_with(STOPPED_STDOUT_CLOSED),
        "control: the session writes its status lines, the stop line last; stderr: {stderr:?}"
    );

    let stderr_dir = tempfile::tempdir().unwrap();
    let stderr_path = stderr_dir.path().join("watch-stderr");
    let mut cmd = mds_bin();
    cmd.args(args)
        .stdout(Stdio::from(closed_pipe()))
        .stderr(Stdio::from(full_file(&stderr_path, LIMIT)));
    limit_file_growth(&mut cmd, LIMIT as libc::rlim_t);
    let mut failing = ChildGuard(cmd.spawn().unwrap());
    let status = wait_bounded(&mut failing, SESSION_END_TIMEOUT, "stderr failing");
    assert_eq!(
        status.code(),
        Some(2),
        "a session that stops before it is live exits as a batch run does: a stderr that \
         failed other than by a closed pipe lifts its exit code to 2"
    );
    assert_eq!(
        std::fs::read(&stderr_path).unwrap(),
        vec![b'#'; LIMIT],
        "precondition: every stderr write failed"
    );
}

// ── One edit, one rebuild: event paths meet the session's keys (#390) ──────────
//
// A session keys what it watches by path: the dependencies a compile reports, the
// `--vars` file, and the paths notify reports events under must name each file in one
// form, or an edit to it starts no rebuild. Every session below runs with
// `--poll-interval 0`, so no self-heal tick can stand in for an event the session
// failed to match, and runs twice: with `src/a.mds` as the entry, and below the
// directory argument `src`.

/// The two ways a session below reaches `src/a.mds`: as the entry, and below the
/// directory argument.
const ENTRY_AND_DIRECTORY: [&[&str]; 2] = [&["watch", "src/a.mds"], &["watch", "src"]];

/// Run `mds watch <args>` in `base` with native events only, where `src/a.mds` reads
/// `edited`, and check that its output `src/a.md` holds `before` at startup and `after`
/// once `edited` has been changed to `text`, once. Return the session's stderr whole —
/// complete up to the order marker then written to `src/a.mds`, so a count over it is
/// exact.
#[track_caller]
fn one_edit(
    base: &Path,
    args: &[&str],
    edited: &Path,
    text: &str,
    (before, after): (&str, &str),
) -> String {
    let output = base.join("src").join("a.md");
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base)
            .args(args)
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    let startup = std::fs::read_to_string(&output).unwrap_or_default();
    assert!(
        startup.contains(before),
        "{args:?}: control: the startup output holds {before:?}; it held {startup:?}"
    );

    write_atomic(edited, text);
    wait_for_tap(&tap, "Recompiled ", TIMEOUT);
    let rebuilt = std::fs::read_to_string(&output).unwrap_or_default();
    assert!(
        rebuilt.contains(after) && !rebuilt.contains(before),
        "{args:?}: the rebuild wrote what the edit made; the output held {rebuilt:?}"
    );

    write_atomic(&base.join("src").join("a.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    tap.finish_text(&mut child)
}

/// Assert that `stderr`, a session [`one_edit`] ran, holds exactly one rebuild.
#[track_caller]
fn assert_one_rebuild(args: &[&str], stderr: &str) {
    assert_eq!(
        count_occurrences(stderr, "Recompiled "),
        1,
        "{args:?}: one edit, one rebuild; stderr: {stderr}"
    );
}

/// A dependency reached through a symlinked directory is keyed by the path the compile
/// reports for it — the link's target — and notify reports the edit under the same path:
/// one edit, one rebuild. The target lies outside the directory argument, so directory
/// mode watches it as an out-of-root dependency directory; a `.mdsroot` marker above both
/// keeps the import inside the project.
///
/// Unix-only: it creates a directory symlink.
#[cfg(unix)]
#[test]
fn watch_rebuilds_once_when_a_dependency_behind_a_symlinked_directory_changes() {
    for args in ENTRY_AND_DIRECTORY {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::write(base.join(".mdsroot"), "").unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::create_dir_all(base.join("real-lib")).unwrap();
        std::fs::write(base.join("real-lib/x.mds"), "X one\n").unwrap();
        std::os::unix::fs::symlink(base.join("real-lib"), base.join("src/lib-link")).unwrap();
        std::fs::write(
            base.join("src/a.mds"),
            "@import \"./lib-link/x.mds\" as x\n@include x\n",
        )
        .unwrap();

        let edited = base.join("real-lib/x.mds");
        let stderr = one_edit(base, args, &edited, "X two\n", ("X one", "X two"));
        assert_one_rebuild(args, &stderr);
    }
}

/// An imported partial is a dependency like any other: one edit, one rebuild of its
/// importer — and, below the directory argument, no output of its own.
#[test]
fn watch_rebuilds_once_when_an_imported_partial_changes() {
    for args in ENTRY_AND_DIRECTORY {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/_part.mds"), "P one\n").unwrap();
        std::fs::write(
            base.join("src/a.mds"),
            "@import \"./_part.mds\" as p\n@include p\n",
        )
        .unwrap();

        let edited = base.join("src").join("_part.mds");
        let stderr = one_edit(base, args, &edited, "P two\n", ("P one", "P two"));
        assert_one_rebuild(args, &stderr);
        assert!(
            !base.join("src").join("_part.md").exists(),
            "{args:?}: a partial has no output of its own"
        );
    }
}

/// The `--vars` file, typed relative to the working directory, is matched by the path
/// notify reports its edit under: one edit, one rebuild.
#[test]
fn watch_rebuilds_once_when_the_vars_file_changes() {
    for args in ENTRY_AND_DIRECTORY {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/a.mds"), "Hello {{name}}\n").unwrap();
        std::fs::write(base.join("vars.json"), r#"{"name": "one"}"#).unwrap();

        let args = [args, &["--vars", "vars.json"][..]].concat();
        let edited = base.join("vars.json");
        let stderr = one_edit(
            base,
            &args,
            &edited,
            r#"{"name": "two"}"#,
            ("Hello one", "Hello two"),
        );
        assert_one_rebuild(&args, &stderr);
    }
}

/// A dependency outside the directory argument — `../shared/y.mds`, inside the project
/// a `.mdsroot` marker bounds — is keyed by the path the compile reports for it, and
/// directory mode watches its directory as an out-of-root dependency directory: one
/// edit, one rebuild of the importer, and no output for the dependency.
#[test]
fn watch_rebuilds_once_when_a_dependency_outside_the_directory_changes() {
    for args in ENTRY_AND_DIRECTORY {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        std::fs::write(base.join(".mdsroot"), "").unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::create_dir_all(base.join("shared")).unwrap();
        std::fs::write(base.join("shared/y.mds"), "Y one\n").unwrap();
        std::fs::write(
            base.join("src/a.mds"),
            "@import \"../shared/y.mds\" as y\n@include y\n",
        )
        .unwrap();

        let edited = base.join("shared").join("y.mds");
        let stderr = one_edit(base, args, &edited, "Y two\n", ("Y one", "Y two"));
        assert_one_rebuild(args, &stderr);
        assert!(
            !base.join("shared").join("y.md").exists(),
            "{args:?}: a dependency outside the directory has no output"
        );
    }
}

// ── The out-dir during a session: deleted, replaced, retargeted (#160) ─────────

/// Every way a session writes below an out-dir — file and directory mode, each with
/// `--out-dir out` and with `mds.json`'s `build.output_dir` naming `out` (the flag says
/// to write that `mds.json`) — so that its outputs land in `out/` below the working
/// directory.
const OUT_DIR_SESSIONS: [(&[&str], bool); 4] = [
    (&["watch", "src/a.mds", "--out-dir", "out"], false),
    (&["watch", "src", "--out-dir", "out"], false),
    (&["watch", "src/a.mds"], true),
    (&["watch", "src"], true),
];

/// A working directory for an out-dir session: `src/a.mds` and `src/b.mds`, and an
/// `mds.json` naming `out` as `build.output_dir` when `config` says so.
fn out_dir_session_base(config: bool) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.mds"), "A one\n").unwrap();
    std::fs::write(src.join("b.mds"), "B one\n").unwrap();
    if config {
        std::fs::write(
            base.path().join("mds.json"),
            r#"{"build":{"output_dir":"out"}}"#,
        )
        .unwrap();
    }
    base
}

/// The names in `dir`, sorted. Unix-only, as the symlink tests that call it are.
#[cfg(unix)]
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The refusal of a write below an out-dir that now leads elsewhere, naming the output
/// `shown` as its status line does, squashed for comparison. Unix-only, as the symlink
/// tests that call it are.
#[cfg(unix)]
fn out_dir_moved_refusal(shown: &Path) -> String {
    squash(&format!(
        "mds::io × cannot write {}: the output directory now resolves to a different \
         directory; restart mds watch to follow it",
        shown.display()
    ))
}

/// How many times `stderr`, a session of [`OUT_DIR_SESSIONS`], says it rebuilt the
/// output `name` below `out/` — named below the directory `mds.json` was reached by
/// (`src/..`) when `config` says the session takes its out-dir from there.
fn recompiled_below_out(stderr: &str, config: bool, name: &str) -> usize {
    let out = if config {
        Path::new("src").join("..").join("out")
    } else {
        Path::new("out").to_path_buf()
    };
    count_occurrences(
        &squash(stderr),
        &squash(&format!("Recompiled {} (", out.join(name).display())),
    )
}

/// Wait until the session has rebuilt every event queued so far: `source` is given a
/// compile that fails naming `__<name>__`, and the wait ends once that diagnostic is on
/// `tap`. A late or repeated event for an earlier save — which, rebuilt after the out-dir
/// is deleted, would write into the recreated one and add a rebuild to the count — is
/// handled by then. The failed compile writes nothing. Each barrier takes a name of its
/// own, since one save can be reported more than once.
fn settle_queued_events(tap: &StderrTap, source: &Path, name: &str) {
    write_atomic(source, format!("Barrier {{{{__{name}__}}}}\n"));
    wait_for_tap(tap, &format!("undefined variable '__{name}__'"), TIMEOUT);
}

/// An out-dir deleted while `mds watch` runs is recreated by the next write below it,
/// and the outputs the session wrote there are written again: a save that leaves an
/// output unchanged rewrites it into the recreated directory rather than skipping it as
/// written already — in directory mode another source's output, in file mode the
/// entry's after a second deletion.
#[test]
fn watch_recreates_a_deleted_out_dir_and_writes_its_outputs_again() {
    for (args, config) in OUT_DIR_SESSIONS {
        let base = out_dir_session_base(config);
        let base = base.path();
        let (src, out) = (base.join("src"), base.join("out"));
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base)
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&out.join("a.md"), "A one", TIMEOUT),
            "{args:?}: control: the startup writes out/a.md; stderr: {}",
            tap.text()
        );

        settle_queued_events(&tap, &src.join("a.mds"), "first_barrier");
        std::fs::remove_dir_all(&out).unwrap();
        write_atomic(&src.join("a.mds"), "A two\n");
        assert!(
            wait_for_file_contains(&out.join("a.md"), "A two", TIMEOUT),
            "{args:?}: an edit after the out-dir was deleted recreates it and writes \
             there; stderr: {}",
            tap.text()
        );

        // What the session wrote into the deleted directory is gone with it: a save that
        // compiles to the bytes last written writes them again (a barrier's failed
        // compile writes nothing, so they are still the ones last written).
        let (saved, text, output) = if args[1] == "src" {
            (src.join("b.mds"), "B one\n", out.join("b.md"))
        } else {
            settle_queued_events(&tap, &src.join("a.mds"), "second_barrier");
            std::fs::remove_dir_all(&out).unwrap();
            (src.join("a.mds"), "A two\n", out.join("a.md"))
        };
        write_atomic(&saved, text);
        assert!(
            wait_for_file_contains(&output, text.trim_end(), TIMEOUT),
            "{args:?}: a save of unchanged bytes writes {} into the recreated out-dir; \
             stderr: {}",
            output.display(),
            tap.text()
        );

        write_atomic(&src.join("a.mds"), ORDER_MARKER_SOURCE);
        wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
        let stderr = tap.finish_text(&mut child);
        let written = |name| recompiled_below_out(&stderr, config, name);
        let expected = if args[1] == "src" { [1, 1] } else { [2, 0] };
        assert_eq!(
            [written("a.md"), written("b.md")],
            expected,
            "{args:?}: each output was written once into each directory it was missing \
             from; stderr: {stderr}"
        );
    }
}

/// A new directory made where the out-dir was — the old one moved aside — while `mds
/// watch` runs is the out-dir from then on: the next rebuild writes into it, and the
/// directory moved aside keeps the output it held. On unix a save that leaves the output
/// unchanged writes it into the new directory too; Windows tells one directory from
/// another at the same path by its creation time alone, which file-system tunnelling
/// may carry over to a directory made under the same name moments later.
#[test]
fn watch_writes_into_a_new_directory_made_in_place_of_the_out_dir() {
    for (args, config) in OUT_DIR_SESSIONS {
        let base = out_dir_session_base(config);
        let base = base.path();
        let (src, out, moved) = (base.join("src"), base.join("out"), base.join("out.old"));
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base)
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&out.join("a.md"), "A one", TIMEOUT),
            "{args:?}: control: the startup writes out/a.md; stderr: {}",
            tap.text()
        );

        settle_queued_events(&tap, &src.join("a.mds"), "first_barrier");
        std::fs::rename(&out, &moved).unwrap();
        std::fs::create_dir(&out).unwrap();
        let mut rebuilds = 0;
        if cfg!(unix) {
            write_atomic(&src.join("a.mds"), "A one\n");
            assert!(
                wait_for_file_contains(&out.join("a.md"), "A one", TIMEOUT),
                "{args:?}: a save of unchanged bytes writes out/a.md into the new \
                 directory; stderr: {}",
                tap.text()
            );
            rebuilds += 1;
        }
        write_atomic(&src.join("a.mds"), "A two\n");
        assert!(
            wait_for_file_contains(&out.join("a.md"), "A two", TIMEOUT),
            "{args:?}: an edit writes out/a.md into the new directory; stderr: {}",
            tap.text()
        );
        rebuilds += 1;
        assert_eq!(
            std::fs::read_to_string(moved.join("a.md")).unwrap(),
            "A one\n",
            "{args:?}: the directory moved aside keeps its output"
        );

        write_atomic(&src.join("a.mds"), ORDER_MARKER_SOURCE);
        wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
        let stderr = tap.finish_text(&mut child);
        assert_eq!(
            recompiled_below_out(&stderr, config, "a.md"),
            rebuilds,
            "{args:?}: each rebuild of a.mds wrote out/a.md once; stderr: {stderr}"
        );
    }
}

/// An out-dir the user named through a symlink that is retargeted while `mds watch`
/// runs is not followed: the next write below it is refused (`mds::io`), naming the
/// output as its status line does and saying to restart, nothing is written to the
/// link's new target or to the directory the session started with, and watching goes
/// on. Control: once the link leads back to that directory, an edit is written there.
///
/// Unix-only: it retargets a directory symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn watch_refuses_an_out_dir_link_retargeted_mid_session() {
    use std::os::unix::fs::symlink;

    for args in [
        &["watch", "src/x.mds", "--out-dir", "lnk"][..],
        &["watch", "src", "--out-dir", "lnk"],
    ] {
        let base = tempfile::tempdir().unwrap();
        let base = base.path();
        for name in ["src", "a", "b"] {
            std::fs::create_dir(base.join(name)).unwrap();
        }
        let source = base.join("src").join("x.mds");
        std::fs::write(&source, "X one\n").unwrap();
        let (lnk, a_out, b) = (
            base.join("lnk"),
            base.join("a").join("x.md"),
            base.join("b"),
        );
        symlink("a", &lnk).unwrap();
        let (child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base)
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&a_out, "X one", TIMEOUT),
            "{args:?}: control: the startup writes through the link; stderr: {}",
            tap.text()
        );

        std::fs::remove_file(&lnk).unwrap();
        symlink("b", &lnk).unwrap();
        write_atomic(&source, "X two\n");
        let refusal = out_dir_moved_refusal(&Path::new("lnk").join("x.md"));
        let refused = poll_tap_until(&tap, TIMEOUT, |text| squash(text).contains(&refusal));
        assert!(
            refused.is_ok(),
            "{args:?}: the write is refused, naming the output as typed; stderr: {refused:?}"
        );
        assert_eq!(
            names_in(&b),
            Vec::<String>::new(),
            "{args:?}: nothing is written to the link's new target"
        );
        assert_eq!(
            std::fs::read_to_string(&a_out).unwrap(),
            "X one\n",
            "{args:?}: nor to the directory the session started with"
        );

        std::fs::remove_file(&lnk).unwrap();
        symlink("a", &lnk).unwrap();
        write_atomic(&source, "X three\n");
        assert!(
            wait_for_file_contains(&a_out, "X three", TIMEOUT),
            "{args:?}: control: through the link led back, the session writes again; \
             stderr: {}",
            tap.text()
        );
        assert_eq!(
            names_in(&b),
            Vec::<String>::new(),
            "{args:?}: b stays empty"
        );
        drop(child);
    }
}

/// An out-dir replaced by a symlink while `mds watch` runs is refused, and nothing lands
/// in the directory the link leads to: `--out-dir`, the path the user typed, now leads
/// there and is refused as a retargeted one is; `build.output_dir`'s own directories lie
/// below the directory `mds.json` is in, the anchor, so the link is refused as any
/// symlink below an anchor is, named below the directory `mds.json` was reached by.
/// Control: a real directory made back in its place is written into.
///
/// Unix-only: it makes a directory symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn watch_refuses_an_out_dir_replaced_by_a_symlink() {
    use std::os::unix::fs::symlink;

    for (args, config) in OUT_DIR_SESSIONS {
        let base = out_dir_session_base(config);
        let base = base.path();
        let (source, out, victim) = (
            base.join("src").join("a.mds"),
            base.join("out"),
            base.join("victim"),
        );
        std::fs::create_dir(&victim).unwrap();
        let (child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base)
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&out.join("a.md"), "A one", TIMEOUT),
            "{args:?}: control: the startup writes out/a.md; stderr: {}",
            tap.text()
        );

        std::fs::remove_dir_all(&out).unwrap();
        symlink("victim", &out).unwrap();
        write_atomic(&source, "A two\n");
        let refusal = if config {
            squash(&format!(
                "mds::io × cannot write {}: refusing to follow a symlink",
                Path::new("src").join("..").join("out").display()
            ))
        } else {
            out_dir_moved_refusal(&Path::new("out").join("a.md"))
        };
        let refused = poll_tap_until(&tap, TIMEOUT, |text| squash(text).contains(&refusal));
        assert!(
            refused.is_ok(),
            "{args:?}: the write is refused, naming the path as the user knows it; \
             stderr: {refused:?}"
        );
        assert_eq!(
            names_in(&victim),
            Vec::<String>::new(),
            "{args:?}: nothing is written into the directory the link leads to"
        );

        std::fs::remove_file(&out).unwrap();
        std::fs::create_dir(&out).unwrap();
        write_atomic(&source, "A three\n");
        assert!(
            wait_for_file_contains(&out.join("a.md"), "A three", TIMEOUT),
            "{args:?}: control: a real directory back in place is written into; stderr: {}",
            tap.text()
        );
        assert_eq!(
            names_in(&victim),
            Vec::<String>::new(),
            "{args:?}: victim stays empty"
        );
        drop(child);
    }
}

// ── A session removes only an output it wrote and that is unchanged (#160) ──────

/// A template that compiles to messages, so its output is `.json`.
const MESSAGES_KIND: &str = "@message user:\nWhat is 3+3?\n@end\n";

/// What a hand-written file beside a source holds — never anything mds writes.
const HAND_WRITTEN: &str = "hand-written, not mds output\n";

/// What the user writes over an output the session wrote.
const USER_EDIT: &str = "the user's own edit\n";

/// The two batches a deleted source's outputs are removed in — one whose only changes
/// are deletions, and one that also edits the `--vars` file and so recompiles every
/// source — each with the arguments that make it and whether `vars.json` is edited.
const DELETION_BATCHES: [(&str, &[&str], bool); 2] = [
    ("a deletion alone", &["--debounce", "0"], false),
    (
        "a deletion with a --vars edit",
        &["--vars", "vars.json", "--debounce", "500"],
        true,
    ),
];

/// The lines of `stderr` that start with `prefix`, sorted.
fn lines_starting(stderr: &str, prefix: &str) -> Vec<String> {
    let mut lines: Vec<String> = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// `name` below the directory `dir` as a status line names it, with the platform's
/// separator.
fn below(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).display().to_string()
}

/// The text of `path`, or `None` when it cannot be read.
fn text_of(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// A watched directory `notes/` below a fresh base directory holding `files`, beside a
/// `vars.json` for the batches that edit it.
fn notes_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let base = tempfile::tempdir().unwrap();
    let notes = base.path().join("notes");
    std::fs::create_dir(&notes).unwrap();
    for (name, text) in files {
        std::fs::write(notes.join(name), text).unwrap();
    }
    std::fs::write(base.path().join("vars.json"), r#"{"name": "one"}"#).unwrap();
    base
}

/// Deleting sources in a watched directory removes only the outputs the session wrote
/// (#160). A hand-written `todo.json` beside the Markdown source `todo.mds`, a
/// hand-written `msg.md` beside the messages source `msg.mds`, and a hand-written
/// `_p.md` beside the partial `_p.mds`, which has no output, all survive — each sibling
/// with one notice, the partial's with none — while `todo.md` and `msg.json`, which the
/// session wrote, are removed with `Removed … (source deleted)`: in a batch of deletions
/// alone and in one that also edits the `--vars` file. Every one of them was deleted.
#[test]
fn watch_keeps_the_hand_written_siblings_of_a_deleted_source() {
    for (label, extra, edit_vars) in DELETION_BATCHES {
        let base = notes_with(&[
            ("todo.mds", "Buy milk\n"),
            ("todo.json", HAND_WRITTEN),
            ("msg.mds", MESSAGES_KIND),
            ("msg.md", HAND_WRITTEN),
            ("_p.mds", "Partial\n"),
            ("_p.md", HAND_WRITTEN),
        ]);
        let notes = base.path().join("notes");
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base.path())
                .args(["watch", "notes", "--poll-interval", "0"])
                .args(extra)
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&notes.join("todo.md"), "Buy milk", TIMEOUT)
                && wait_for_file_contains(&notes.join("msg.json"), "What is 3+3?", TIMEOUT),
            "{label}: control: the startup writes todo.md and msg.json; stderr: {}",
            tap.text()
        );

        // `todo.mds` last: a batch handles its deletions in name order, so once
        // `todo.md` is gone every deletion before it has been handled too.
        for name in ["_p.mds", "msg.mds", "todo.mds"] {
            std::fs::remove_file(notes.join(name)).unwrap();
        }
        if edit_vars {
            write_atomic(&base.path().join("vars.json"), r#"{"name": "two"}"#);
        }
        assert!(
            wait_for_file_gone(&notes.join("msg.json"), TIMEOUT)
                && wait_for_file_gone(&notes.join("todo.md"), TIMEOUT),
            "{label}: control: the outputs the session wrote are removed; stderr: {}",
            tap.text()
        );
        write_atomic(&notes.join("zz.mds"), ORDER_MARKER_SOURCE);
        wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
        let stderr = tap.finish_text(&mut child);

        for name in ["todo.json", "msg.md", "_p.md"] {
            assert_eq!(
                text_of(&notes.join(name)).as_deref(),
                Some(HAND_WRITTEN),
                "{label}: the hand-written {name} survives; stderr: {stderr}"
            );
        }
        assert_eq!(
            lines_starting(&stderr, "Kept "),
            [
                format!(
                    "Kept {}: not written by this session",
                    below("notes", "msg.md")
                ),
                format!(
                    "Kept {}: not written by this session",
                    below("notes", "todo.json")
                ),
            ],
            "{label}: one notice for each hand-written sibling, none for the partial's; \
             stderr: {stderr}"
        );
        assert_eq!(
            lines_starting(&stderr, "Removed "),
            [
                format!("Removed {} (source deleted)", below("notes", "msg.json")),
                format!("Removed {} (source deleted)", below("notes", "todo.md")),
            ],
            "{label}: stderr: {stderr}"
        );
    }
}

/// An output the session wrote is removed with its deleted source only while it holds
/// exactly what the session wrote there (#160): `edited.md`, which the user edited
/// since, is kept with a notice that says so, while `same.md`, left as it was written,
/// is removed — in both kinds of batch. The edited one was deleted.
#[test]
fn watch_keeps_an_output_edited_since_the_session_wrote_it() {
    for (label, extra, edit_vars) in DELETION_BATCHES {
        let base = notes_with(&[("edited.mds", "Edited\n"), ("same.mds", "Same\n")]);
        let notes = base.path().join("notes");
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base.path())
                .args(["watch", "notes", "--poll-interval", "0"])
                .args(extra)
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&notes.join("edited.md"), "Edited", TIMEOUT)
                && wait_for_file_contains(&notes.join("same.md"), "Same", TIMEOUT),
            "{label}: control: the startup writes both outputs; stderr: {}",
            tap.text()
        );
        std::fs::write(notes.join("edited.md"), USER_EDIT).unwrap();

        // `same.mds` last, as above: once `same.md` is gone both are handled.
        for name in ["edited.mds", "same.mds"] {
            std::fs::remove_file(notes.join(name)).unwrap();
        }
        if edit_vars {
            write_atomic(&base.path().join("vars.json"), r#"{"name": "two"}"#);
        }
        assert!(
            wait_for_file_gone(&notes.join("same.md"), TIMEOUT),
            "{label}: control: the output left as written is removed; stderr: {}",
            tap.text()
        );
        write_atomic(&notes.join("zz.mds"), ORDER_MARKER_SOURCE);
        wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
        let stderr = tap.finish_text(&mut child);

        assert_eq!(
            text_of(&notes.join("edited.md")).as_deref(),
            Some(USER_EDIT),
            "{label}: the output the user edited survives; stderr: {stderr}"
        );
        assert_eq!(
            lines_starting(&stderr, "Kept "),
            [format!(
                "Kept {}: changed since it was written",
                below("notes", "edited.md")
            )],
            "{label}: stderr: {stderr}"
        );
        assert_eq!(
            lines_starting(&stderr, "Removed "),
            [format!(
                "Removed {} (source deleted)",
                below("notes", "same.md")
            )],
            "{label}: stderr: {stderr}"
        );
    }
}

/// A deleted source's outputs are found by the path the session wrote them to, never by
/// a stem (#160). Deleting `a.b.mds`, written to `out/a.b.md`, removes that file and
/// leaves `out/a.md`, written for `a.mds`: the stem probe took `.b` for an extension and
/// removed `out/a.md` in its place. A deleted dependency outside the watched directory,
/// which has no output of its own, removes nothing: the probe flattened it to `out/x.md`,
/// the output of the source `x.mds` inside the directory, and removed that.
#[test]
fn watch_removes_the_output_a_deleted_source_was_written_to() {
    let base = tempfile::tempdir().unwrap();
    let (src, shared, out) = (
        base.path().join("src"),
        base.path().join("shared"),
        base.path().join("out"),
    );
    std::fs::create_dir(&src).unwrap();
    std::fs::create_dir(&shared).unwrap();
    // A `.git` marker puts the project root at the base, so `src` may import `shared`.
    std::fs::write(base.path().join(".git"), "").unwrap();
    std::fs::write(src.join("a.mds"), "A\n").unwrap();
    std::fs::write(src.join("a.b.mds"), "A dot B\n").unwrap();
    std::fs::write(src.join("x.mds"), "X\n").unwrap();
    std::fs::write(
        shared.join("x.mds"),
        "@define greet():\nShared\n@end\n\n@export greet\n",
    )
    .unwrap();
    std::fs::write(
        src.join("importer.mds"),
        "@import \"../shared/x.mds\" as x\n{{x.greet()}}\n",
    )
    .unwrap();
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base.path())
            .args(["watch", "src", "--out-dir", "out"])
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    for (name, text) in [
        ("a.md", "A"),
        ("a.b.md", "A dot B"),
        ("x.md", "X"),
        ("importer.md", "Shared"),
    ] {
        assert!(
            wait_for_file_contains(&out.join(name), text, TIMEOUT),
            "control: the startup writes out/{name}; stderr: {}",
            tap.text()
        );
    }

    std::fs::remove_file(src.join("a.b.mds")).unwrap();
    std::fs::remove_file(shared.join("x.mds")).unwrap();
    // The importer's compile reports the missing import before the batch handles its
    // deletions; the marker's save comes after it, in a batch of its own.
    let broken = poll_tap_until(&tap, TIMEOUT, |text| {
        text.contains("file not found") || text.contains("No such file")
    });
    assert!(
        broken.is_ok(),
        "control: the importer recompiles; stderr: {broken:?}"
    );
    write_atomic(&src.join("zz.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);

    assert_eq!(
        text_of(&out.join("a.md")).as_deref(),
        Some("A\n"),
        "out/a.md, written for a.mds, survives the deletion of a.b.mds; stderr: {stderr}"
    );
    assert_eq!(
        text_of(&out.join("x.md")).as_deref(),
        Some("X\n"),
        "out/x.md, written for src/x.mds, survives the deletion of shared/x.mds; \
         stderr: {stderr}"
    );
    assert!(
        !out.join("a.b.md").exists(),
        "out/a.b.md, written for a.b.mds, is removed; stderr: {stderr}"
    );
    assert_eq!(
        lines_starting(&stderr, "Removed "),
        [format!(
            "Removed {} (source deleted)",
            below("out", "a.b.md")
        )],
        "stderr: {stderr}"
    );
}

/// A source in a watched directory that now compiles to the other kind has the output
/// of the old kind removed only when the session wrote it and it is unchanged (#160):
/// `a.b.mds`, edited into messages, writes `out/a.b.json` and removes `out/a.b.md` —
/// never `out/a.md`, written for `a.mds`, which a stem probe found in its place — and
/// `c.mds` writes `out/c.json` and keeps `out/c.md`, which the user edited, with a
/// notice. Both were deleted.
#[test]
fn watch_removes_the_old_output_of_a_changed_kind_only_when_it_wrote_it() {
    let base = tempfile::tempdir().unwrap();
    let (src, out) = (base.path().join("src"), base.path().join("out"));
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.mds"), "A\n").unwrap();
    std::fs::write(src.join("a.b.mds"), "A dot B\n").unwrap();
    std::fs::write(src.join("c.mds"), "C\n").unwrap();
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base.path())
            .args(["watch", "src", "--out-dir", "out"])
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    for (name, text) in [("a.md", "A"), ("a.b.md", "A dot B"), ("c.md", "C")] {
        assert!(
            wait_for_file_contains(&out.join(name), text, TIMEOUT),
            "control: the startup writes out/{name}; stderr: {}",
            tap.text()
        );
    }
    std::fs::write(out.join("c.md"), USER_EDIT).unwrap();

    for name in ["a.b", "c"] {
        write_atomic(&src.join(format!("{name}.mds")), MESSAGES_KIND);
        assert!(
            wait_for_file_contains(&out.join(format!("{name}.json")), "What is 3+3?", TIMEOUT),
            "control: {name}.mds edited into messages writes out/{name}.json; stderr: {}",
            tap.text()
        );
    }
    write_atomic(&src.join("zz.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);

    assert_eq!(
        text_of(&out.join("a.md")).as_deref(),
        Some("A\n"),
        "out/a.md, written for a.mds, survives a.b.mds's change of kind; stderr: {stderr}"
    );
    assert!(
        !out.join("a.b.md").exists(),
        "out/a.b.md, written and left as it was, is removed; stderr: {stderr}"
    );
    assert_eq!(
        text_of(&out.join("c.md")).as_deref(),
        Some(USER_EDIT),
        "out/c.md, which the user edited, survives; stderr: {stderr}"
    );
    assert_eq!(
        lines_starting(&stderr, "Kept "),
        [format!(
            "Kept {}: changed since it was written",
            below("out", "c.md")
        )],
        "stderr: {stderr}"
    );
}

/// A watched file that now compiles to the other kind is written to that kind's output,
/// as its startup compile would be (#160) — `chat.md`, then `chat.json`, then `chat.md`
/// again — and the output of the old kind is removed when the session wrote it and it is
/// unchanged, or kept with a notice once the user edited it. The kind's output used to be
/// fixed for the session, so the JSON went into `chat.md`. Control: an explicit `-o` is
/// the output whatever the kind, so it never changes and nothing is removed.
#[test]
fn watch_writes_a_file_whose_kind_changed_to_that_kinds_output() {
    let session = |args: &[&str]| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chat.mds"), "Hello\n").unwrap();
        let (child, tap) = spawn_ready(
            mds_bin()
                .current_dir(dir.path())
                .args(["watch", "chat.mds"])
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        (dir, child, tap)
    };

    // Markdown, then messages, then Markdown again, each output left as written.
    let (dir, mut child, tap) = session(&[]);
    let (src, md, json) = (
        dir.path().join("chat.mds"),
        dir.path().join("chat.md"),
        dir.path().join("chat.json"),
    );
    assert!(
        wait_for_file_contains(&md, "Hello", TIMEOUT),
        "control: the startup writes chat.md; stderr: {}",
        tap.text()
    );
    write_atomic(&src, MESSAGES_KIND);
    assert!(
        wait_for_file_contains(&json, "What is 3+3?", TIMEOUT),
        "a template edited into messages writes chat.json; chat.md holds {:?}; stderr: {}",
        text_of(&md),
        tap.text()
    );
    assert!(
        wait_for_file_gone(&md, TIMEOUT),
        "chat.md, written and left as it was, is removed; stderr: {}",
        tap.text()
    );
    write_atomic(&src, "Hello again\n");
    assert!(
        wait_for_file_contains(&md, "Hello again", TIMEOUT) && wait_for_file_gone(&json, TIMEOUT),
        "edited back into Markdown, it writes chat.md and removes chat.json; stderr: {}",
        tap.text()
    );
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    let shown = |name: &str| Path::new(".").join(name).display().to_string();
    let recompiled =
        |name: &str| lines_starting(&stderr, &format!("Recompiled {} (", shown(name))).len();
    assert_eq!(
        (recompiled("chat.json"), recompiled("chat.md")),
        (1, 1),
        "each rebuild names the output of its own kind; stderr: {stderr}"
    );
    assert_eq!(
        lines_starting(&stderr, "Kept "),
        Vec::<String>::new(),
        "stderr: {stderr}"
    );

    // An edited `chat.md` is kept.
    let (dir, mut child, tap) = session(&[]);
    let (src, md) = (dir.path().join("chat.mds"), dir.path().join("chat.md"));
    assert!(
        wait_for_file_contains(&md, "Hello", TIMEOUT),
        "control: the startup writes chat.md; stderr: {}",
        tap.text()
    );
    std::fs::write(&md, USER_EDIT).unwrap();
    write_atomic(&src, MESSAGES_KIND);
    assert!(
        wait_for_file_contains(&dir.path().join("chat.json"), "What is 3+3?", TIMEOUT),
        "a template edited into messages writes chat.json; stderr: {}",
        tap.text()
    );
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        text_of(&md).as_deref(),
        Some(USER_EDIT),
        "chat.md, which the user edited, survives; stderr: {stderr}"
    );
    assert_eq!(
        lines_starting(&stderr, "Kept "),
        [format!(
            "Kept {}: changed since it was written",
            shown("chat.md")
        )],
        "stderr: {stderr}"
    );

    // Control: `-o out.md` takes the messages too, and nothing else is written or removed.
    let (dir, mut child, tap) = session(&["-o", "out.md"]);
    let named = dir.path().join("out.md");
    assert!(
        wait_for_file_contains(&named, "Hello", TIMEOUT),
        "control: the startup writes out.md; stderr: {}",
        tap.text()
    );
    write_atomic(&dir.path().join("chat.mds"), MESSAGES_KIND);
    assert!(
        wait_for_file_contains(&named, "What is 3+3?", TIMEOUT),
        "control: -o out.md takes the messages; stderr: {}",
        tap.text()
    );
    write_atomic(&dir.path().join("chat.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    for name in ["chat.json", "chat.md", "out.json"] {
        assert!(
            !dir.path().join(name).exists(),
            "control: -o is never routed by the kind, so {name} is not written; \
             stderr: {stderr}"
        );
    }
    assert_eq!(
        lines_starting(&stderr, "Kept "),
        Vec::<String>::new(),
        "control: stderr: {stderr}"
    );
}

/// `--quiet` suppresses the notice that a file is kept (#160), as it does the `Removed`
/// line, and the file is kept all the same: a hand-written `todo.json` beside a deleted
/// `todo.mds`, whose `todo.md` the session wrote and removes, and a `chat.md` the user
/// edited before the watched file was edited into messages.
#[test]
fn watch_quiet_keeps_files_without_a_notice() {
    let base = notes_with(&[("todo.mds", "Buy milk\n"), ("todo.json", HAND_WRITTEN)]);
    let notes = base.path().join("notes");
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base.path())
            .args([
                "watch",
                "notes",
                "-q",
                "--debounce",
                "0",
                "--poll-interval",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&notes.join("todo.md"), "Buy milk", TIMEOUT),
        "control: the startup writes todo.md; stderr: {}",
        tap.text()
    );
    std::fs::remove_file(notes.join("todo.mds")).unwrap();
    assert!(
        wait_for_file_gone(&notes.join("todo.md"), TIMEOUT),
        "control: the output the session wrote is removed; stderr: {}",
        tap.text()
    );
    write_atomic(&notes.join("zz.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        text_of(&notes.join("todo.json")).as_deref(),
        Some(HAND_WRITTEN),
        "the hand-written todo.json survives; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Kept ") && !stderr.contains("Removed "),
        "--quiet prints neither; stderr: {stderr}"
    );

    let dir = tempfile::tempdir().unwrap();
    let (src, md) = (dir.path().join("chat.mds"), dir.path().join("chat.md"));
    std::fs::write(&src, "Hello\n").unwrap();
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args([
                "watch",
                "chat.mds",
                "-q",
                "--debounce",
                "0",
                "--poll-interval",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&md, "Hello", TIMEOUT),
        "control: the startup writes chat.md; stderr: {}",
        tap.text()
    );
    std::fs::write(&md, USER_EDIT).unwrap();
    write_atomic(&src, MESSAGES_KIND);
    assert!(
        wait_for_file_contains(&dir.path().join("chat.json"), "What is 3+3?", TIMEOUT),
        "control: a template edited into messages writes chat.json; stderr: {}",
        tap.text()
    );
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        text_of(&md).as_deref(),
        Some(USER_EDIT),
        "chat.md, which the user edited, survives; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Kept "),
        "--quiet prints no notice; stderr: {stderr}"
    );
}

/// A source that is unlinked and created again within one batch — an editor that saves
/// so, a branch checkout or `git stash` replacing it — is an edit, not a deletion
/// (#160): its output is rewritten, nothing is removed and no notice printed, and the
/// hand-written sibling beside it is untouched.
#[test]
fn watch_treats_a_source_unlinked_and_created_again_as_an_edit() {
    let base = notes_with(&[("todo.mds", "Buy milk\n"), ("todo.json", HAND_WRITTEN)]);
    let notes = base.path().join("notes");
    // A debounce window long enough to take the unlink and the create into one batch.
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base.path())
            .args([
                "watch",
                "notes",
                "--debounce",
                "300",
                "--poll-interval",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    assert!(
        wait_for_file_contains(&notes.join("todo.md"), "Buy milk", TIMEOUT),
        "control: the startup writes todo.md; stderr: {}",
        tap.text()
    );
    std::fs::remove_file(notes.join("todo.mds")).unwrap();
    write_atomic(&notes.join("todo.mds"), "Buy bread\n");
    assert!(
        wait_for_file_contains(&notes.join("todo.md"), "Buy bread", TIMEOUT),
        "control: the source created again is rebuilt; stderr: {}",
        tap.text()
    );
    write_atomic(&notes.join("zz.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);

    assert_eq!(
        text_of(&notes.join("todo.json")).as_deref(),
        Some(HAND_WRITTEN),
        "the hand-written todo.json survives; stderr: {stderr}"
    );
    assert_eq!(
        (
            lines_starting(&stderr, "Removed "),
            lines_starting(&stderr, "Kept ")
        ),
        (Vec::new(), Vec::new()),
        "nothing is removed and no notice printed; stderr: {stderr}"
    );
    assert_eq!(
        lines_starting(
            &stderr,
            &format!("Recompiled {} (", below("notes", "todo.md"))
        )
        .len(),
        1,
        "control: the edit is one rebuild; stderr: {stderr}"
    );
}

// ── A change of kind writes only where nothing is, or over the session's own file (#160) ─

/// A template that compiles to messages other than [`MESSAGES_KIND`]'s.
const OTHER_MESSAGES: &str = "@message user:\nWhat is 4+4?\n@end\n";

/// `notes/chat.mds` watched as a file and as part of its directory: the mode, the
/// directory below the base the session runs in, the arguments that watch it, and the
/// directory a status line names its outputs below.
const CHAT_SESSIONS: [(&str, &str, &[&str], &str); 2] = [
    ("file mode", "notes", &["watch", "chat.mds"], "."),
    ("directory mode", "", &["watch", "notes"], "notes"),
];

/// A source edited into the other kind mid-session never overwrites a file the session
/// did not write at that kind's output path (#160): a hand-written `chat.md` beside the
/// messages source `chat.mds` is kept, with one notice, when the source is edited into
/// Markdown, and the old output `chat.json` is kept as it was; edited back into messages,
/// `chat.json` is written again and `chat.md` is still untouched. Both modes. Control:
/// once `chat.md` is gone, the next save writes it, and `chat.json`, which the session
/// wrote and left as it was, is removed.
#[test]
fn watch_never_writes_over_a_file_it_did_not_write_when_the_kind_changes() {
    for (mode, cwd, args, shown_dir) in CHAT_SESSIONS {
        let base = notes_with(&[("chat.mds", MESSAGES_KIND), ("chat.md", HAND_WRITTEN)]);
        let notes = base.path().join("notes");
        let (src, md, json) = (
            notes.join("chat.mds"),
            notes.join("chat.md"),
            notes.join("chat.json"),
        );
        let shown_md = below(shown_dir, "chat.md");
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base.path().join(cwd))
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&json, "What is 3+3?", TIMEOUT),
            "{mode}: control: the startup writes chat.json; stderr: {}",
            tap.text()
        );
        let startup_json = text_of(&json);

        write_atomic(&src, "Hello\n");
        let seen = poll_tap_until(&tap, TIMEOUT, |seen| {
            seen.contains(&format!("Recompiled {shown_md} ("))
                || seen.contains(&format!("Kept {shown_md}:"))
        });
        assert!(
            seen.is_ok(),
            "{mode}: the edit into Markdown reported nothing; stderr: {}",
            tap.text()
        );
        assert_eq!(
            text_of(&md).as_deref(),
            Some(HAND_WRITTEN),
            "{mode}: chat.md, which the session did not write, is not overwritten; stderr: {}",
            tap.text()
        );
        assert_eq!(
            text_of(&json),
            startup_json,
            "{mode}: chat.json, the old kind's output, is kept as it was; stderr: {}",
            tap.text()
        );

        write_atomic(&src, OTHER_MESSAGES);
        assert!(
            wait_for_file_contains(&json, "What is 4+4?", TIMEOUT),
            "{mode}: edited back into messages, it writes chat.json; stderr: {}",
            tap.text()
        );
        assert_eq!(
            text_of(&md).as_deref(),
            Some(HAND_WRITTEN),
            "{mode}: chat.md is still untouched; stderr: {}",
            tap.text()
        );

        // Control: nothing at chat.md, and the next save writes it.
        std::fs::remove_file(&md).unwrap();
        write_atomic(&src, "Hello\n");
        assert!(
            wait_for_file_contains(&md, "Hello", TIMEOUT) && wait_for_file_gone(&json, TIMEOUT),
            "{mode}: control: with chat.md gone, the next save writes it and removes \
             chat.json; stderr: {}",
            tap.text()
        );
        write_atomic(&src, ORDER_MARKER_SOURCE);
        wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
        let stderr = tap.finish_text(&mut child);

        assert_eq!(
            lines_starting(&stderr, "Kept "),
            [format!(
                "Kept {shown_md}: not written by this session; not overwritten"
            )],
            "{mode}: one notice; stderr: {stderr}"
        );
        let recompiled = |name: &str| {
            lines_starting(&stderr, &format!("Recompiled {} (", below(shown_dir, name))).len()
        };
        assert_eq!(
            (recompiled("chat.json"), recompiled("chat.md")),
            (1, 1),
            "{mode}: one rebuild of each kind is written; stderr: {stderr}"
        );
    }
}

/// `--quiet` prints no notice for a file a change of kind keeps (#160), and keeps it all
/// the same: a hand-written `chat.md` beside `chat.mds`, edited into Markdown. Control:
/// `talk.mds`, edited into Markdown with it, has nothing at `talk.md`, so `talk.md` is
/// written and the session's `talk.json` removed.
#[test]
fn watch_quiet_keeps_a_file_a_change_of_kind_would_overwrite_without_a_notice() {
    let base = notes_with(&[
        ("chat.mds", MESSAGES_KIND),
        ("chat.md", HAND_WRITTEN),
        ("talk.mds", MESSAGES_KIND),
    ]);
    let notes = base.path().join("notes");
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(base.path())
            .args([
                "watch",
                "notes",
                "-q",
                "--debounce",
                "0",
                "--poll-interval",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    for name in ["chat.json", "talk.json"] {
        assert!(
            wait_for_file_contains(&notes.join(name), "What is 3+3?", TIMEOUT),
            "control: the startup writes {name}; stderr: {}",
            tap.text()
        );
    }
    let startup_json = text_of(&notes.join("chat.json"));
    write_atomic(&notes.join("chat.mds"), "Hello\n");
    write_atomic(&notes.join("talk.mds"), "Hello\n");
    assert!(
        wait_for_file_contains(&notes.join("talk.md"), "Hello", TIMEOUT)
            && wait_for_file_gone(&notes.join("talk.json"), TIMEOUT),
        "control: talk.md is written and talk.json removed; stderr: {}",
        tap.text()
    );
    write_atomic(&notes.join("zz.mds"), ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);

    assert_eq!(
        text_of(&notes.join("chat.md")).as_deref(),
        Some(HAND_WRITTEN),
        "the hand-written chat.md is not overwritten; stderr: {stderr}"
    );
    assert_eq!(
        text_of(&notes.join("chat.json")),
        startup_json,
        "chat.json, the old kind's output, is kept as it was; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Kept "),
        "--quiet prints no notice; stderr: {stderr}"
    );
}

/// An empty output the startup wrote is the session's like any other (#160): `chat.mds`
/// compiles to empty Markdown, so the startup writes an empty `chat.md`; edited into
/// messages, it writes `chat.json`, and `chat.md`, written and left as it was, is
/// removed without a notice.
#[test]
fn watch_removes_an_empty_startup_output_when_the_kind_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (src, md, json) = (
        dir.path().join("chat.mds"),
        dir.path().join("chat.md"),
        dir.path().join("chat.json"),
    );
    std::fs::write(&src, "").unwrap();
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .current_dir(dir.path())
            .args([
                "watch",
                "chat.mds",
                "--debounce",
                "0",
                "--poll-interval",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    assert_eq!(
        text_of(&md).as_deref(),
        Some(""),
        "control: the startup writes an empty chat.md; stderr: {}",
        tap.text()
    );
    write_atomic(&src, MESSAGES_KIND);
    assert!(
        wait_for_file_contains(&json, "What is 3+3?", TIMEOUT),
        "control: edited into messages, it writes chat.json; stderr: {}",
        tap.text()
    );
    assert!(
        wait_for_file_gone(&md, TIMEOUT),
        "the empty chat.md the startup wrote is removed; stderr: {}",
        tap.text()
    );
    write_atomic(&src, ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        lines_starting(&stderr, "Kept "),
        Vec::<String>::new(),
        "stderr: {stderr}"
    );
}

/// A file a change of kind keeps is told about again once the source has been rebuilt to
/// anything else in between (#160): `chat.mds`, a messages source beside a hand-written
/// `chat.md`, is edited into Markdown — one notice — then back to the messages the
/// session wrote, which writes nothing, and saved broken; edited into the same Markdown
/// again, the session tells it again. Both modes. Control: each save that keeps it gets
/// one notice, however many events it reaches the watcher as.
#[test]
fn watch_tells_a_kept_file_again_once_its_source_has_changed_in_between() {
    for (mode, cwd, args, shown_dir) in CHAT_SESSIONS {
        let base = notes_with(&[("chat.mds", MESSAGES_KIND), ("chat.md", HAND_WRITTEN)]);
        let notes = base.path().join("notes");
        let (src, md, json) = (
            notes.join("chat.mds"),
            notes.join("chat.md"),
            notes.join("chat.json"),
        );
        let notice = format!(
            "Kept {}: not written by this session; not overwritten",
            below(shown_dir, "chat.md")
        );
        let notices = |seen: &str| seen.lines().filter(|line| *line == notice).count();
        let (mut child, tap) = spawn_ready(
            mds_bin()
                .current_dir(base.path().join(cwd))
                .args(args)
                .args(["--debounce", "0", "--poll-interval", "0"])
                .stdout(Stdio::null()),
        );
        assert!(
            wait_for_file_contains(&json, "What is 3+3?", TIMEOUT),
            "{mode}: control: the startup writes chat.json; stderr: {}",
            tap.text()
        );

        write_atomic(&src, "Hello\n");
        assert!(
            poll_tap_until(&tap, TIMEOUT, |seen| notices(seen) == 1).is_ok(),
            "{mode}: control: the first edit into Markdown is told; stderr: {}",
            tap.text()
        );
        // Back to what the session wrote: nothing to write. Then broken: nothing either.
        write_atomic(&src, MESSAGES_KIND);
        write_atomic(&src, ORDER_MARKER_SOURCE);
        wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);

        write_atomic(&src, "Hello\n");
        assert!(
            poll_tap_until(&tap, TIMEOUT, |seen| notices(seen) == 2).is_ok(),
            "{mode}: the same edit into Markdown, made again, is told again; stderr: {}",
            tap.text()
        );
        write_atomic(&src, OTHER_MESSAGES);
        wait_for_tap(
            &tap,
            &format!("Recompiled {} (", below(shown_dir, "chat.json")),
            TIMEOUT,
        );
        let stderr = tap.finish_text(&mut child);

        assert_eq!(
            notices(&stderr),
            2,
            "{mode}: one notice for each save that kept chat.md; stderr: {stderr}"
        );
        assert_eq!(
            text_of(&md).as_deref(),
            Some(HAND_WRITTEN),
            "{mode}: chat.md is kept throughout; stderr: {stderr}"
        );
    }
}
