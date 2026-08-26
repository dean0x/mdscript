use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(dead_code)]
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Return a `Command` for the `mds` binary with `NO_COLOR=1` set so that
/// miette does not emit ANSI SGR codes.  Tests that inspect raw stderr/stdout
/// bytes for control-character sanitization must not see miette's own escape
/// sequences, and suppressing colour globally is the safest way to ensure that.
#[allow(dead_code)]
pub fn mds_bin() -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_mds"));
    cmd.env("NO_COLOR", "1");
    cmd
}

// ── Watch readiness handshake ────────────────────────────────────────────────

/// Contents `mds watch` writes to the file named by `MDS_TEST_READY`.
///
/// Must match `READY_MARKER` in `crates/mds-cli/src/watch.rs`.
const READY_MARKER: &str = "MDS_WATCH_READY";

/// How often [`spawn_watch_ready`] checks for the readiness file.
///
/// Small because it is pure latency on every watch test in the suite: the handshake
/// normally completes in single-digit milliseconds and this is the granularity at
/// which that is observed.
const READY_POLL: Duration = Duration::from_millis(2);

/// Bound for the startup handshake: process spawn + startup compile + arming.
///
/// This is a *failure* bound, not a synchroniser — the handshake normally completes
/// in milliseconds. It is deliberately looser than the per-edit `TIMEOUT` in
/// `cli_watch.rs` because it also has to absorb process spawn and the compile of
/// every source in the tree while the suite runs at full parallelism.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Captured stderr of a watcher spawned by [`spawn_watch_ready`] or
/// [`spawn_watch_unsynchronized`].
///
/// Holds **exactly** what the child wrote and nothing else — the readiness handshake
/// travels over a file, not this stream. That is load-bearing: tests assert that a
/// compile error reaches stderr through `--quiet` and that no raw ESC byte appears in
/// a diagnostic, and both assertions become unfalsifiable if the harness itself
/// contributes bytes here.
#[allow(dead_code)]
#[derive(Clone)]
pub struct StderrTap(Arc<Mutex<Vec<u8>>>);

#[allow(dead_code)]
impl StderrTap {
    /// Raw bytes written to stderr so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.lock().expect("stderr tap poisoned").clone()
    }

    /// Lossy-UTF8 view of [`StderrTap::bytes`].
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

/// Spawn a `mds watch` command and drain its stderr, WITHOUT waiting for readiness.
///
/// Almost every test wants [`spawn_watch_ready`] instead. Use this only when the test
/// is deliberately racing startup — the `watch_*_edit_during_startup_window_is_not_lost`
/// and `watch_*_ctrl_c_during_startup_compile_terminates` tests in `cli_watch.rs`. They
/// must act *inside* the startup window, so they cannot synchronise on it closing.
///
/// stderr is piped and drained on a background thread so the pipe can never fill and
/// block the child.
#[allow(dead_code)]
pub fn spawn_watch_unsynchronized(cmd: &mut Command) -> (Child, StderrTap) {
    let mut child = cmd
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mds watch");

    let handle = child.stderr.take().expect("stderr must be piped");
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let tap = StderrTap(buf.clone());

    std::thread::spawn(move || {
        let mut handle = handle;
        let mut chunk = [0u8; 512];
        // Bounded by EOF: the loop ends when the child's stderr closes.
        loop {
            match handle.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf
                    .lock()
                    .expect("stderr tap poisoned")
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });

    (child, tap)
}

/// Spawn a `mds watch` command and block until the watcher is **fully armed**.
///
/// Returns once the child has created the file named by `MDS_TEST_READY`, which it
/// does only after every watch is registered and every `(mtime, size)` baseline
/// captured. An edit made after this call returns is guaranteed to be seen by the
/// watcher.
///
/// This replaces the previous "wait for the output file to appear, then edit"
/// pattern, which was unsound: the startup output is published *before* the last
/// dependency directory is armed, so an edit could land in a window where the
/// watcher could not observe it. Waiting on the output file synchronised against
/// the wrong event.
///
/// The handshake travels over a **file**, not stderr, so that it cannot perturb the
/// streams tests assert on. A marker written to stderr would have to bypass `--quiet`
/// and would then make every "stderr is non-empty" assertion in the suite vacuous.
///
/// stderr is still piped and drained on a background thread so the pipe can never
/// fill and block the child. Use the returned [`StderrTap`] to inspect it.
///
/// # Panics
/// Panics if the child cannot be spawned, or if readiness is not signalled within
/// [`READY_TIMEOUT`] — a watcher that never reports readiness is a defect, not a
/// slow machine.
#[allow(dead_code)]
pub fn spawn_watch_ready(cmd: &mut Command) -> (Child, StderrTap) {
    // A private directory per spawn: the suite runs at full parallelism, so a shared
    // path would let one watcher's marker satisfy another's wait. Dropped — and so
    // deleted — when this function returns, by which point the marker has been read.
    let ready_dir = tempfile::tempdir().expect("failed to create readiness tempdir");
    let ready_path = ready_dir.path().join("watch-ready");
    assert!(
        ready_path.is_absolute(),
        "MDS_TEST_READY must be absolute; mds watch ignores relative values"
    );

    let (mut child, tap) = spawn_watch_unsynchronized(cmd.env("MDS_TEST_READY", &ready_path));

    // Bounded by READY_TIMEOUT: at most READY_TIMEOUT / READY_POLL iterations.
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        if std::fs::read(&ready_path).is_ok_and(|b| b == READY_MARKER.as_bytes()) {
            return (child, tap);
        }
        // Check liveness before the deadline so a watcher that failed at startup is
        // reported as "exited", not as "timed out".
        if let Ok(Some(status)) = child.try_wait() {
            let seen = tap.text();
            panic!(
                "mds watch exited with {status:?} before signalling readiness; \
                 stderr was:\n{seen}"
            );
        }
        if std::time::Instant::now() >= deadline {
            let seen = tap.text();
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "mds watch did not signal readiness within {READY_TIMEOUT:?}; \
                 stderr so far was:\n{seen}"
            );
        }
        std::thread::sleep(READY_POLL);
    }
}

/// Assert that `s` contains no raw C0 (excluding `\t` and `\n`), DEL, C1, bidi
/// control, line/paragraph separator, or BOM codepoint.
///
/// The predicate iterates over *chars* (Unicode codepoints), not raw bytes,
/// so it correctly identifies C1 characters encoded as two-byte UTF-8
/// sequences (0xC2 0x80–0xC2 0x9F) without false-positives on continuation
/// bytes inside ordinary multi-byte codepoints.
///
/// `\n` is permitted because this helper is used on HUMAN-mode output too, where
/// newlines are preserved by design. Wire-mode newline escaping is asserted
/// explicitly at the call sites that need it.
///
/// # Panics
/// Panics on the first offending codepoint with a human-readable message that
/// includes `label`, the codepoint, its byte offset, and the full string.
#[allow(dead_code)]
pub fn assert_no_control_chars(s: &str, label: &str) {
    for (byte_offset, ch) in s.char_indices() {
        let code = ch as u32;
        let is_c0 = code < 0x20 && code != 0x09 && code != 0x0a;
        let is_del = code == 0x7f;
        let is_c1 = (0x80..=0x9f).contains(&code);
        // All twelve Unicode `Bidi_Control=Yes` codepoints (Trojan Source,
        // CVE-2021-42574) — note U+061C, the only one outside U+200E–U+2069 — plus the
        // JS line/paragraph separators and the invisible BOM. All escaped by the
        // sanitizers.
        let is_format_hazard = matches!(ch,
            '\u{061C}'
            | '\u{200E}' | '\u{200F}'
            | '\u{2028}' | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
        );
        assert!(
            !is_c0 && !is_del && !is_c1 && !is_format_hazard,
            "{label}: raw hostile char U+{code:04X} at byte offset {byte_offset}; \
             full string: {s:?}"
        );
    }
}
