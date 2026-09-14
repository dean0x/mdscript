use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

// ── Frontmatter YAML bounds builders (#162) ──────────────────────────────────

/// Frontmatter size cap (1 MiB) — mirrors `mds-core`'s `MAX_FRONTMATTER_SIZE`.
#[allow(dead_code)]
pub const MAX_FRONTMATTER_SIZE: usize = 1 << 20;

/// Wrap a YAML frontmatter body in `---` fences with a one-line body.
#[allow(dead_code)]
pub fn wrap(yaml: &str) -> String {
    format!("---\n{yaml}---\nHi\n")
}

/// A single `k: <sentinel><padding>\n` line whose total byte length is EXACTLY `bytes`.
///
/// The `ZZSENTINELZZ` marker lets the size-cap tests assert the rejection message never
/// echoes the (arbitrarily large) frontmatter content back to the user.
#[allow(dead_code)]
pub fn fm_of_size(bytes: usize) -> String {
    const PREFIX: &str = "k: ZZSENTINELZZ";
    assert!(bytes > PREFIX.len() + 1, "requested size too small");
    let pad = bytes - PREFIX.len() - 1;
    let out = format!("{PREFIX}{}\n", "x".repeat(pad));
    assert_eq!(
        out.len(),
        bytes,
        "fm_of_size must produce EXACTLY `bytes` bytes"
    );
    out
}

/// An alias-fan-out bomb: `a: &a [x, x, ...(n)]`, `b: [*a, *a, ...(m)]`. Each `*a`
/// re-expands the `n`-element anchor at deserialise time, so the materialised tree far
/// exceeds the node budget while the SOURCE stays small (~700 KB for n = m = 100 000).
#[allow(dead_code)]
pub fn alias_bomb(n: usize, m: usize) -> String {
    let xs = vec!["x"; n].join(", ");
    let refs = vec!["*a"; m].join(", ");
    format!("a: &a [{xs}]\nb: [{refs}]\n")
}

/// `k: [[[...x...]]]` with `d` nested flow sequences around a scalar (a deep-nest DoS
/// repro; the pre-parse flow-depth guard rejects any `d > 1024`).
#[allow(dead_code)]
pub fn nested_flow_seq(d: usize) -> String {
    format!("k: {}x{}\n", "[".repeat(d), "]".repeat(d))
}

// ── Duplicate --vars file key warnings (#326) ────────────────────────────────

/// USER-FACING CONTRACT (#326). `{key}` = dotted/bracketed path, `{path}` = the
/// `--vars` arg as typed on the command line.
#[allow(dead_code)]
pub const DUP_VARS_FILE_WARNING_FMT: &str =
    "warning: key '{key}' is set more than once in vars file {path}; the last value wins";

/// Tail line printed when more distinct duplicate paths exist than the
/// 1 000-path cap on [`mds::VarsLoad::duplicate_keys`] allows; `{n}` is
/// [`mds::VarsLoad::duplicate_keys_omitted`] (#326).
#[allow(dead_code)]
pub const DUP_VARS_FILE_OMITTED_FMT: &str =
    "warning: {n} more duplicate keys in vars file {path} are not listed";

/// Render [`DUP_VARS_FILE_WARNING_FMT`] for a given key path and `--vars` path.
#[allow(dead_code)]
pub fn dup_vars_file_warning(key: &str, path: &Path) -> String {
    DUP_VARS_FILE_WARNING_FMT
        .replace("{key}", key)
        .replace("{path}", &path.display().to_string())
}

/// Render [`DUP_VARS_FILE_OMITTED_FMT`] for a given omitted count and `--vars` path.
#[allow(dead_code)]
pub fn dup_vars_file_omitted(n: usize, path: &Path) -> String {
    DUP_VARS_FILE_OMITTED_FMT
        .replace("{n}", &n.to_string())
        .replace("{path}", &path.display().to_string())
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
///
/// Same body as the private `count_occurrences` in `warnings.rs` / `cli_watch.rs` —
/// those files import only the two render helpers above (E0255 otherwise) and keep
/// their own private copy of this one.
#[allow(dead_code)]
pub fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

// ── Atomic file replacement ──────────────────────────────────────────────────

/// Monotonic counter making every [`write_atomic`] temp name unique within a
/// process; the pid disambiguates across processes.
static WRITE_ATOMIC_SEQ: AtomicU64 = AtomicU64::new(0);

/// Replace `path`'s contents in ONE filesystem event, the way an editor does: write a
/// fresh temp file in the same directory, then `rename` it over `path`.
///
/// Why: `std::fs::write` truncates before it writes, so a zero-debounce watcher can
/// compile the 0-byte intermediate — CI run 34366009518 on 2b91850 printed two
/// `Recompiled` lines for one write. notify 8 surfaces the rename as
/// `Modify(Name(RenameMode::To))` on the destination path, which the watcher treats
/// as a content event.
///
/// The temp name is `.<name>.tmp-<pid>-<n>` — the suffix goes AFTER the name so
/// `Path::extension()` is never `mds`: `collect_mds_files_inner` (output.rs) and the
/// dir-mode event filter (watch.rs) gate on exactly that, so an in-flight temp file
/// is invisible to both.
///
/// No fsync: `rename` orders the replacement for every live process, which is all a
/// watcher needs. The product's own readiness marker is written the same way.
///
/// Deliberate non-user: `watch_single_status_line_per_rebuild`, whose subject IS the
/// coalescing of the truncate+write pair.
///
/// The Windows sharing-violation caveat (a rename over a file another process holds
/// open can fail) is developer-machine only; CI runs this suite on ubuntu.
///
/// # Panics
/// Panics if `path` has no parent or no file name, or if either filesystem step
/// fails — a test whose edit did not land is a defect, not a slow machine.
#[allow(dead_code)]
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) {
    let dir = path
        .parent()
        .unwrap_or_else(|| panic!("write_atomic: path has no parent: {}", path.display()));
    let name = path
        .file_name()
        .unwrap_or_else(|| panic!("write_atomic: path has no file name: {}", path.display()));
    let seq = WRITE_ATOMIC_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".{}.tmp-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        seq
    ));
    debug_assert_ne!(
        tmp.extension().and_then(|e| e.to_str()),
        Some("mds"),
        "write_atomic temp name must never end in .mds; it would be collected as a source"
    );
    if let Err(e) = std::fs::write(&tmp, contents.as_ref()) {
        panic!(
            "write_atomic: cannot write temp file {}: {e}",
            tmp.display()
        );
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        panic!(
            "write_atomic: cannot rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        );
    }
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

/// RAII guard that kills + waits the child on drop.
///
/// Lives here rather than in `cli_watch.rs` so [`PipeTap::finish`] can take
/// `&mut ChildGuard` and thereby establish "reaped before join" in the type, not in a
/// comment: the drain thread's loop ends at EOF, and EOF arrives only once the child's
/// write end is closed.
#[allow(dead_code)]
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[allow(dead_code)]
impl ChildGuard {
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    /// Reap an already-exiting child. `Child::wait` caches its status, so calling this
    /// and then letting `Drop` run is safe.
    pub fn wait_status(&mut self) -> std::process::ExitStatus {
        self.0.wait().expect("wait failed")
    }

    /// Kill (best-effort) and reap. Idempotent — a second call returns the cached
    /// status.
    pub fn kill_and_wait(&mut self) -> std::process::ExitStatus {
        let _ = self.0.kill();
        self.0.wait().expect("wait failed")
    }
}

/// A background-drained capture of one of the child's output pipes.
///
/// Holds **exactly** what the child wrote and nothing else — the readiness handshake
/// travels over a file, not over these streams. That is load-bearing: tests assert
/// that a compile error reaches stderr through `--quiet` and that no raw ESC byte
/// appears in a diagnostic, and both assertions become unfalsifiable if the harness
/// itself contributes bytes here.
///
/// [`PipeTap::bytes`] stays NON-blocking so the live-poll sites keep working;
/// [`PipeTap::finish`] is the end-of-test read that is guaranteed complete.
#[allow(dead_code)]
#[derive(Clone)]
pub struct PipeTap {
    buf: Arc<Mutex<Vec<u8>>>,
    /// `Option` because `finish` takes the handle out; behind `Arc<Mutex<_>>` so
    /// `PipeTap` stays `Clone`. `Clone` is harness API — it lets a tap be shared with
    /// a helper thread — and the mutex is what makes that safe: a clone calling
    /// `finish` concurrently blocks on this slot until the drain has been joined, and
    /// then observes the fully drained buffer. No call site clones a tap today.
    drain: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

/// A [`PipeTap`] over the child's stderr.
#[allow(dead_code)]
pub type StderrTap = PipeTap;

/// A [`PipeTap`] over the child's stdout.
#[allow(dead_code)]
pub type StdoutTap = PipeTap;

#[allow(dead_code)]
impl PipeTap {
    /// Bytes written so far.
    ///
    /// Non-blocking, and therefore carries **no** happens-before edge to the child's
    /// last write: a snapshot taken right after the child is reaped can be a truncated
    /// prefix. Use it only while polling a live child; use [`PipeTap::finish`] for the
    /// final read.
    pub fn bytes(&self) -> Vec<u8> {
        self.buf.lock().expect("pipe tap poisoned").clone()
    }

    /// Lossy-UTF8 view of [`PipeTap::bytes`], with the same caveat.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    /// Stop the child, JOIN the drain thread, and return everything it wrote.
    ///
    /// Termination is proved, not bounded: the drain loop exits only at EOF, EOF
    /// arrives when the child's write end closes, and the child is reaped here first —
    /// so the join cannot hang on a live writer. A clone calling `finish` concurrently
    /// blocks on the drain slot and then observes a fully drained buffer.
    #[must_use]
    pub fn finish(self, child: &mut ChildGuard) -> Vec<u8> {
        child.kill_and_wait();
        {
            let mut slot = self.drain.lock().expect("pipe tap drain slot poisoned");
            if let Some(handle) = slot.take() {
                handle.join().expect("pipe drain thread panicked");
            }
        }
        self.bytes()
    }

    /// Lossy-UTF8 view of [`PipeTap::finish`].
    #[must_use]
    pub fn finish_text(self, child: &mut ChildGuard) -> String {
        String::from_utf8_lossy(&self.finish(child)).into_owned()
    }
}

/// Spawn a background thread that drains `reader` into a fresh [`PipeTap`].
fn tap_reader<R: Read + Send + 'static>(reader: R) -> PipeTap {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = buf.clone();
    let handle = std::thread::spawn(move || {
        let mut reader = reader;
        let mut chunk = [0u8; 512];
        // Bounded by EOF: the loop ends when the child's pipe closes.
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink
                    .lock()
                    .expect("pipe tap poisoned")
                    .extend_from_slice(&chunk[..n]),
            }
        }
    });
    PipeTap {
        buf,
        drain: Arc::new(Mutex::new(Some(handle))),
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
/// block the child. If the caller also piped stdout, that pipe is drained the same
/// way and the tap is returned as the third element; `Command` inherits stdout by
/// default, so `child.stdout.is_some()` is exactly "the caller asked for a pipe".
///
/// Draining stdout here rather than in the caller is what keeps the readiness wait
/// sound: `mds watch -o -` publishes its startup output before it writes the marker,
/// so an undrained stdout pipe fills and blocks the child while the poller waits for
/// a marker that can never be written.
#[allow(dead_code)]
pub fn spawn_watch_unsynchronized(cmd: &mut Command) -> (Child, StderrTap, Option<StdoutTap>) {
    let mut child = cmd
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mds watch");

    let tap = tap_reader(child.stderr.take().expect("stderr must be piped"));
    let stdout_tap = child.stdout.take().map(tap_reader);

    (child, tap, stdout_tap)
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
/// A piped stdout is drained too, and its tap handed back as the third element. That
/// ordering is load-bearing, not a convenience: `mds watch -o -` publishes its startup
/// output before it writes the marker, so leaving stdout undrained would let the child
/// block on a full pipe while this function waits for a marker that can never arrive.
///
/// # Panics
/// Panics if the child cannot be spawned, or if readiness is not signalled within
/// [`READY_TIMEOUT`] — a watcher that never reports readiness is a defect, not a
/// slow machine.
#[allow(dead_code)]
pub fn spawn_watch_ready(cmd: &mut Command) -> (Child, StderrTap, Option<StdoutTap>) {
    // A private directory per spawn: the suite runs at full parallelism, so a shared
    // path would let one watcher's marker satisfy another's wait. Dropped — and so
    // deleted — when this function returns, by which point the marker has been read.
    let ready_dir = tempfile::tempdir().expect("failed to create readiness tempdir");
    let ready_path = ready_dir.path().join("watch-ready");
    assert!(
        ready_path.is_absolute(),
        "MDS_TEST_READY must be absolute; mds watch ignores relative values"
    );

    let (mut child, tap, stdout_tap) =
        spawn_watch_unsynchronized(cmd.env("MDS_TEST_READY", &ready_path));

    // Bounded by READY_TIMEOUT: at most READY_TIMEOUT / READY_POLL iterations.
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        if std::fs::read(&ready_path).is_ok_and(|b| b == READY_MARKER.as_bytes()) {
            return (child, tap, stdout_tap);
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
