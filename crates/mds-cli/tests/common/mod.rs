use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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

/// Readiness marker `mds watch` prints on stderr when `MDS_TEST_READY=1` is set.
///
/// Must match `READY_MARKER` in `crates/mds-cli/src/watch.rs`.
const READY_MARKER: &str = "MDS_WATCH_READY";

/// Bound for the startup handshake: process spawn + startup compile + arming.
///
/// This is a *failure* bound, not a synchroniser — the handshake normally completes
/// in milliseconds. It is deliberately looser than the per-edit `TIMEOUT` in
/// `cli_watch.rs` because it also has to absorb process spawn and the compile of
/// every source in the tree while the suite runs at full parallelism.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Captured stderr of a watcher spawned by [`spawn_watch_ready`].
///
/// Holds **everything** the child wrote, including the lines printed before the
/// readiness marker, so tests that count `Compiled to` / `Recompiled` lines see the
/// same stream they would have captured with their own drain thread.
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

/// Spawn a `mds watch` command and block until the watcher is **fully armed**.
///
/// Returns once the child has printed [`READY_MARKER`], which it does only after
/// every watch is registered and every `(mtime, size)` baseline captured. An edit
/// made after this call returns is guaranteed to be seen by the watcher.
///
/// This replaces the previous "wait for the output file to appear, then edit"
/// pattern, which was unsound: the startup output is published *before* the last
/// dependency directory is armed, so an edit could land in a window where the
/// watcher could not observe it. Waiting on the output file synchronised against
/// the wrong event.
///
/// stderr is always piped and drained on a background thread — both to read the
/// marker and so the pipe can never fill and block the child. Use the returned
/// [`StderrTap`] to inspect it.
///
/// # Panics
/// Panics if the child cannot be spawned, or if the marker does not arrive within
/// [`READY_TIMEOUT`] — a watcher that never reports readiness is a defect, not a
/// slow machine.
#[allow(dead_code)]
pub fn spawn_watch_ready(cmd: &mut Command) -> (Child, StderrTap) {
    let mut child = cmd
        .env("MDS_TEST_READY", "1")
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mds watch");

    let handle = child.stderr.take().expect("stderr must be piped");
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let tap = StderrTap(buf.clone());
    let (tx, rx) = mpsc::channel::<()>();

    std::thread::spawn(move || {
        let mut handle = handle;
        let mut chunk = [0u8; 512];
        let mut signalled = false;
        // Bounded by EOF: the loop ends when the child's stderr closes.
        loop {
            match handle.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = buf.lock().expect("stderr tap poisoned");
                    let prev_len = guard.len();
                    guard.extend_from_slice(&chunk[..n]);
                    if !signalled {
                        // Scan the new bytes plus a marker-length-1 overlap, so a
                        // marker straddling a read boundary is still found without
                        // rescanning the whole buffer on every chunk.
                        let from = prev_len.saturating_sub(READY_MARKER.len() - 1);
                        if guard[from..]
                            .windows(READY_MARKER.len())
                            .any(|w| w == READY_MARKER.as_bytes())
                        {
                            signalled = true;
                            let _ = tx.send(());
                        }
                    }
                }
            }
        }
    });

    if rx.recv_timeout(READY_TIMEOUT).is_err() {
        let seen = tap.text();
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "mds watch did not print {READY_MARKER} within {READY_TIMEOUT:?}; \
             stderr so far was:\n{seen}"
        );
    }

    (child, tap)
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
