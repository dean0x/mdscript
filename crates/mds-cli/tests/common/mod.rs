use std::io::Read;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// The write end of a pipe whose read end has already been dropped.
///
/// Handing this to a child as one of its standard streams makes every write the child
/// makes to that stream fail with a broken pipe from the first byte on. The reader is
/// dropped BEFORE the child is spawned: dropping `child.stdout` / `child.stderr` after
/// the spawn instead races the child, since a short run can finish writing first.
#[allow(dead_code)]
pub fn closed_pipe() -> std::io::PipeWriter {
    let (reader, writer) = std::io::pipe().expect("create a pipe");
    drop(reader);
    writer
}

/// Creates a symlink for a test, tolerating Windows' unprivileged restriction.
///
/// Unix symlink creation needs no special privilege. On Windows it needs either
/// Developer Mode or `SeCreateSymbolicLinkPrivilege` (an elevated process) —
/// GitHub's `windows-latest` runners have Developer Mode enabled, so a failure
/// there is a genuine regression and must panic. Locally, without that
/// privilege, the OS reports `ERROR_PRIVILEGE_NOT_HELD` (raw error 1314); this
/// helper treats exactly that failure as a skip (never a false pass) when the
/// `CI` env var is unset, printing a one-line reason. Returns `false` when the
/// caller should skip the rest of the test.
///
/// Windows makes a link to a directory and a link to a file differently, so the
/// target is looked at first — a relative one from the link's directory, where the
/// link will resolve it, not from the working directory. A target that is not there
/// gets a file link.
///
/// Mirrors `crates/mds-core/src/lib.rs`'s crate-internal helper of the same
/// name and contract (#147); duplicated rather than shared because
/// `mds-core`'s helper is `pub(crate)` to that crate and each `mds-cli`
/// integration test file compiles `tests/common/mod.rs` as its own module.
#[allow(dead_code)]
pub fn make_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = {
        let resolved = link
            .parent()
            .map_or_else(|| target.to_path_buf(), |dir| dir.join(target));
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    };

    match result {
        Ok(()) => true,
        Err(err) => {
            #[cfg(windows)]
            {
                const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
                if err.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
                    && std::env::var_os("CI").is_none()
                {
                    eprintln!(
                        "skipping: symlink creation needs Developer Mode or an elevated process on Windows"
                    );
                    return false;
                }
            }
            panic!(
                "failed to create symlink {} -> {}: {err}",
                target.display(),
                link.display()
            );
        }
    }
}

/// Removes the symlink at `link` — the link, never what it points to.
///
/// Windows keeps a link to a directory as a directory entry, which `remove_dir` removes,
/// not `remove_file`; every other link, and every link on unix, is removed with
/// `remove_file`.
#[allow(dead_code)]
pub fn remove_symlink(link: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt as _;

        let linked = std::fs::symlink_metadata(link)
            .unwrap_or_else(|e| panic!("look at the link {}: {e}", link.display()));
        if linked.file_type().is_symlink_dir() {
            std::fs::remove_dir(link)
                .unwrap_or_else(|e| panic!("remove the link {}: {e}", link.display()));
            return;
        }
    }
    std::fs::remove_file(link)
        .unwrap_or_else(|e| panic!("remove the link {}: {e}", link.display()));
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
/// Same body as the private `count_occurrences` in `warnings.rs` — that file imports
/// only the two render helpers above (E0255 otherwise) and keeps its own private copy
/// of this one.
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
/// On Windows, a rename over a file another process holds open succeeds when that
/// process opened it as std does, sharing deletion (`FILE_SHARE_DELETE`): std's `rename`
/// falls back to a POSIX-semantics replace. A handle opened without that share flag — a
/// virus scanner's, say — makes it fail with a sharing violation. The Windows CI job
/// (`Rust — clippy, test (windows-latest)`) runs `cli_watch`, `cli_watch_cap` (hundreds of
/// rename-overs in a run) and `cli_watch_truncate` through this helper, so a
/// `cannot rename` panic there is a failure to triage, not one that cannot happen.
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
    #[track_caller]
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
///
/// The spawn helpers below call it with a child's pipe; the wait self-tests in
/// `cli_watch.rs` call it with an in-memory reader, so they exercise the waits without
/// a process.
#[allow(dead_code)]
pub fn tap_reader<R: Read + Send + 'static>(reader: R) -> PipeTap {
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

// ── Pipe-tap waits ───────────────────────────────────────────────────────────

/// How often the pipe-tap waits sample their tap.
const TAP_POLL: Duration = Duration::from_millis(20);

/// Poll `tap` until `done` holds for its text, or `timeout` elapses. Never panics.
///
/// `Ok` carries the text that satisfied `done`; `Err` carries the last text seen when
/// the time ran out. This is for a caller that treats an unmet condition as data
/// rather than as a failure; a wait whose timeout is a failure goes through
/// [`wait_for_tap`] or [`wait_for_tap_count`], which fail at the caller.
#[allow(dead_code)]
pub fn poll_tap_until(
    tap: &PipeTap,
    timeout: Duration,
    done: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    // Bounded by `timeout`: at most timeout / TAP_POLL iterations, and the text is
    // tested once more after the deadline passes, so a condition met on the last
    // sample still counts.
    loop {
        let text = tap.text();
        if done(&text) {
            return Ok(text);
        }
        if Instant::now() >= deadline {
            return Err(text);
        }
        std::thread::sleep(TAP_POLL);
    }
}

/// Wait until `tap` holds `needle`, and return everything it holds at that moment.
///
/// PANICS on timeout. The message opens with the CALLER's `file:line:column` and ends
/// with what the tap actually held, so a missing line is reported where the test waited
/// for it, as the precondition that never happened — never as a later assertion about
/// text that was simply incomplete. A wait that returned the text on timeout let the
/// caller's own assertion report the shortfall as if it were a final answer.
///
/// A tap samples a live pipe: text returned here is complete only up to `needle`.
/// A count over it needs an ordered anchor — a line the child writes AFTER everything
/// being counted — or [`PipeTap::finish_text`] once such an anchor has been seen.
#[track_caller]
#[allow(dead_code)]
pub fn wait_for_tap(tap: &PipeTap, needle: &str, timeout: Duration) -> String {
    match poll_tap_until(tap, timeout, |text| text.contains(needle)) {
        Ok(text) => text,
        Err(seen) => panic!(
            "wait_for_tap at {}: {needle:?} did not appear within {timeout:?}; \
             the tap held:\n{seen}",
            Location::caller()
        ),
    }
}

/// Wait until `tap` holds at least `n` occurrences of `needle`, and return everything
/// it holds at that moment.
///
/// PANICS on timeout, naming the caller's `file:line:column` and the count it saw.
///
/// Why a count and not "contains": a stderr line the watcher emits AFTER the output
/// write has no ordering relationship with the output file the test waited on.
/// Dir-mode emits the duplicate-vars-key warning after the write (watch.rs
/// `handle_fs_event_dir`), so a snapshot taken the instant `wait_for_file_contains`
/// returns can legitimately be one warning short — or, if the previous rebuild's
/// warning has not been sampled yet, one long. Waiting for the expected count first
/// turns the assertion that follows into a genuine over-count check instead of a race.
#[track_caller]
#[allow(dead_code)]
pub fn wait_for_tap_count(tap: &PipeTap, needle: &str, n: usize, timeout: Duration) -> String {
    match poll_tap_until(tap, timeout, |text| count_occurrences(text, needle) >= n) {
        Ok(text) => text,
        Err(seen) => panic!(
            "wait_for_tap_count at {}: expected at least {n} occurrences of {needle:?} \
             within {timeout:?}; saw {}; the tap held:\n{seen}",
            Location::caller(),
            count_occurrences(&seen, needle)
        ),
    }
}

/// A source whose compile always fails with one `mds::undefined_var` diagnostic
/// carrying [`ORDER_MARKER_LINE`]. Diagnostics survive `--quiet`.
///
/// Writing it to a watched source makes an ORDERED ANCHOR on stderr. The watch loop
/// handles one event at a time and finishes a rebuild — its `Recompiled` line, its
/// post-write warnings — before it takes the next event, so once the marker's
/// diagnostic is on the tap, everything earlier rebuilds wrote is on it too. A count
/// or an absence taken at that point is exact rather than a sample of a live pipe.
/// The failed compile writes no output, so the output file keeps what it held.
#[allow(dead_code)]
pub const ORDER_MARKER_SOURCE: &str = "Order marker {{__order_marker__}}\n";

/// The line of the diagnostic [`ORDER_MARKER_SOURCE`] produces, once per compile.
///
/// One edit can reach the watcher as several events, and each failed compile reports
/// again, so this line can print several times for a single edit: wait for it, never
/// count it.
#[allow(dead_code)]
pub const ORDER_MARKER_LINE: &str = "undefined variable '__order_marker__'";

// ── Write cadence and run flags (#397) ───────────────────────────────────────

/// When each write of a test's writer began and when it completed.
///
/// A watcher sees a write's events at some instant between the two, so the longest
/// pause it can have seen between two successive writes runs from the START of the one
/// to the END of the next ([`WriteCadence::gaps`]). A claim that depends on how often a
/// test managed to write is judged on these MEASURED gaps, never on the cadence the
/// test asked for: a loaded runner can deschedule a writer for longer than any window
/// under test.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct WriteCadence {
    /// `(began, completed)` for each write, in order.
    writes: Vec<(Instant, Instant)>,
}

#[allow(dead_code)]
impl WriteCadence {
    /// An empty record with room for `writes` writes.
    pub fn with_capacity(writes: usize) -> Self {
        Self {
            writes: Vec::with_capacity(writes),
        }
    }

    /// A record of writes timed elsewhere, as `(began, completed)` in order.
    ///
    /// # Panics
    /// Panics if a write completes before it begins, or begins before the one before it
    /// completed: one writer writes one file at a time.
    pub fn of(writes: Vec<(Instant, Instant)>) -> Self {
        for (i, &(began, completed)) in writes.iter().enumerate() {
            assert!(began <= completed, "write {i} completes before it begins");
            if let Some(&(_, previous)) = i.checked_sub(1).and_then(|p| writes.get(p)) {
                assert!(
                    previous <= began,
                    "write {i} begins before the write before it completed"
                );
            }
        }
        Self { writes }
    }

    /// Run `write`, recording when it began and when it completed.
    pub fn time<T>(&mut self, write: impl FnOnce() -> T) -> T {
        let began = Instant::now();
        let done = write();
        self.writes.push((began, Instant::now()));
        done
    }

    /// How many writes were recorded.
    pub fn writes(&self) -> usize {
        self.writes.len()
    }

    /// The first `n` writes as a record of their own — all of them when fewer were
    /// recorded — to judge what the writer had done by a given write.
    pub fn first(&self, n: usize) -> Self {
        Self {
            writes: self.writes.iter().take(n).copied().collect(),
        }
    }

    /// From the start of the first write to the end of the last: the longest the
    /// writes' events can have been spread over. Zero with no write.
    pub fn span(&self) -> Duration {
        match (self.writes.first(), self.writes.last()) {
            (Some(&(began, _)), Some(&(_, completed))) => completed.duration_since(began),
            _ => Duration::ZERO,
        }
    }

    /// For each two successive writes, the longest pause a watcher can have seen
    /// between their events: from the start of the first to the end of the second.
    pub fn gaps(&self) -> Vec<Duration> {
        self.writes
            .windows(2)
            .map(|pair| pair[1].1.duration_since(pair[0].0))
            .collect()
    }

    /// How many [`Self::gaps`] are `at_least` long.
    pub fn pauses(&self, at_least: Duration) -> usize {
        self.gaps()
            .into_iter()
            .filter(|&gap| gap >= at_least)
            .count()
    }
}

impl std::fmt::Display for WriteCadence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut gaps = self.gaps();
        gaps.sort_unstable();
        let rank = |per_mille: usize| -> Duration {
            gaps.get(gaps.len().saturating_sub(1) * per_mille / 1000)
                .copied()
                .unwrap_or_default()
        };
        write!(
            f,
            "{} writes over {:?}; gaps: max {:?}, median {:?}, p99 {:?}",
            self.writes.len(),
            self.span(),
            rank(1000),
            rank(500),
            rank(990)
        )
    }
}

/// The most debounce windows that can have closed over a stream with this `cadence`,
/// under a quiet period of `window` that a cap ends at most `cap` after it opened.
///
/// It holds while the watcher drains each write's events within half a window of the
/// write:
/// - A window the cap closed lasted the whole cap, and its last event came no earlier
///   than one window before the cap's end. Such windows do not overlap, and they lie
///   between the first write and one and a half windows after the last, so at most
///   `(span + window + window / 2) / cap` of them close at the cap.
/// - A window closes quietly only after a whole window without a drained event. With
///   each drain at most half a window late, that takes a measured gap of at least half
///   a window ([`WriteCadence::pauses`]) — or the end of the stream: one more.
///
/// A window can also close at the watcher's message limit, which no cadence measures; a
/// caller that cannot rule that out adds an allowance of its own.
///
/// # Panics
/// Panics unless `window` is nonzero and `cap` is at least `window`.
#[allow(dead_code)]
pub fn most_debounce_windows(cadence: &WriteCadence, window: Duration, cap: Duration) -> usize {
    assert!(
        !window.is_zero() && cap >= window,
        "a debounce cap is at least its nonzero window"
    );
    let reach = cadence.span() + window + window / 2;
    let cap_closes = usize::try_from(reach.as_nanos() / cap.as_nanos()).unwrap_or(usize::MAX);
    cap_closes
        .saturating_add(cadence.pauses(window / 2))
        .saturating_add(1)
}

/// Env var naming a file that a test judging its own run appends one line to.
///
/// A claim that depends on the runner's cadence is judged only on a run whose measured
/// cadence can decide it; any other run passes INCONCLUSIVE — it neither fails on the
/// runner's speed nor skips. The watch soak workflow sets this per iteration and counts
/// conclusive and inconclusive iterations apart from passed, failed and skipped ones.
#[allow(dead_code)]
pub const RUN_FLAG_LOG_ENV: &str = "MDS_TEST_FLAG_LOG";

/// Whether a run could decide the claim it flags.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFlag {
    /// The measured cadence let the run judge the claim, and it did.
    Conclusive,
    /// The measured cadence left the claim undecided; the run passed without judging it.
    Inconclusive,
}

impl std::fmt::Display for RunFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RunFlag::Conclusive => "conclusive",
            RunFlag::Inconclusive => "inconclusive",
        })
    }
}

/// The one line that flags a run of `test`: `<test>: <flag>: <details>`. The watch
/// soak counts the lines holding `: conclusive: ` and those holding `: inconclusive: `.
///
/// # Panics
/// Panics if `test` or `details` holds a line break: a flag is one line.
#[allow(dead_code)]
pub fn run_flag_line(test: &str, flag: RunFlag, details: &str) -> String {
    assert!(
        !test.contains(['\n', '\r']) && !details.contains(['\n', '\r']),
        "a run flag is one line"
    );
    format!("{test}: {flag}: {details}")
}

/// Append `line` and a newline to the file at `path`, creating it if it is missing.
///
/// Line and newline go out in ONE write: the tests of a binary run at once and append to
/// the same log, and an appending write lands whole at the end, while two writes per line
/// let another writer's line fall between a line and its newline.
#[allow(dead_code)]
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    file.write_all(format!("{line}\n").as_bytes())
}

/// Report whether this run of `test` was conclusive, where a passing run cannot hide
/// it: on stderr, and as one line appended to the file [`RUN_FLAG_LOG_ENV`] names.
/// Without that file under GitHub Actions — a CI run, not a soak that counts the lines
/// itself — an inconclusive run also adds its line to the job summary.
///
/// # Panics
/// Panics if [`RUN_FLAG_LOG_ENV`] is set and the line cannot be appended: whoever set
/// it is counting flags, and a flag it cannot see would read as a run that never
/// reported one.
#[allow(dead_code)]
pub fn record_run_flag(test: &str, flag: RunFlag, details: &str) {
    let line = run_flag_line(test, flag, details);
    eprintln!("{line}");
    if let Some(path) = std::env::var_os(RUN_FLAG_LOG_ENV) {
        let path = PathBuf::from(path);
        if let Err(e) = append_line(&path, &line) {
            panic!(
                "{RUN_FLAG_LOG_ENV}={}: cannot record the run's flag: {e}",
                path.display()
            );
        }
    } else if flag == RunFlag::Inconclusive {
        // Best effort: the job summary is a convenience; a soak's flag log is the record.
        if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
            let _ = append_line(Path::new(&summary), &format!(":information_source: {line}"));
        }
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
    let ready = ReadyFile::new();
    let (mut child, tap, stdout_tap) =
        spawn_watch_unsynchronized(cmd.env("MDS_TEST_READY", ready.path()));
    ready.wait(&mut child, || tap.text());
    (child, tap, stdout_tap)
}

/// [`spawn_watch_ready`] for a command whose stderr the caller has set to something
/// that needs no draining — a closed pipe, a file — which is left alone here.
///
/// A piped stdout is still drained, and its tap returned, as [`spawn_watch_ready`]
/// does. With nothing tapping stderr, a watcher that ends at startup is reported by its
/// exit status alone.
#[allow(dead_code)]
pub fn spawn_watch_ready_stderr_untapped(cmd: &mut Command) -> (Child, Option<StdoutTap>) {
    let ready = ReadyFile::new();
    let mut child = cmd
        .env("MDS_TEST_READY", ready.path())
        .spawn()
        .expect("failed to spawn mds watch");
    let stdout_tap = child.stdout.take().map(tap_reader);
    ready.wait(&mut child, || "(stderr is not tapped)".to_string());
    (child, stdout_tap)
}

/// [`spawn_watch_ready`] with the readiness file at `marker`, a path the caller chose —
/// so it can plant something at it, or at the temporary path beside it, first. `marker`
/// must be absolute and in a directory that test owns.
#[allow(dead_code)]
pub fn spawn_watch_ready_at(
    cmd: &mut Command,
    marker: &Path,
) -> (Child, StderrTap, Option<StdoutTap>) {
    assert!(
        marker.is_absolute(),
        "MDS_TEST_READY must be absolute; mds watch ignores relative values"
    );
    let (mut child, tap, stdout_tap) =
        spawn_watch_unsynchronized(cmd.env("MDS_TEST_READY", marker));
    wait_for_ready(marker, &mut child, || tap.text());
    (child, tap, stdout_tap)
}

/// The file one spawned watcher creates when it is live (`MDS_TEST_READY`).
struct ReadyFile {
    /// A private directory per spawn: the suite runs at full parallelism, so a shared
    /// path would let one watcher's marker satisfy another's wait. Dropped — and so
    /// deleted — with this value, by which point the marker has been read.
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl ReadyFile {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("failed to create readiness tempdir");
        let path = dir.path().join("watch-ready");
        assert!(
            path.is_absolute(),
            "MDS_TEST_READY must be absolute; mds watch ignores relative values"
        );
        Self { _dir: dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Block until `child` has created the marker; see [`wait_for_ready`].
    fn wait(&self, child: &mut Child, stderr: impl Fn() -> String) {
        wait_for_ready(&self.path, child, stderr);
    }
}

/// Block until `child` has created the readiness file at `marker`. `stderr` reports what
/// the child has printed so far, for the panic messages.
///
/// # Panics
/// Panics if the child exits first, or if [`READY_TIMEOUT`] passes (the child is killed
/// and reaped first).
fn wait_for_ready(marker: &Path, child: &mut Child, stderr: impl Fn() -> String) {
    // Bounded by READY_TIMEOUT: at most READY_TIMEOUT / READY_POLL iterations.
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        if std::fs::read(marker).is_ok_and(|b| b == READY_MARKER.as_bytes()) {
            return;
        }
        // Check liveness before the deadline so a watcher that failed at startup is
        // reported as "exited", not as "timed out".
        if let Ok(Some(status)) = child.try_wait() {
            let seen = stderr();
            panic!(
                "mds watch exited with {status:?} before signalling readiness; \
                 stderr was:\n{seen}"
            );
        }
        if std::time::Instant::now() >= deadline {
            let seen = stderr();
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

// ── A stream that fails other than by a closed pipe (#157) ───────────────────

/// Make the child `cmd` spawns unable to grow a file past `limit` bytes: its file-size
/// limit is `limit` and it ignores SIGXFSZ, so a write past the limit fails with "file
/// too large" (EFBIG) instead of killing the child.
///
/// A stream on a regular file the child may not grow then fails every write for a
/// reason other than a closed pipe — on every unix, where `/dev/full` is Linux only.
/// Pipes are not files, so the limit leaves a piped stream alone; every file the child
/// writes itself (outputs, the readiness marker) is limited too.
#[cfg(unix)]
#[allow(dead_code)]
pub fn limit_file_growth(cmd: &mut Command, limit: libc::rlim_t) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY: the closure runs in the forked child just before `exec`, where only
    // async-signal-safe work is sound: `signal` is on POSIX's async-signal-safe list, and
    // `setrlimit` is a thin wrapper around its system call that takes no lock and
    // allocates nothing. The closure touches none of the parent's state.
    unsafe {
        cmd.pre_exec(move || {
            if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            let growth = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &growth) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// A regular file of exactly `len` bytes, open for writing at its end, to hand to a
/// child as a stream: under [`limit_file_growth`] with `len` as the limit, every write
/// the child makes to it fails with "file too large".
///
/// Not opened for appending: macOS checks the limit against the descriptor's offset,
/// and an appending descriptor sits at 0 until its first write, so a write shorter than
/// the limit would get through. The offset is shared with every copy of the descriptor
/// — the child's stream and a `try_clone` the test keeps — so the test makes room again
/// by shortening the file and moving that clone's offset back (#157).
#[cfg(unix)]
#[allow(dead_code)]
pub fn full_file(path: &Path, len: usize) -> std::fs::File {
    use std::io::{Seek as _, SeekFrom};

    std::fs::write(path, vec![b'#'; len]).expect("fill the stream file");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open the stream file");
    file.seek(SeekFrom::End(0))
        .expect("move to the end of the stream file");
    file
}

// ── SIGINT's default action in a child (#381) ────────────────────────────────

/// Give the child `cmd` spawns SIGINT's default action, terminate, whatever this
/// process's own disposition is.
///
/// A child inherits an ignored signal across `exec`, and `std::process::Command` resets
/// SIGPIPE alone: a test run started as a background job of a non-interactive shell has
/// SIGINT ignored, and so would every child it spawns. A test that sends SIGINT to `mds
/// watch` before its Ctrl+C handler is installed asserts the default action, so without
/// this the signal is discarded and the test waits for an exit that never comes.
///
/// Unix-only: a signal disposition is a unix notion, and Windows has no SIGINT a test can
/// send one child.
#[cfg(unix)]
#[allow(dead_code)]
pub fn default_sigint(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY: the closure runs in the forked child just before `exec`, where only
    // async-signal-safe work is sound: `signal` is on POSIX's async-signal-safe list. The
    // closure touches none of the parent's state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::signal(libc::SIGINT, libc::SIG_DFL) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
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
