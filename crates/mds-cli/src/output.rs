//! Shared output-path machinery for build, check, watch, fmt, and lint subcommands.
//!
//! # What lives here
//!
//! - [`OutputBase`] / [`resolve_output_base`] / [`output_path_for`]: directory-mode
//!   path resolution used by watch and build-directory.
//! - [`collect_mds_files`] / [`is_partial`]: directory traversal helpers.
//! - [`probe_and_remove_stale`]: stale-output cleanup for format-flip (AC-FUNC-23).
//! - `ewrite!` / `ewriteln!` over [`write_stderr_fmt`]: the CLI's stderr choke point,
//!   which never panics — a closed pipe or a failed write becomes sticky [`OutputState`]
//!   instead (#157). [`write_stdout`] writes a command's product and reports a
//!   [`StdoutOutcome`], writing nothing once stdout's reader is gone; [`exit`] ends the
//!   process through [`final_exit_code`].
//! - [`install_panic_hook`] / [`catch_panic`]: a panic prints one fixed
//!   internal-compiler-error text — never the panic's message or location — and the run
//!   exits 101 (#389).
//! - [`eprint_error`]: the CLI's error-report choke point — escapes every report's
//!   message, help, and label text before miette renders it (CWE-150), then writes the
//!   frame through `ewriteln!`.
//! - [`atomic_write_file`]: temp-file-then-rename writer shared by `fmt` and `lint --fix`,
//!   and — since #227 — by every `build` / `watch` output and `.map` sidecar. The
//!   [`Durability`] argument says whether the bytes are fsynced before the rename;
//!   atomicity does not depend on it.
//! - [`preview_text_for`]: `--diff` preview output — neutralized on TTY, byte-faithful
//!   when piped, so redirected diffs stay applicable by `patch`/tooling.
//!
//! Single-file path helpers (`OutputKind`, `compile_to_content`,
//! `resolve_output_path_for_kind`) remain in `build.rs`; they are imported here when
//! callers need both single-file and directory logic.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use miette::Result;

use crate::build::{MdsConfig, OutputKind};

// ── Streams and the exit funnel (#157) ───────────────────────────────────────

/// `eprint!` for the CLI: write to stderr through [`write_stderr_fmt`], which never
/// panics.
///
/// `tests/print_discipline.rs` lists this macro and `ewriteln!` beside the std print
/// macros and scans their arguments the same way. A stderr writer the guard does not
/// list would take every site that uses it out of the guard.
macro_rules! ewrite {
    ($($arg:tt)*) => {
        $crate::output::write_stderr_fmt(::std::format_args!($($arg)*))
    };
}

/// `eprintln!` for the CLI: `ewrite!` plus a trailing newline.
macro_rules! ewriteln {
    () => {
        $crate::output::write_stderr_fmt(::std::format_args!("\n"))
    };
    ($($arg:tt)+) => {
        $crate::output::write_stderr_fmt(::std::format_args!(
            "{}\n",
            ::std::format_args!($($arg)+)
        ))
    };
}

pub(crate) use ewrite;
pub(crate) use ewriteln;

/// Sticky facts about the CLI's output streams, read when the process exits.
///
/// Each fact is one bit, set with `fetch_or`, so a fact recorded on any thread is still
/// there at exit. Only `STDOUT_FAILED` is ever cleared: it marks stdout's current
/// failure, which a write that lands ends. No fact the exit code reads is cleared.
/// Functions that decide something from these facts take `&OutputState`, so a unit test
/// builds its own instead of sharing the process's [`OUTPUT_STATE`] with every other
/// test in the binary.
///
/// Adding a fact is adding one bit and its accessors.
pub(crate) struct OutputState {
    bits: AtomicU8,
}

impl OutputState {
    /// A stderr write hit a closed pipe: the reader is gone.
    const STDERR_CLOSED: u8 = 1 << 0;
    /// An output operation — a write, a flush, a directory creation, a delete — failed
    /// for any reason other than a closed pipe.
    const IO_FAILED: u8 = 1 << 1;
    /// A stdout write hit a closed pipe: the reader is gone.
    const STDOUT_CLOSED: u8 = 1 << 2;
    /// A stdout write failed for a reason other than a closed pipe, and no write has
    /// landed since. Cleared by a write that lands, so a later failure is a new one.
    const STDOUT_FAILED: u8 = 1 << 3;
    /// A `mds watch` session went live: from then on its output failures do not change
    /// the exit code ([`ExitPolicy::WatchSession`]).
    const WATCH_LIVE: u8 = 1 << 4;
    /// A panic happened (#389): the run exits 101, whatever else it recorded.
    const PANICKED: u8 = 1 << 5;

    pub(crate) const fn new() -> Self {
        Self {
            bits: AtomicU8::new(0),
        }
    }

    /// Set `bit`; `true` when this call is the one that set it.
    fn set(&self, bit: u8) -> bool {
        self.bits.fetch_or(bit, Ordering::AcqRel) & bit == 0
    }

    fn clear(&self, bit: u8) {
        self.bits.fetch_and(!bit, Ordering::AcqRel);
    }

    fn has(&self, bit: u8) -> bool {
        self.bits.load(Ordering::Acquire) & bit != 0
    }

    /// Record that stderr's reader is gone. Later stderr writes are dropped.
    pub(crate) fn note_stderr_closed(&self) {
        self.set(Self::STDERR_CLOSED);
    }

    /// Record an output operation that failed for a reason other than a closed pipe.
    pub(crate) fn note_io_failure(&self) {
        self.set(Self::IO_FAILED);
    }

    /// Record that stdout's reader is gone. Later stdout writes are dropped.
    pub(crate) fn note_stdout_closed(&self) {
        self.set(Self::STDOUT_CLOSED);
    }

    /// Record a stdout write that failed for a reason other than a closed pipe; `true`
    /// for the first failed write since the run began or since a write last landed —
    /// the one that is reported.
    pub(crate) fn note_stdout_failure(&self) -> bool {
        self.set(Self::STDOUT_FAILED)
    }

    /// Record a stdout write that landed: stdout's current failure, if any, is over, so
    /// the next failure is reported again. The I/O failure recorded for the exit code
    /// stays recorded.
    pub(crate) fn note_stdout_written(&self) {
        self.clear(Self::STDOUT_FAILED);
    }

    pub(crate) fn stderr_closed(&self) -> bool {
        self.has(Self::STDERR_CLOSED)
    }

    pub(crate) fn io_failed(&self) -> bool {
        self.has(Self::IO_FAILED)
    }

    pub(crate) fn stdout_closed(&self) -> bool {
        self.has(Self::STDOUT_CLOSED)
    }

    /// Record that a `mds watch` session went live.
    pub(crate) fn note_watch_live(&self) {
        self.set(Self::WATCH_LIVE);
    }

    /// Record that a panic happened (#389).
    pub(crate) fn note_panicked(&self) {
        self.set(Self::PANICKED);
    }

    pub(crate) fn panicked(&self) -> bool {
        self.has(Self::PANICKED)
    }

    /// The rule [`final_exit_code`] applies when the process exits:
    /// [`ExitPolicy::WatchSession`] once a watch session went live,
    /// [`ExitPolicy::Batch`] for every other run.
    pub(crate) fn exit_policy(&self) -> ExitPolicy {
        if self.has(Self::WATCH_LIVE) {
            ExitPolicy::WatchSession
        } else {
            ExitPolicy::Batch
        }
    }
}

/// The process's own [`OutputState`], used by the process-boundary functions
/// [`write_stderr_fmt`], [`write_stdout`], [`note_io_failure`],
/// [`note_watch_session_live`], [`exit`] and the panic hook, [`on_panic`].
static OUTPUT_STATE: OutputState = OutputState::new();

/// Record, for the exit code, that an output operation of this run failed for a reason
/// other than a closed pipe: [`exit`] then ends the run with at least 2 (#157).
///
/// For a run that reports a failure and carries on — a directory build or `mds fmt
/// <dir>` counting the file as failed — and so never returns the error to `main`. A run
/// that returns the `mds::io` error reaches the same exit code through `exit_code`
/// instead. `mds watch` never calls it: a rebuild's failure is reported as it happens
/// and does not change how the session exits ([`note_watch_session_live`]).
pub(crate) fn note_io_failure() {
    OUTPUT_STATE.note_io_failure();
}

/// Record that this `mds watch` session is live — every watch armed, every baseline
/// captured (#157). From here on [`exit`] applies [`ExitPolicy::WatchSession`]: a
/// rebuild's output failure, reported as it happened, and a stderr that fails other
/// than by a closed pipe no longer change the exit code. Until then — a session that
/// ends at startup — the batch rule applies.
pub(crate) fn note_watch_session_live() {
    OUTPUT_STATE.note_watch_live();
}

/// Report an I/O failure of a run that carries on past it — a directory build, `mds fmt
/// <dir>` — as one `mds::io` error, and record it for the exit code (#157).
pub(crate) fn eprint_io_failure(e: mds::MdsError) {
    note_io_failure();
    eprint_error(miette::Report::new(e));
}

/// Report why one file of a directory build, check or fmt failed, and carry on.
///
/// A failure in the I/O and file-system class — exit 2 for the same file given alone:
/// `mds::io` (a source that cannot be read, a forbidden path character),
/// `mds::file_not_found`, `mds::not_mds` — is recorded for the exit code, so the run
/// exits 2 as that file alone would (#157). Any other failure, a template error or a
/// resource limit, leaves the exit code to the caller's count of failed files (1).
pub(crate) fn eprint_file_failure(report: miette::Report) {
    if crate::build::exit_code(&report) == IO_FAILURE_EXIT {
        note_io_failure();
    }
    eprint_error(report);
}

/// The body of `ewrite!` / `ewriteln!`: write `args` to stderr, never panicking.
///
/// std's `eprintln!` panics when the write fails, which ends the run with exit 101.
/// Here instead:
///
/// - A closed pipe (the reader is gone) records [`OutputState::note_stderr_closed`];
///   this write and every later stderr write are dropped, and the exit code does not
///   change.
/// - Any other write error records [`OutputState::note_io_failure`], which
///   [`final_exit_code`] turns into an exit of at least 2. Later writes are still
///   attempted.
pub(crate) fn write_stderr_fmt(args: std::fmt::Arguments<'_>) {
    write_stderr_to(&OUTPUT_STATE, &mut std::io::stderr().lock(), args);
}

/// [`write_stderr_fmt`] against any sink and state.
///
/// The text is rendered first and written with one `write_all`, then flushed. A
/// `Display` that fails ends the text where it failed: that is a bug in the impl, not an
/// output failure, so it records nothing.
fn write_stderr_to<W: std::io::Write + ?Sized>(
    state: &OutputState,
    sink: &mut W,
    args: std::fmt::Arguments<'_>,
) {
    if state.stderr_closed() {
        return;
    }
    let mut text = String::new();
    let _ = std::fmt::Write::write_fmt(&mut text, args);
    match sink.write_all(text.as_bytes()).and_then(|()| sink.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => state.note_stderr_closed(),
        Err(_) => state.note_io_failure(),
    }
}

/// What happened to one [`write_stdout`] call.
#[must_use]
#[derive(Debug)]
pub(crate) enum StdoutOutcome {
    /// Every byte was written and flushed.
    Written,
    /// The reader is gone — a closed pipe, on this write or an earlier one. Nothing more
    /// is written to stdout, and the verdict is kept.
    Closed,
    /// A new stdout failure for any other reason — the first of the run, or the first
    /// since a write last landed: the caller reports it as `mds::io`.
    Failed(std::io::Error),
    /// Another failure after [`StdoutOutcome::Failed`] was returned, with no write
    /// landing in between: nothing was written, and the failure is not reported a
    /// second time.
    FailedAgain,
}

impl StdoutOutcome {
    /// What a run that ends on its own — build, fmt — makes of the outcome.
    ///
    /// A closed pipe is not an error: the reader is gone, so stdout gets nothing more
    /// and the run keeps its verdict. A new failure for another reason is `mds::io`,
    /// naming stdout; a repeat of it was already reported (#157). Nothing is recorded
    /// here; an `Err` reaches the exit code through the caller.
    ///
    /// `mds watch -o -` reads the outcome itself instead: a closed pipe ends its session,
    /// and neither it nor a repeated failure is a write it may remember as done.
    pub(crate) fn into_batch_result(self) -> std::result::Result<(), mds::MdsError> {
        match self {
            Self::Written | Self::Closed | Self::FailedAgain => Ok(()),
            Self::Failed(e) => Err(stdout_failure(&e)),
        }
    }
}

/// The `mds::io` error for a stdout write that failed for a reason other than a closed
/// pipe (#157).
pub(crate) fn stdout_failure(e: &std::io::Error) -> mds::MdsError {
    mds::MdsError::Io {
        message: format!("cannot write to stdout: {}", safe_inline(e)),
    }
}

/// Write a command's product — compiled output, a diff, a JSON report — to stdout and
/// flush it. [`StdoutOutcome::Written`] only when both the write and the flush succeed.
///
/// Once stdout's reader is gone, every later call returns [`StdoutOutcome::Closed`]
/// without writing. After a failure for another reason, later calls still write, and a
/// repeat of the failure is [`StdoutOutcome::FailedAgain`], so it is reported once for
/// as long as it lasts. A write that lands ends it: a failure after that is new, and
/// [`StdoutOutcome::Failed`] again — a `mds watch -o -` session whose stdout recovers
/// and fails again reports both (#157). A write of no bytes shows nothing about stdout
/// and ends nothing.
pub(crate) fn write_stdout(bytes: &[u8]) -> StdoutOutcome {
    write_stdout_to(&OUTPUT_STATE, &mut std::io::stdout().lock(), bytes)
}

/// [`write_stdout`] against any sink and state.
fn write_stdout_to<W: std::io::Write + ?Sized>(
    state: &OutputState,
    sink: &mut W,
    bytes: &[u8],
) -> StdoutOutcome {
    if state.stdout_closed() {
        return StdoutOutcome::Closed;
    }
    match sink.write_all(bytes).and_then(|()| sink.flush()) {
        Ok(()) => {
            if !bytes.is_empty() {
                state.note_stdout_written();
            }
            StdoutOutcome::Written
        }
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            state.note_stdout_closed();
            StdoutOutcome::Closed
        }
        Err(e) => {
            if state.note_stdout_failure() {
                StdoutOutcome::Failed(e)
            } else {
                StdoutOutcome::FailedAgain
            }
        }
    }
}

/// Which rule [`final_exit_code`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitPolicy {
    /// A run that ends on its own — build, check, fmt, lint, init, and a watch session
    /// that never went live: an output failure lifts the exit code to at least 2.
    Batch,
    /// A watch session after it went live ([`note_watch_session_live`]): its per-rebuild
    /// failures were reported as they happened, and they do not change the exit code.
    WatchSession,
}

/// The lowest exit code a batch run with an output failure ends with.
const IO_FAILURE_EXIT: i32 = 2;

/// The code a run whose verdict is `verdict` exits with, given what happened to its
/// output.
///
/// A panic comes first: a run that panicked exits 101 ([`PANIC_EXIT`]) under either
/// policy, whatever its verdict (#389). Otherwise a closed stdout or stderr pipe never
/// changes the code. Under [`ExitPolicy::Batch`], any other output failure lifts it to
/// `max(verdict, 2)` — a resource limit keeps its 3. [`ExitPolicy::WatchSession`]
/// returns `verdict` unchanged.
#[must_use]
pub(crate) fn final_exit_code(verdict: i32, state: &OutputState, policy: ExitPolicy) -> i32 {
    if state.panicked() {
        return PANIC_EXIT;
    }
    match policy {
        ExitPolicy::Batch if state.io_failed() => verdict.max(IO_FAILURE_EXIT),
        ExitPolicy::Batch | ExitPolicy::WatchSession => verdict,
    }
}

/// End the process: the CLI's exit funnel.
///
/// Exits with [`final_exit_code`] of `verdict` under the policy the run's state records
/// ([`OutputState::exit_policy`]): [`ExitPolicy::Batch`], so an output failure recorded
/// anywhere in the run is honoured on the way out — unless a watch session went live.
pub(crate) fn exit(verdict: i32) -> ! {
    std::process::exit(final_exit_code(
        verdict,
        &OUTPUT_STATE,
        OUTPUT_STATE.exit_policy(),
    ))
}

// ── Panics: one internal-compiler-error text (#389) ──────────────────────────
//
// A panic anywhere in the process runs `on_panic`, which prints `ICE_TEXT` and never
// the panic's message or location: a panic message can carry a user's text or a build
// machine's absolute paths. The run then exits 101. `main` runs the whole command
// inside `catch_panic`, so a panic on its thread unwinds — running the destructors that
// remove temporary files — to the exit funnel; a panic nothing catches, on a helper
// thread for one, ends the process at once through `exit_after_panic`.

/// What the CLI prints when it panics: that it failed, and where to report it. Nothing
/// about the panic itself.
const ICE_TEXT: &str = concat!(
    "mds: internal compiler error\n",
    "note: this is a bug in mds; please report it at ",
    env!("CARGO_PKG_REPOSITORY"),
    "/issues\n",
);

/// The exit code of a run that panicked, whatever else happened (#389) — Rust's own code
/// for a panic that ends `main`.
pub(crate) const PANIC_EXIT: i32 = 101;

/// The backtrace a panic prints after [`ICE_TEXT`], as `RUST_BACKTRACE` asks for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BacktraceStyle {
    /// The frames from the panic on.
    Short,
    /// Every frame (`RUST_BACKTRACE=full`).
    Full,
}

impl BacktraceStyle {
    /// The backtrace `RUST_BACKTRACE`'s `value` asks for, read as std reads it: none when
    /// it is unset or `0`, every frame for `full`, the short form for any other value.
    fn from_env(value: Option<&OsStr>) -> Option<Self> {
        match value {
            None => None,
            Some(value) if value == OsStr::new("0") => None,
            Some(value) if value == OsStr::new("full") => Some(Self::Full),
            Some(_) => Some(Self::Short),
        }
    }
}

thread_local! {
    /// Whether this thread is running a [`catch_panic`] closure, which catches a panic
    /// that unwinds out of it.
    static CATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether this thread is unwinding from a panic the hook let unwind, and no
    /// [`catch_panic`] has caught yet.
    static UNWINDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install the CLI's panic hook, [`on_panic`] (#389). `main` calls it before anything
/// else. `RUST_BACKTRACE` is read here, once: the hook itself reads no environment.
pub(crate) fn install_panic_hook() {
    let backtrace = BacktraceStyle::from_env(std::env::var_os("RUST_BACKTRACE").as_deref());
    std::panic::set_hook(Box::new(move |info| on_panic(backtrace, info)));
}

/// The panic hook: what a panic on any thread does (#389).
///
/// 1. One `write_all` of [`ICE_TEXT`] to stderr, its error ignored — a closed or failing
///    stderr loses the text and nothing else. Nothing the panic carries is formatted, so
///    no `Display` of the payload can run here, and std's stderr is a reentrant lock, so a
///    thread that panicked while writing to stderr takes it again. If another thread is
///    stuck inside a stderr write, this one waits with it.
/// 2. Record the panic: from here on [`final_exit_code`] is 101.
/// 3. With `RUST_BACKTRACE` asking, [`write_backtrace`]. With the never-shipped
///    `debug-panics` feature, the panic's message and location.
/// 4. Let the panic unwind only to a [`catch_panic`] on this thread that is not already
///    unwinding from an earlier one. Otherwise end the process now, exit 101
///    ([`exit_after_panic`]): a panic on a thread that nothing catches — a watch helper
///    thread — would end that thread alone and leave the command running without it; and
///    a second panic while the thread unwinds from the first — a destructor that panics
///    — would make std abort the process after a message of its own.
///
/// It must not panic: std aborts a process whose panic hook panics, after printing the
/// second panic's location. `std::panic::always_abort`, with which a panic aborts without
/// calling any hook, is unstable, and the CLI never calls it; std sets it only in a child
/// between `fork` and `exec`, before the child is `mds` at all. A panic that cannot
/// unwind at all — a check of undefined behaviour that debug builds compile in, say —
/// prints the text, and then std aborts.
fn on_panic(backtrace: Option<BacktraceStyle>, info: &std::panic::PanicHookInfo<'_>) {
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(ICE_TEXT.as_bytes());
    OUTPUT_STATE.note_panicked();
    if let Some(style) = backtrace {
        write_backtrace(&mut stderr, style);
    }
    #[cfg(feature = "debug-panics")]
    write_panic_detail(&mut stderr, info);
    #[cfg(not(feature = "debug-panics"))]
    let _ = info;
    if !unwinds_to_a_catch() {
        exit_after_panic();
    }
}

/// Whether the panic being reported will unwind to a [`catch_panic`] on this thread:
/// the thread is inside one, and not already unwinding from an earlier panic. Marks the
/// thread as unwinding.
fn unwinds_to_a_catch() -> bool {
    let catching = CATCHING.with(std::cell::Cell::get);
    let already_unwinding = UNWINDING.with(|unwinding| unwinding.replace(true));
    catching && !already_unwinding
}

/// End the process after a panic that nothing will catch: exit 101 at once (#389). The
/// panicking thread cannot return to [`exit`], so the panic path has this way out of its
/// own.
fn exit_after_panic() -> ! {
    std::process::exit(PANIC_EXIT)
}

/// A panic that unwound out of a [`catch_panic`] closure. The hook has reported it.
#[derive(Debug)]
pub(crate) struct Panicked;

/// Run `f`, catching a panic that unwinds out of it — the one place the CLI catches a
/// panic (#389). The hook has already printed the text and recorded the panic, so the
/// run exits 101 whatever it does next; nothing here reports the panic again.
///
/// The panic's payload is dropped without running its destructor (`mem::forget`): that
/// destructor is arbitrary code, and a panic in it would be a panic outside any catch.
pub(crate) fn catch_panic<T>(
    f: impl FnOnce() -> T + std::panic::UnwindSafe,
) -> std::result::Result<T, Panicked> {
    let outer = CATCHING.with(|catching| catching.replace(true));
    let caught = std::panic::catch_unwind(f);
    CATCHING.with(|catching| catching.set(outer));
    caught.map_err(|payload| {
        std::mem::forget(payload);
        UNWINDING.with(|unwinding| unwinding.set(false));
        Panicked
    })
}

/// Write the panicking thread's backtrace to `sink`, as `RUST_BACKTRACE` asked: a
/// `stack backtrace:` line, then the frames, each line WIRE-escaped with its line break
/// kept. A write that fails is ignored — stderr is where it would be reported.
fn write_backtrace<W: std::io::Write + ?Sized>(sink: &mut W, style: BacktraceStyle) {
    let backtrace = std::backtrace::Backtrace::force_capture();
    let mut text = String::from("stack backtrace:\n");
    // A frame that fails to render ends the text where it failed.
    let _ = match style {
        BacktraceStyle::Short => std::fmt::Write::write_fmt(&mut text, format_args!("{backtrace}")),
        BacktraceStyle::Full => {
            std::fmt::Write::write_fmt(&mut text, format_args!("{backtrace:#}"))
        }
    };
    let _ = sink.write_all(escape_each_line(&text).as_bytes());
}

/// `text` with each line WIRE-escaped on its own and ended by a line break, so a line
/// keeps its break and gains no raw control character.
fn escape_each_line(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for line in text.split_terminator('\n') {
        escaped.push_str(&mds::sanitize_control_chars_wire(line));
        escaped.push('\n');
    }
    escaped
}

/// The panic's message and location, escaped as a backtrace line is — only in a build
/// with the never-shipped `debug-panics` feature (#389).
#[cfg(feature = "debug-panics")]
fn write_panic_detail<W: std::io::Write + ?Sized>(
    sink: &mut W,
    info: &std::panic::PanicHookInfo<'_>,
) {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(a panic payload that is not text)");
    let mut text = String::new();
    let _ = std::fmt::Write::write_fmt(&mut text, format_args!("panic: {message}\n"));
    if let Some(location) = info.location() {
        let _ = std::fmt::Write::write_fmt(&mut text, format_args!("  at {location}\n"));
    }
    let _ = sink.write_all(escape_each_line(&text).as_bytes());
}

// ── Test-only panic trigger (#389) ───────────────────────────────────────────

/// `MDS_TEST_PANIC`: how a debug build is made to panic on purpose, for the tests of the
/// panic hook (`tests/panic_hook.rs`). A release build has none of it.
#[cfg(debug_assertions)]
mod panic_trigger {
    /// The variable that asks for a panic, and where: `main` in the command's dispatch,
    /// `thread` in a thread the dispatch starts and waits for.
    const VARIABLE: &str = "MDS_TEST_PANIC";

    /// A word the payload carries, for a test to look for.
    pub(super) const SENTINEL: &str = "mds-test-panic-payload";

    /// Panic as `MDS_TEST_PANIC` asks, if it asks. The dispatch calls it first.
    pub(crate) fn panic_on_request() {
        match std::env::var_os(VARIABLE)
            .as_deref()
            .and_then(std::ffi::OsStr::to_str)
        {
            Some("main") => std::panic::panic_any(payload()),
            Some("thread") => {
                // A thread's panic that does not end the process leaves the dispatch to
                // carry on as if nothing had happened — what a test must be able to see.
                let _ = std::thread::spawn(|| std::panic::panic_any(payload())).join();
            }
            _ => {}
        }
    }

    /// A payload no panic output may show: the sentinel, a raw ESC and the absolute path
    /// of the working directory.
    pub(super) fn payload() -> String {
        let esc = char::from(0x1b);
        let here = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        format!("{SENTINEL} {esc}[2J {}", here.display())
    }
}

#[cfg(debug_assertions)]
pub(crate) use panic_trigger::panic_on_request;

/// A release build's `MDS_TEST_PANIC` trigger: nothing (#389).
#[cfg(not(debug_assertions))]
pub(crate) fn panic_on_request() {}

// ── Stdin display sentinel ────────────────────────────────────────────────────

/// AD-211-1 / AD-211-3: the single stdin source-identity sentinel used by every
/// CLI diagnostic context.
///
/// Every user-visible emission of stdin's source identity — human diagnostics, JSON
/// `files[].file`, fix-preview status lines, diff headers, source-map `sources[]`,
/// and the analysis-failure envelope — uses this exact string.  The remap is applied
/// at the CLI output boundary; `crates/mds-core` continues to carry `"input.mds"`
/// (STRING_SOURCE_MAP_LABEL) as the internal VFS entry key, which is NOT changed.
///
/// Centralised here so the CLI has exactly one definition of the sentinel (AD-211-3),
/// replacing the previously scattered literals — including the hardcoded `"<stdin>"`
/// in `apply_source_map_file_label`, the `OK: <stdin>` status line in `main.rs`, and
/// the `fmt` stdin label.
pub(crate) const STDIN_DISPLAY_LABEL: &str = "<stdin>";

// ── Stdin source-identity relabel (AD-211-5) ─────────────────────────────────

/// Render-boundary wrapper that replaces the source identity embedded in an
/// [`mds::MdsError`] with [`STDIN_DISPLAY_LABEL`].
///
/// `resolve_source_intrinsic` sets `ctx.file_str = "<source>"`, so every error a
/// string-source (stdin) analysis produces carries `NamedSource::new("<source>", …)`
/// and renders as `<source>:L:C`. Replacing it here — not in `crates/mds-core` —
/// keeps the core constant intact for the non-stdin paths that legitimately use it
/// (`resolver_tests.rs` locks `SOURCE_LABEL`) and matches the "relabel at the CLI
/// output boundary" discipline of AD-211-1.
///
/// It also matches PF-014: the swap happens on the miette **input** (the
/// `NamedSource` handed to the renderer), never on already-rendered output.
///
/// Delegates every `Diagnostic` method to `inner` except `source_code`, which
/// returns the pre-built replacement (or `None` when the inner error carried no
/// embedded source, so miette skips code-frame rendering rather than trying to
/// resolve spans against a source that is not there).
///
/// # Exit codes
///
/// This type is transparent to [`crate::build::exit_code`], which unwraps it before
/// classifying the error. A wrapped `MdsError::FileNotFound` must still exit 2, not
/// 1 — see the downcast ladder there.
pub(crate) struct StdinRelabeledError {
    inner: mds::MdsError,
    /// `Some(named)` when the inner error's embedded source is the stdin sentinel
    /// `"<source>"` — replaced with a `NamedSource` labelled `"<stdin>"`.
    ///
    /// `None` in two cases:
    /// - The inner error had no embedded source at all (e.g. `MdsError::Io`).
    /// - The inner error's embedded source belongs to an **imported file** — its
    ///   real path must be preserved so miette renders the caret at the correct
    ///   location.  `source_code()` delegates to `inner` in both sub-cases.
    source: Option<miette::NamedSource<String>>,
}

impl StdinRelabeledError {
    /// The error this wrapper renders. Used by `exit_code` so wrapping cannot
    /// change a process exit status.
    pub(crate) fn inner(&self) -> &mds::MdsError {
        &self.inner
    }
}

impl std::fmt::Display for StdinRelabeledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl std::fmt::Debug for StdinRelabeledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl std::error::Error for StdinRelabeledError {}

impl miette::Diagnostic for StdinRelabeledError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        miette::Diagnostic::code(&self.inner)
    }
    fn severity(&self) -> Option<miette::Severity> {
        miette::Diagnostic::severity(&self.inner)
    }
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        miette::Diagnostic::help(&self.inner)
    }
    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        miette::Diagnostic::url(&self.inner)
    }
    fn labels<'a>(&'a self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + 'a>> {
        miette::Diagnostic::labels(&self.inner)
    }
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        match &self.source {
            Some(ns) => Some(ns as &dyn miette::SourceCode),
            // When `source` is None the relabel decided NOT to replace: either the
            // inner error carries no source at all (MdsError::Io) or it carries a
            // real imported-file source that must be preserved intact.  Delegate so
            // miette still renders the imported-file code frame correctly.
            None => miette::Diagnostic::source_code(&self.inner),
        }
    }
    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        miette::Diagnostic::related(&self.inner)
    }
    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        miette::Diagnostic::diagnostic_source(&self.inner)
    }
}

/// AD-211-5: build a report whose embedded source identity reads
/// [`STDIN_DISPLAY_LABEL`] instead of the core's `"<source>"`.
///
/// This is a **conditional** label swap: the replacement only happens when the
/// inner error's embedded `NamedSource` carries the stdin sentinel `"<source>"`
/// set by `resolve_source_intrinsic`.  Errors whose embedded source belongs to an
/// **imported file** carry the real file path and are left untouched — replacing
/// them would render the caret against stdin text at the wrong location, which is
/// exactly the PF-012 in-bounds-but-wrong class this PR set out to avoid.
/// AD-211-5 only authorised relabelling stdin's OWN source identity.
///
/// The source text used for span rendering, the message, the code, the help and
/// the labels are all untouched.  `miette`'s own
/// [`miette::Report::with_source_code`] cannot do this: its `WithSourceCode`
/// wrapper returns `self.error.source_code().or(Some(&self.source_code))`, so an
/// inner diagnostic that already carries a `NamedSource` (which these do) wins
/// and the replacement is ignored.
///
/// Call sites (symbolic references; prefer these over line numbers to avoid stale citations):
/// - `mds check -`: `run_check` in `crates/mds-cli/src/main.rs`
/// - `mds build -` (single-file path): `compile_to_content` in `crates/mds-cli/src/build.rs`
/// - `mds build -` (directory stdin path): `run_build` in `crates/mds-cli/src/build.rs`
/// - `mds lint -`: `run_lint_stdin` in `crates/mds-cli/src/lint.rs` (direct call)
///   and `run_lint_file` via `emit_analysis_failure_json_or_stderr` (indirect)
///
/// Any new CLI boundary that renders a stdin analysis failure must call this
/// function; skipping it renders `<source>` and breaks the uniform-sentinel rule.
pub(crate) fn relabel_stdin_error(e: &mds::MdsError, source: &str) -> miette::Report {
    // Only replace the embedded source when the inner error's source name is the
    // stdin sentinel set by `resolve_source_intrinsic` — checked via
    // `e.is_string_source()`, which keeps the sentinel comparison inside mds-core.
    // Errors from imported files carry the real file path; replacing them would
    // render the caret against stdin text at the wrong location (PF-012 / AD-211-5 scope).
    miette::Report::new(StdinRelabeledError {
        source: if e.is_string_source() {
            miette::Diagnostic::source_code(e)
                .map(|_| mds::named_source_for_render(STDIN_DISPLAY_LABEL, source))
        } else {
            None
        },
        inner: e.clone(),
    })
}

// ── Output-location validation (#265) ─────────────────────────────────────────

/// Refuse an output location that carries a forbidden path character (#265):
/// `mds::io`, exit 2.
///
/// `what` names the setting (`-o/--output`, `--out-dir`, `mds.json build.output_dir`).
/// Callers run this UP FRONT — before any input is read or compiled — so a hostile
/// output location never reaches path derivation, `create_dir_all` or the write
/// guard. The message names the codepoint as `U+XXXX` and shows the value escaped
/// by [`mds::escape_path_for_message`], so it carries none of the 80 codepoints.
///
/// `ensure_existing_mds_file` also runs it on a single-file argument (`what` =
/// `path`), before that file's existence check.
///
/// A value that is not valid UTF-8 is scanned lossily: every forbidden codepoint
/// that is validly encoded survives the conversion. The scan and the message are
/// [`mds::reject_forbidden_path`]'s, so the CLI refuses a path in mds-core's words.
pub(crate) fn reject_forbidden_output_path(
    what: &str,
    value: &OsStr,
) -> std::result::Result<(), mds::MdsError> {
    mds::reject_forbidden_path(what, Path::new(value), &value.to_string_lossy())
}

/// Refuse an output location whose RESOLVED form carries a forbidden path character
/// (#265): `mds::io`, exit 2.
///
/// [`reject_forbidden_output_path`] checks the value as typed; this checks where it
/// leads. A symlinked directory in it — or, for a relative value, the working
/// directory above it — can resolve into a hostile-named directory the value never
/// names. The location need not exist yet (it is created on the first write): its
/// deepest existing ancestor is canonicalized — the components below that are the
/// typed ones, already checked — and the whole resolved form is scanned. Callers run
/// it up front beside the typed check, so a refused location is never created or
/// written, and no later status line can show it.
///
/// The message, `<what> resolved path contains forbidden character U+XXXX: "<typed>"`,
/// names `typed`, the value as typed, escaped by [`mds::escape_path_for_message`] —
/// never the absolute resolved path. The scan and the message are
/// [`mds::reject_forbidden_path`]'s.
pub(crate) fn reject_forbidden_resolved_output_path(
    what: &str,
    path: &Path,
    typed: &OsStr,
) -> std::result::Result<(), mds::MdsError> {
    let Some(resolved) = resolve_existing_prefix(path) else {
        return Ok(());
    };
    mds::reject_forbidden_path(
        &format!("{what} resolved path"),
        &resolved,
        &typed.to_string_lossy(),
    )
}

/// The canonical form of the deepest existing ancestor of `path` (`path` itself when
/// it exists); a relative `path` is taken against the working directory.
///
/// `None` when nothing resolves — the working directory is gone, so there is nothing
/// on disk the value could lead through, and the typed check stands alone.
fn resolve_existing_prefix(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    // Bounded: one step per component of `absolute`.
    absolute.ancestors().find_map(|a| a.canonicalize().ok())
}

/// Refuse an `mds.json` `build.output_dir` with a `..` component: `mds::io`, exit 2.
///
/// The raw components are checked rather than a canonical form because the
/// directory may not exist yet (it is created on the first write). Shared by the
/// single-file (`resolve_output_path_for_kind`) and directory
/// ([`resolve_output_base`]) resolvers so both refuse it identically.
pub(crate) fn reject_output_dir_traversal(
    output_dir: &str,
) -> std::result::Result<(), mds::MdsError> {
    let traversal = Path::new(output_dir)
        .components()
        .any(|c| c == std::path::Component::ParentDir);
    if traversal {
        return Err(mds::MdsError::Io {
            message: format!(
                "mds.json output_dir '{}' must not contain '..' components",
                mds::escape_path_for_message(output_dir)
            ),
        });
    }
    Ok(())
}

// ── Output base for directory mode ────────────────────────────────────────────

/// Describes where directory-mode output files are written.
///
/// `Dir(base)` mirrors the source subtree under `base`:
///   `source.strip_prefix(root)` → `base/rel/stem.<ext>`
/// `NextToSource` places the output next to the source file.
#[derive(Debug, Clone)]
pub(crate) enum OutputBase {
    Dir(PathBuf),
    NextToSource,
}

/// Resolve `out_dir` to an absolute, canonicalized path for reliable `starts_with` checks.
///
/// Used by both `run_build_directory` and `dir_watch_startup` before calling
/// [`resolve_output_base`]. Relative paths are resolved against `current_dir`; the result
/// is then canonicalized (falls back to the absolute form when the directory does not yet exist).
pub(crate) fn canonicalize_out_dir(out_dir: Option<&PathBuf>) -> Option<PathBuf> {
    out_dir.map(|d| {
        let abs = if d.is_absolute() {
            d.clone()
        } else {
            // Fail-OPEN, deliberately left alone here (#217): when `current_dir()` fails
            // — the cwd was deleted, or is unreadable — the relative `--out-dir` is
            // anchored at `"."` instead, which resolves against whatever the process's
            // cwd actually is. The subsequent `canonicalize()` then usually fails too and
            // the non-absolute form is returned, so `starts_with` containment checks
            // downstream compare against a path that is not the one they assume.
            // Turning this into a hard error changes the signature of an infallible
            // helper and every caller with it; tracked as a follow-up rather than folded
            // into this change.
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(d)
        };
        abs.canonicalize().unwrap_or(abs)
    })
}

/// Compute the `OutputBase` for directory mode.
///
/// Precedence (mirrors `resolve_output_path` for file mode):
/// 1. `--out-dir` → `Dir(abs_out_dir)`
/// 2. `mds.json build.output_dir` → `Dir(config_dir.join(output_dir))`
///    — rejects `..` components at startup (`mds::io`, exit 2).
/// 3. Default → `NextToSource`
pub(crate) fn resolve_output_base(
    abs_out_dir: Option<&Path>,
    config: &Option<(MdsConfig, PathBuf)>,
) -> Result<OutputBase> {
    if let Some(d) = abs_out_dir {
        return Ok(OutputBase::Dir(d.to_path_buf()));
    }
    if let Some((cfg, config_dir)) = config {
        if let Some(ref output_dir) = cfg.build.output_dir {
            reject_output_dir_traversal(output_dir)?;
            return Ok(OutputBase::Dir(config_dir.join(output_dir)));
        }
    }
    Ok(OutputBase::NextToSource)
}

/// Compute the mirrored output path for a source file in directory mode.
///
/// Infallible — no directory creation.
///
/// Defined in terms of [`mirror_stem`] so the strip_prefix / AC-M7 path-escape logic is
/// kept in one place (issue 5 — single source of truth), shared with
/// [`output_base_no_ext`].
///
/// - `Dir(base)`: mirrors `source` relative to `root` under `base`.
///   If `strip_prefix` fails (source not under root after canonicalization),
///   falls back to `base/stem.<ext>` — **never** joins an absolute path that
///   could escape the output directory (AC-M7 path-escape guard).
/// - `NextToSource`: `source.with_extension(ext)`.
///
/// The `ext` parameter is the output extension without leading `.` (`"md"` or `"json"`).
///
/// # This is the WRITE oracle
///
/// It is called once per output path actually computed for a write, so it is where the
/// [`MirroredStem::Flattened`] arm is reported — a warning naming the source, the root
/// and the flat output. [`output_base_no_ext`] computes the same stem for bookkeeping
/// probes and stays silent; moving the report there would fire it on paths that are
/// never written, several times per watch batch (#217).
///
/// No live caller can reach the flattened arm: `build` walks `root` and hands the walk's
/// own prefix back here; `watch` gates event paths on `starts_with(&ctx.root)` and its
/// startup/baseline loops use canonical keys under a canonical root whose walker skips
/// symlinks. The warning is therefore an invariant report, not a user-facing condition —
/// if it is ever seen, one of those gates has moved.
pub(crate) fn output_path_for(source: &Path, root: &Path, base: &OutputBase, ext: &str) -> PathBuf {
    match base {
        OutputBase::Dir(d) => {
            let mirrored = mirror_stem(source, root, d);
            let flattened = matches!(mirrored, MirroredStem::Flattened(_));
            let no_ext = mirrored.into_path();
            // Invariant: `no_ext` was built by `mirror_stem` as `<something>/<stem>`, so
            // `file_name()` is `Some`. The literal fallback exists because the previous
            // one — `source.as_os_str()` — could be absolute, and an absolute name makes
            // the `join` below re-root out of the out-dir.
            let mut name = no_ext
                .file_name()
                .unwrap_or_else(|| OsStr::new("output"))
                .to_os_string();
            name.push(".");
            name.push(ext);
            let out = no_ext.parent().unwrap_or(d.as_path()).join(&name);
            // AC-M7 containment invariant: the output path must remain inside the out-dir.
            // `mirror_stem` already guards the strip_prefix escape case by returning
            // `d/<stem>` for out-of-root sources; the with-extension step cannot escape.
            // The check here is a defence-in-depth belt-and-suspenders assertion, and it
            // stays DEBUG-ONLY on purpose: the release fallback below is contained, so
            // there is nothing for a release-time check to prevent.
            let out = if out.starts_with(d) {
                out
            } else {
                debug_assert!(
                    false,
                    "output_path_for: AC-M7 violated — output {out:?} escaped out-dir {d:?}"
                );
                let flat_name = {
                    // Invariant: same as above — the join argument must be relative.
                    let mut n = source
                        .file_stem()
                        .unwrap_or_else(|| OsStr::new("output"))
                        .to_os_string();
                    n.push(".");
                    n.push(ext);
                    n
                };
                d.join(flat_name)
            };
            if flattened {
                // Invariant report, not gated on --quiet (like the depth-limit and
                // stale-unlink warnings above). Emitted once per output-path computation:
                // build — once per file, since its `output_base_no_ext` probes are silent;
                // watch — once per rebuild (#217).
                eprint_warning(&format!(
                    "warning: {} is outside the build root {}; its output is written flat \
                     as {} (another source outside the root with the same file name would \
                     overwrite it)",
                    safe_path(source),
                    safe_path(root),
                    safe_path(&out)
                ));
            }
            out
        }
        OutputBase::NextToSource => output_base_no_ext(source, root, base).with_extension(ext),
    }
}

// ── Directory traversal ───────────────────────────────────────────────────────

/// Result of a directory walk, carrying both the collected files and a count
/// of `.mds` files that were skipped because they reside inside
/// default-excluded directories (hidden dirs, `node_modules`).
///
/// A non-zero `excluded_by_default` with an empty `files` list means every
/// candidate was filtered out by the default exclusions — distinguishable from
/// a genuinely empty tree (where both are zero).
pub(crate) struct WalkResult {
    /// Files that were collected and are eligible for processing.
    pub files: Vec<PathBuf>,
    /// Count of `.mds` files found inside default-excluded directories.
    pub excluded_by_default: usize,
}

/// Recursively collect all `.mds` files under `root`, bounded by `max_depth`,
/// returning a [`WalkResult`] that also carries the count of files skipped due
/// to default exclusions (hidden dirs, `node_modules`).
///
/// Use this at call sites that need to distinguish "genuinely empty tree" from
/// "all candidates excluded". Use [`collect_mds_files`] at call sites (e.g.
/// watch) that only need the file list.
pub(crate) fn collect_mds_files_detailed(
    root: &Path,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
) -> WalkResult {
    let mut files = Vec::new();
    let mut excluded_by_default = 0;
    collect_mds_files_inner(
        root,
        0,
        max_depth,
        exclude_prefix,
        &mut files,
        &mut excluded_by_default,
    );
    WalkResult {
        files,
        excluded_by_default,
    }
}

/// Recursively collect all `.mds` files under `root`, bounded by `max_depth`.
///
/// Symlinked directories AND symlinked files are skipped to avoid cycles and
/// to maintain build parity with the single-file symlink guard (PF-004 / commit aa0c538).
/// When `exclude_prefix` is `Some(p)`, any path that starts with `p` is skipped
/// (used to exclude the out-dir when it is inside the watched root).
///
/// For callers that need to distinguish "genuinely empty tree" from "all candidates
/// excluded", use [`collect_mds_files_detailed`] instead.
pub(crate) fn collect_mds_files(
    root: &Path,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
) -> Vec<PathBuf> {
    collect_mds_files_detailed(root, max_depth, exclude_prefix).files
}

/// Return `true` when a directory name should be excluded from recursive
/// traversal by default (PF-004: enforced on the shared walker so ALL
/// subcommands — build / check / lint / fmt / watch — inherit the behaviour).
///
/// Excluded directory names:
/// - Any name that starts with `.` (hidden directories, e.g. `.git`, `.cache`)
/// - `node_modules`
///
/// Note: this gate applies to the RECURSION step only — the root directory
/// that was explicitly passed to `collect_mds_files` is always processed,
/// even if its own name happens to start with `.`.  Hidden *files* (e.g.
/// `.dotfile.mds`) at the traversed directory level are still collected.
pub(crate) fn is_default_excluded_dir(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

/// Return `true` when `path` lives inside a default-excluded sub-directory
/// of `root` (i.e. traversal would have been skipped there by
/// `is_default_excluded_dir`).
///
/// Used by the watch guards to detect events that should be treated as external
/// dependencies rather than normal output-producing sources (PF-004 class:
/// the same limit must be enforced on the parallel event-processing path as on
/// the initial walker path — avoids the "limit on one path but not another"
/// bug class).
pub(crate) fn is_within_default_excluded_dir(root: &Path, path: &Path) -> bool {
    // Strip the root prefix to get a relative path, then walk the ancestor
    // chain using Path::parent() — avoids allocating a Vec<Component> just to
    // drop the final component (issue #69: this runs on the watch per-event
    // hot path).
    //
    // Edge case: when `rel` is a single component (e.g. "foo.mds"),
    // rel.parent() returns Some("") whose file_name() is None, and
    // "".parent() returns None, ending the loop correctly.
    let rel = match path.strip_prefix(root) {
        // `false` IS the closed value here (#217): this predicate answers "is `path`
        // inside a default-excluded subdirectory OF `root`", and a path that is not
        // under `root` at all has no such subdirectory — the honest answer is no. It is
        // not a fail-open default, because neither caller acts on this answer alone:
        // both `handle_fs_event_dir` and `process_dir_batch_incremental` evaluate it
        // only in conjunction with their own `starts_with(root)` test, so an out-of-root
        // path is already classified as an external dep (compile for deps, never emit
        // output) before this function's answer is consulted.
        Err(_) => return false,
        Ok(r) => r,
    };
    let mut ancestor = rel.parent();
    while let Some(dir) = ancestor {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            if is_default_excluded_dir(name) {
                return true;
            }
        }
        ancestor = dir.parent();
    }
    false
}

fn collect_mds_files_inner(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
    results: &mut Vec<PathBuf>,
    excluded_count: &mut usize,
) {
    if depth > max_depth {
        // The directory name is discovered by the walk — the user never types it — so it
        // is untrusted on every subcommand that shares this walker (build / check / fmt /
        // lint / watch).  Prose through `eprint_warning` (HUMAN), the path through
        // `safe_path` (WIRE): a directory name is never legitimately multi-line, and a
        // raw one here forged a standalone `Clean: …` line as well as emitting raw ESC.
        eprint_warning(&format!(
            "warning: directory depth limit ({max_depth}) reached at {}; \
             deeper files will not be processed",
            safe_path(dir)
        ));
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();

        // Skip the output directory when it is nested inside the root.
        if let Some(excl) = exclude_prefix {
            if path.starts_with(excl) {
                continue;
            }
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            // Symlinked dirs AND symlinked files are skipped (PF-004 / build parity).
            // This preserves the same guard as single-file mode where symlinked entries
            // are rejected at startup (commit aa0c538).
            continue;
        }
        if file_type.is_dir() {
            // Skip hidden directories (e.g. .git, .cache) and node_modules on the
            // RECURSION step so all subcommands inherit the default exclusions via
            // the shared walker (PF-004).  The root dir that was explicitly passed
            // to collect_mds_files() is NEVER checked here — this guard applies
            // only to directory ENTRIES discovered during traversal.  Since entries
            // are always children of the current dir, they are never the explicit root.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if is_default_excluded_dir(name) {
                    // Count the .mds files we're skipping so callers can emit a
                    // meaningful diagnostic when all candidates are excluded.
                    count_mds_in_excluded_dir(&path, depth + 1, max_depth, excluded_count);
                    continue;
                }
            }
            collect_mds_files_inner(
                &path,
                depth + 1,
                max_depth,
                exclude_prefix,
                results,
                excluded_count,
            );
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds") {
            results.push(path);
        }
    }
}

/// Count `.mds` files inside a directory that is being skipped due to a
/// default exclusion. Symlinks are still skipped. Does not apply further
/// exclusion filtering — we are already inside an excluded root, so every
/// `.mds` descendant is a skipped candidate regardless of name.
fn count_mds_in_excluded_dir(dir: &Path, depth: usize, max_depth: usize, count: &mut usize) {
    if depth > max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            count_mds_in_excluded_dir(&path, depth + 1, max_depth, count);
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds") {
            *count += 1;
        }
    }
}

/// Return `true` if `path`'s file name starts with `_` (partial convention, DD2).
///
/// A name that is not valid UTF-8 answers `false` — "not a partial", i.e. a source that
/// should be compiled and written. That is the closed answer, not the open one (#217):
/// such a source never reaches a write, because every read goes through
/// `mds-core`'s `path_to_str`, which rejects a non-UTF-8 path with `MdsError::Io`. The
/// file is reported as a per-file failure (exit 2 in single-file mode, one `failed` in
/// the directory tally) instead of being silently skipped the way `true` would skip it.
pub(crate) fn is_partial(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.starts_with('_'))
        .unwrap_or(false)
}

/// `Some(n)` when `files` is non-empty and every entry is a `_`-prefixed partial —
/// the "nothing to build/check" case #387 closes; `None` for an empty list (the
/// empty-tree arm owns that) or when any non-partial entry exists. One predicate for
/// `run_build_directory` and `run_check_directory` so the two arms cannot drift.
pub(crate) fn partials_only(files: &[PathBuf]) -> Option<usize> {
    if files.is_empty() {
        return None;
    }
    files.iter().all(|f| is_partial(f)).then_some(files.len())
}

// ── Stale-output cleanup ──────────────────────────────────────────────────────

/// Probe for BOTH possible output siblings and unlink the one that does NOT match `kind`.
///
/// Called after writing a compiled output to clean up a stale sibling from a previous
/// format flip (e.g. a file that used to emit `x.md` but now emits `x.json`).
///
/// If neither sibling exists the function is a no-op, and a removal that succeeds is
/// silent: stale cleanup is a housekeeping detail. A wrong-extension file that exists but
/// cannot be removed is an `mds::io` error the caller reports (#157); nothing is printed
/// here.
///
/// `base_path` must be the path WITHOUT extension (e.g. `/out/foo` for a source
/// `foo.mds`). The function constructs `base_path.with_extension("md")` and
/// `base_path.with_extension("json")` and removes the one that contradicts `kind`.
///
/// AC-FUNC-23 (watch format-flip) and the equivalent dir-build stale-cleanup both
/// call this function so the probe-and-unlink logic is shared.
pub(crate) fn probe_and_remove_stale(
    base_no_ext: &Path,
    kind: OutputKind,
) -> std::result::Result<(), mds::MdsError> {
    let stale_ext = kind.stale_extension();
    let stale_path = base_no_ext.with_extension(stale_ext);
    if !stale_path.exists() {
        return Ok(());
    }
    // The path is walker-derived and the `io::Error` Display embeds a path of its own,
    // so both are WIRE-escaped as the message is built.
    std::fs::remove_file(&stale_path).map_err(|e| mds::MdsError::Io {
        message: format!(
            "could not remove stale output {}: {}",
            safe_path(&stale_path),
            safe_inline(&e)
        ),
    })
}

/// Where a `Dir(_)`-mode source landed.
///
/// `Flattened` is the `strip_prefix` failure arm: contained by construction (the join
/// argument is always a relative `OsStr`) but it abandons the subtree mirror, so two
/// out-of-root sources with the same file name map to the same path. Unreachable from
/// every live caller — see [`output_path_for`] — and the variant exists so the write
/// oracle can *say* so instead of silently degrading (#217).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MirroredStem {
    /// `source` was below `root`: its relative subtree is preserved under the out-dir.
    Mirrored(PathBuf),
    /// `source` was not below `root`: only its stem survives, joined to the out-dir.
    Flattened(PathBuf),
}

impl MirroredStem {
    /// The extension-less output path, whichever arm produced it.
    pub(crate) fn into_path(self) -> PathBuf {
        match self {
            Self::Mirrored(p) | Self::Flattened(p) => p,
        }
    }
}

/// Compute the `Dir(_)`-mode extension-less output stem for `source`, classified by
/// whether the subtree mirror survived.
///
/// Single source of truth for both [`output_base_no_ext`] (the silent probe oracle) and
/// [`output_path_for`] (the write oracle that reports the flatten).
fn mirror_stem(source: &Path, root: &Path, d: &Path) -> MirroredStem {
    match source.strip_prefix(root) {
        Ok(rel) => {
            // Invariant: `rel` is a non-empty RELATIVE path — `source` is a regular
            // `.mds` file strictly below `root` — so `file_stem()` is `Some`. For a
            // single-component `rel` (a bare file name) `parent()` is `Some("")`, not
            // `None`, and `d.join("")` is `d`, so bare names land directly in the
            // out-dir rather than re-rooting.
            let stem = rel.file_stem().unwrap_or(rel.as_os_str()).to_os_string();
            MirroredStem::Mirrored(d.join(rel.parent().unwrap_or(Path::new(""))).join(stem))
        }
        Err(_) => {
            // Invariant: the join argument must be relative, or `d.join` re-roots and
            // the result leaves the out-dir entirely (`d.join("/") == "/"`).
            // `file_stem()` is `None` only for `/`, `..` and a bare drive prefix —
            // never a `.mds` file — and the fallback that used to stand here,
            // `source.as_os_str()`, was exactly the absolute value that escapes. A
            // literal is the only value guaranteed relative for every input.
            let stem = source.file_stem().unwrap_or_else(|| OsStr::new("output"));
            MirroredStem::Flattened(d.join(stem))
        }
    }
}

/// Return the path stem (path without extension) for a compiled source.
///
/// Used to construct the `base_no_ext` argument to [`probe_and_remove_stale`].
///
/// For `Dir(base)` mode this defers to [`mirror_stem`] so the stem is always computed
/// consistently with [`output_path_for`].
pub(crate) fn output_base_no_ext(source: &Path, root: &Path, base: &OutputBase) -> PathBuf {
    match base {
        OutputBase::Dir(d) => mirror_stem(source, root, d).into_path(),
        OutputBase::NextToSource => {
            // source.with_extension("") removes the existing extension.
            source.with_extension("")
        }
    }
}

// ── Atomic file write ─────────────────────────────────────────────────────────

/// How hard [`atomic_write_file`] works to make the new bytes survive a crash.
///
/// Atomicity — a reader sees either the whole old file or the whole new one, never a
/// truncated mix — is unconditional: it comes from the rename, not from the fsync. This
/// knob only chooses whether the data is forced to stable storage *before* that rename.
///
/// The split exists because the two families of file MDS writes have different recovery
/// costs, and on macOS `sync_all()` is `F_FULLFSYNC` — a full drive cache flush, ~7 ms
/// per file. Measured on a 500-template `mds watch` startup (#227): 1.44 s → 4.69 s, and
/// the `cli_watch` suite 4.2 s → 8.3 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
    /// `sync_all()` before the rename. For files whose content exists nowhere else:
    /// `mds fmt` and `mds lint --fix` rewrite the user's hand-authored `.mds` source in
    /// place, so bytes lost to a power failure are lost for good.
    Fsync,
    /// Rename only. For **derived** artifacts — compiled outputs and `.map` sidecars —
    /// which are reproducible by re-running `mds build`. A crash can leave the previous
    /// artifact or an unflushed new one; either way the fix is one rebuild, and paying
    /// `F_FULLFSYNC` per file to avoid it costs more than it saves.
    RenameOnly,
}

/// Write `content` to `path` atomically via a temp-file-then-rename cycle.
///
/// Centralising this helper in `output.rs` ensures both `fmt` and `lint --fix`
/// route through the same write path (avoids PF-004 — a check enforced on the
/// primary path silently absent on a sibling path).
///
/// This is the single write primitive for every file the CLI produces: `fmt` and
/// `lint --fix` rewrites, and — since #227 — every `mds build` / `mds watch`
/// artifact and `.map` sidecar. The parent directory must already exist; callers
/// that need directories create them first.
///
/// Behaviour: the target is probed with `lstat`. A regular file is replaced
/// (final-component symlink re-check, Unix mode preserved with `& 0o7777`). A
/// symlink at the target — live or dangling — is refused rather than written
/// through. An absent target is created with mode `0666 & !umask`, i.e. what
/// `std::fs::write` produced. Any other stat failure is an error, never a silent
/// mode guess (#225).
///
/// Safety properties:
/// - Re-checks for symlink immediately before the write (TOCTOU guard, AC-F-21).
/// - Temp file lives in the SAME directory as the target so the rename is
///   always intra-filesystem (atomic on POSIX, near-atomic on Windows).
/// - Calls `sync_all()` (not `flush()` — `flush()` is a no-op on unbuffered
///   `File`) for crash durability before the rename, when `durability` is
///   [`Durability::Fsync`]. Under [`Durability::RenameOnly`] the fsync is skipped;
///   the rename — and therefore the atomicity — is unchanged. See [`Durability`]
///   for which callers pick which and why.
/// - Directory-level symlinks in the path are resolved, not rejected (the same
///   rule `NativeFs::check_symlink` applies).
///
/// # Contract (#226)
///
/// This is replace-by-rename, not an in-place rewrite. The target path receives a
/// NEW inode, so the write does NOT preserve hard links (other links keep the old
/// content), ACLs, extended attributes (xattrs), or owner/group of the original
/// file; only the permission bits are carried over (Unix). This applies to every
/// path routed through this helper: `mds fmt` and `mds lint --fix` source
/// rewrites and, under #227, `mds build` / `mds watch` compiled outputs and
/// `.map` sidecars. Hard-link preservation is out of scope by construction (it
/// would require truncate-in-place and forfeit crash safety); ACL/xattr/
/// owner-group preservation is not planned — MDS only rewrites its own outputs
/// and `.mds` sources.
///
/// # Errors
///
/// Every failure is `mds::io` (exit 2, #157), its message naming the target.
pub(crate) fn atomic_write_file(
    path: &Path,
    content: &str,
    durability: Durability,
) -> std::result::Result<(), mds::MdsError> {
    use mds::{effective_parent, NativeFs};

    // effective_parent maps "" (bare filename) and None to "." — avoids PF-006.
    let parent = effective_parent(path);

    // #409: this primitive writes every `mds build`/`watch` output (under a
    // possibly-canonicalized `--out-dir`) and every `fmt`/`lint --fix` source
    // rewrite, so its own error text must show the conventional form too, not a
    // Windows verbatim prefix. Computed once and reused below.
    let shown = mds::display_native_path(path);
    let io_error = |message: String| mds::MdsError::Io { message };

    // #227: `mds build` targets may not exist yet. Probe with lstat, which never
    // follows a symlink: `Ok` means something is there (a regular file, or a
    // symlink — live or dangling — which is refused below); `Err(NotFound)` means
    // create a new file. Any other lstat failure is a hard error (#225: silently
    // writing with a guessed mode was the defect, and a warning is not a decision).
    let existing = match path.symlink_metadata() {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(format!("cannot stat {}: {e}", shown.display()))),
    };

    if let Some(m) = &existing {
        if m.file_type().is_symlink() {
            return Err(io_error(format!(
                "cannot write {}: refusing to replace a symlink",
                shown.display()
            )));
        }
        // Re-check for symlink right before writing (TOCTOU guard).
        NativeFs::check_symlink(path)
            .map_err(|e| io_error(format!("cannot write {}: {e}", shown.display())))?;
    }

    // Mode to restore on Unix. The lstat result of a non-symlink IS the file's
    // metadata, so there is no second stat call and no site left for the spurious
    // metadata warning that fired on every first build (#225, #227).
    // `None` = new file.
    #[cfg(unix)]
    let original_mode: Option<u32> = {
        use std::os::unix::fs::PermissionsExt as _;
        existing.as_ref().map(|m| m.permissions().mode())
    };

    // Temp file in same directory so rename is always intra-filesystem.
    let mut builder = tempfile::Builder::new();
    builder.prefix(".mds-tmp-").suffix(".tmp");
    // New file: request 0666 and let the kernel apply the umask, so a first
    // `mds build` creates the same mode `std::fs::write` did (typically 0644).
    // `tempfile`'s default is 0600, which would make every fresh artifact
    // owner-only.
    #[cfg(unix)]
    if original_mode.is_none() {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut tmp = builder.tempfile_in(parent).map_err(|e| {
        io_error(format!(
            "cannot create temp file for {}: {e}",
            shown.display()
        ))
    })?;

    // Restore original permissions before writing; mask off file-type bits
    // (high bits of st_mode) so only the permission bits reach from_mode.
    #[cfg(unix)]
    if let Some(mode) = original_mode {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|e| {
                io_error(format!(
                    "cannot set permissions on temp file for {}: {e}",
                    shown.display()
                ))
            })?;
    }

    tmp.write_all(content.as_bytes())
        .map_err(|e| io_error(format!("cannot write {}: {e}", shown.display())))?;

    // sync_all() flushes data + metadata to storage (flush() is a no-op on
    // unbuffered File and provides no crash durability guarantee). Skipped for
    // derived artifacts, which a rebuild reproduces — see `Durability`.
    if durability == Durability::Fsync {
        tmp.as_file()
            .sync_all()
            .map_err(|e| io_error(format!("cannot fsync {}: {e}", shown.display())))?;
    }

    // persist() atomically renames the temp file to the target path.
    tmp.persist(path).map_err(|e| {
        io_error(format!(
            "cannot rename temp file to {}: {e}",
            shown.display()
        ))
    })?;

    Ok(())
}

// ── Sanitized stderr render ───────────────────────────────────────────────────

/// Re-export: byte-length-preserving source neutralization before miette rendering.
///
/// Lives in `mds-core` so both `MdsError::at()` (compiler error path) and
/// `render_diag_human` (lint diagnostic path) use a single canonical implementation
/// (avoids PF-004 / PF-014 parallel-path drift).
pub(crate) use mds::neutralize_source_for_render;

/// A terminal-safe view of a [`miette::Report`], built **before** rendering.
///
/// Overrides every text surface the frame can render — the `Display` message, the
/// `code`, the `help` and `url` text, each [`miette::LabeledSpan`]'s label, and the
/// whole auxiliary diagnostic graph (`source` cause chain, `related`,
/// `diagnostic_source`) — with [`mds::sanitize_control_chars`]-escaped copies (HUMAN
/// mode, so `\n` and `\t` survive and multi-line frames stay readable).  Everything the
/// frame's geometry depends on — `severity`, `source_code`, and each label's byte span —
/// is delegated to the inner report untouched, so the byte-length-preserving
/// neutralization already applied to source excerpts (via `mds::named_source_for_render`)
/// keeps every span offset and caret column exact.
///
/// Every copy is rendered at construction without panicking (#157): a surface whose
/// `Display` fails is dropped, or — for a message, which is never optional — replaced
/// by a fixed placeholder. miette then formats owned strings for all of those surfaces;
/// what it still reads through the inner report is the source excerpt, and a read that
/// fails there is handled by [`render_error_sanitized`].
///
/// # Why this is the PF-014-correct boundary
///
/// The rendered frame is never post-processed.  Running a sanitizer over an
/// already-rendered miette frame would escape miette's *own* ANSI SGR colour codes
/// into literal `\u001b[33m` noise on any colour-capable TTY — on completely benign
/// input — and desynchronise caret alignment.  CI cannot catch that regression
/// because the CLI tests pin `NO_COLOR=1` and pipe stderr.  Sanitizing the renderer's
/// *inputs* has neither failure mode.
///
/// # Why it wraps the whole `Report` rather than just `MdsError`
///
/// The CLI produces two error families: `MdsError` (compiler diagnostics) and
/// CLI-authored `miette::miette!()` reports, which do **not** downcast to `MdsError`
/// (see `build::exit_code`).  Both interpolate untrusted text — a template's include
/// alias in the first case, an `mds.json` value or a hostile filename in the second.
/// Wrapping at the `Report` level covers both, and keeps covering any error type added
/// later, so the guarantee holds by construction rather than by remembering to extend
/// a downcast ladder (avoids PF-004).
///
/// # The auxiliary graph (`source` / `related` / `diagnostic_source`)
///
/// A `Diagnostic` can hang three further diagnostic graphs off itself, all of which
/// miette renders: the `std::error::Error` cause chain, the `related()` siblings, and
/// `diagnostic_source()`.  All three carry prose, so all three must be escaped.
///
/// They cannot be forwarded by reference the way `code`/`severity`/`url` are: those
/// accessors return borrows of the *inner* report, so handing back a sanitized view
/// would require the wrapper to own it and hand out a borrow of itself.  This wrapper
/// therefore materialises the whole graph into owned [`SanitizedNode`]s at construction
/// (bounded by [`MAX_AUX_DEPTH`]) and forwards borrows of those.
///
/// Enforcing this with a `debug_assert!` that the graph is empty — which is what an
/// earlier revision did, on the grounds that no CLI error populates it today — is
/// exactly PF-005: `debug_assert!` is compiled out of release, so the invariant would
/// hold under test and be absent in the shipped binary.  The first error type to grow a
/// `#[source]` field would have silently dropped its cause chain from release stderr
/// while CI stayed green.
struct SanitizedReport {
    inner: miette::Report,
    message: String,
    code: Option<String>,
    help: Option<String>,
    url: Option<String>,
    source: Option<SanitizedNode>,
    related: Vec<SanitizedNode>,
    diagnostic_source: Option<SanitizedNode>,
}

/// Depth bound for materialising a report's cause / related / diagnostic-source graph.
///
/// Cause chains are finite in practice, but nothing in the `Error` trait forbids a cycle
/// (a `source()` that returns a sibling of itself would loop forever).  Rendering is not
/// a place to discover that, so the walk is explicitly bounded; a graph deeper than this
/// is truncated, which drops trailing context but never hangs or overflows the stack.
const MAX_AUX_DEPTH: usize = 16;

/// An owned, control-character-escaped snapshot of one node in a report's auxiliary
/// diagnostic graph.
///
/// Every prose surface (`message`, `help`, `code`, `url`, label text) is escaped with
/// HUMAN-mode [`mds::sanitize_control_chars`] when the node is built.  Label byte spans
/// are copied verbatim, exactly as `SanitizedReport::labels` does, so caret geometry
/// against the parent's already-neutralized source stays exact.
///
/// `source_code()` returns `None` by design: a `&dyn miette::SourceCode` cannot be
/// cloned out of the inner diagnostic, and miette falls back to the *parent* report's
/// source — which `SanitizedReport::source_code` forwards — when a nested diagnostic
/// supplies none.  So a nested diagnostic still renders against neutralized source.
struct SanitizedNode {
    message: String,
    help: Option<String>,
    code: Option<String>,
    url: Option<String>,
    severity: Option<miette::Severity>,
    labels: Option<Vec<miette::LabeledSpan>>,
    source: Option<Box<SanitizedNode>>,
    related: Vec<SanitizedNode>,
    diagnostic_source: Option<Box<SanitizedNode>>,
}

/// Placeholder for a message whose own `Display` failed (#157).
const UNFORMATTABLE: &str = "(this text could not be formatted)";

/// Render `d` into a `String` without panicking: `None` when its `Display` fails.
///
/// `to_string()` and `format!` panic when an impl returns `fmt::Error`, and a report's
/// text comes from `Display` impls this crate does not control.
fn display_text<T: std::fmt::Display + ?Sized>(d: &T) -> Option<String> {
    let mut text = String::new();
    std::fmt::Write::write_fmt(&mut text, format_args!("{d}")).ok()?;
    Some(text)
}

/// Escape one optional `Display` surface to an owned `String`.
///
/// A surface whose `Display` fails is dropped: code, help and url are optional, so the
/// frame renders without it.
fn escape_display(d: Option<Box<dyn std::fmt::Display + '_>>) -> Option<String> {
    d.and_then(|v| display_text(&*v))
        .map(|text| mds::sanitize_control_chars(&text).into_owned())
}

/// Escape a message, or [`UNFORMATTABLE`] when its `Display` fails — a message is never
/// optional, so its place says why it is empty.
fn escape_message<T: std::fmt::Display + ?Sized>(d: &T) -> String {
    display_text(d).map_or_else(
        || UNFORMATTABLE.to_owned(),
        |text| mds::sanitize_control_chars(&text).into_owned(),
    )
}

/// Escape a `Diagnostic`'s label text, keeping each byte span verbatim.
///
/// `None` is preserved as `None` (rather than collapsed to an empty vector) so miette
/// distinguishes "no labels" from "labels, but none of them" exactly as it did before
/// the wrapper was introduced.
fn escape_labels(d: &dyn miette::Diagnostic) -> Option<Vec<miette::LabeledSpan>> {
    Some(
        d.labels()?
            .map(|label| {
                let text = label
                    .label()
                    .map(|t| mds::sanitize_control_chars(t).into_owned());
                let span = *label.inner();
                if label.primary() {
                    miette::LabeledSpan::new_primary_with_span(text, span)
                } else {
                    miette::LabeledSpan::new_with_span(text, span)
                }
            })
            .collect(),
    )
}

impl SanitizedNode {
    /// Build from a `Diagnostic` node (used for `related` / `diagnostic_source`).
    fn from_diagnostic(d: &dyn miette::Diagnostic, depth: usize) -> Self {
        Self {
            message: escape_message(d),
            help: escape_display(d.help()),
            code: escape_display(d.code()),
            url: escape_display(d.url()),
            severity: d.severity(),
            labels: escape_labels(d),
            source: Self::chain_from_error(std::error::Error::source(d), depth),
            related: Self::related_from(d, depth),
            diagnostic_source: Self::boxed_from_diagnostic(d.diagnostic_source(), depth),
        }
    }

    /// Build from a plain `Error` node (used for the `source()` cause chain, whose links
    /// expose no `Diagnostic` data — only a `Display` message and a further `source()`).
    fn from_error(e: &(dyn std::error::Error + 'static), depth: usize) -> Self {
        Self {
            message: escape_message(e),
            help: None,
            code: None,
            url: None,
            severity: None,
            labels: None,
            source: Self::chain_from_error(e.source(), depth),
            related: Vec::new(),
            diagnostic_source: None,
        }
    }

    fn chain_from_error(
        e: Option<&(dyn std::error::Error + 'static)>,
        depth: usize,
    ) -> Option<Box<SanitizedNode>> {
        if depth >= MAX_AUX_DEPTH {
            return None;
        }
        e.map(|e| Box::new(Self::from_error(e, depth + 1)))
    }

    fn boxed_from_diagnostic(
        d: Option<&dyn miette::Diagnostic>,
        depth: usize,
    ) -> Option<Box<SanitizedNode>> {
        if depth >= MAX_AUX_DEPTH {
            return None;
        }
        d.map(|d| Box::new(Self::from_diagnostic(d, depth + 1)))
    }

    fn related_from(d: &dyn miette::Diagnostic, depth: usize) -> Vec<SanitizedNode> {
        if depth >= MAX_AUX_DEPTH {
            return Vec::new();
        }
        d.related()
            .map(|rs| {
                rs.map(|r| SanitizedNode::from_diagnostic(r, depth + 1))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl std::fmt::Display for SanitizedNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Hand-written for the same reason as [`SanitizedReport`]'s: the derived `Debug` of a
/// miette type can be its full graphical render, which would bypass the escaping.
impl std::fmt::Debug for SanitizedNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SanitizedNode")
            .field("message", &self.message)
            .field("help", &self.help)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for SanitizedNode {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|n| n as &(dyn std::error::Error + 'static))
    }
}

impl miette::Diagnostic for SanitizedNode {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.code.as_deref())
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.severity
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.help.as_deref())
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.url.as_deref())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(self.labels.as_ref()?.iter().cloned()))
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        related_iter(&self.related)
    }

    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.diagnostic_source
            .as_deref()
            .map(|n| n as &dyn miette::Diagnostic)
    }
}

/// Box an optional `&str` as the `Display` trait object miette's accessors return.
fn boxed_str(s: Option<&str>) -> Option<Box<dyn std::fmt::Display + '_>> {
    s.map(|s| -> Box<dyn std::fmt::Display + '_> { Box::new(s) })
}

/// Shared `related()` body: `None` when empty, so miette omits the section entirely.
fn related_iter(
    nodes: &[SanitizedNode],
) -> Option<Box<dyn Iterator<Item = &dyn miette::Diagnostic> + '_>> {
    if nodes.is_empty() {
        return None;
    }
    Some(Box::new(nodes.iter().map(|n| n as &dyn miette::Diagnostic)))
}

impl SanitizedReport {
    fn new(inner: miette::Report) -> Self {
        let message = escape_message(&inner);
        let code = escape_display(inner.code());
        let help = escape_display(inner.help());
        let url = escape_display(inner.url());
        let source = SanitizedNode::chain_from_error(std::error::Error::source(&*inner), 0)
            .map(|boxed| *boxed);
        let related = SanitizedNode::related_from(inner.as_ref(), 0);
        let diagnostic_source =
            SanitizedNode::boxed_from_diagnostic(inner.diagnostic_source(), 0).map(|boxed| *boxed);

        Self {
            inner,
            message,
            code,
            help,
            url,
            source,
            related,
            diagnostic_source,
        }
    }
}

impl std::fmt::Display for SanitizedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Hand-written so the wrapper never surfaces the inner report's own `Debug`, which
/// under miette's `fancy` feature is the full graphical render of the *unsanitized*
/// error.
impl std::fmt::Debug for SanitizedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SanitizedReport")
            .field("message", &self.message)
            .field("help", &self.help)
            .finish_non_exhaustive()
    }
}

/// Forwards the **sanitized** cause chain, never the inner report's own.
impl std::error::Error for SanitizedReport {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|n| n as &(dyn std::error::Error + 'static))
    }
}

impl miette::Diagnostic for SanitizedReport {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.code.as_deref())
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.inner.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.help.as_deref())
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        boxed_str(self.url.as_deref())
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.inner.source_code()
    }

    /// Byte spans are forwarded verbatim (they index the neutralized, byte-length-
    /// preserved source); only the label *text* is escaped.  `LintDiagnostic` uses its
    /// own message as the label text, so this is a real untrusted-text surface.
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(escape_labels(self.inner.as_ref())?.into_iter()))
    }

    /// The **sanitized** related-diagnostic snapshots, not the inner report's.
    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        related_iter(&self.related)
    }

    /// The **sanitized** diagnostic source, not the inner report's.
    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.diagnostic_source
            .as_ref()
            .map(|n| n as &dyn miette::Diagnostic)
    }
}

/// Wrap `report` so every prose surface it renders is control-character-escaped.
///
/// This is the construction-boundary half of the PF-014 design: callers hand it a
/// `Report` built from raw values, and it produces one whose message, help, and label
/// text are safe to hand to miette.  See [`SanitizedReport`] for the full rationale.
fn sanitize_report(report: miette::Report) -> miette::Report {
    miette::Report::new(SanitizedReport::new(report))
}

/// Sanitize `report`'s inputs and render it to a `String` — the pure transformation
/// extracted from [`eprint_error`] so it can be tested without touching stderr.
///
/// The rendered frame itself is never post-processed (PF-014); all escaping happens
/// in [`sanitize_report`], before miette sees the values.
///
/// Never panics (#157). The frame is rendered through `fmt::Write`, which reports a
/// formatting failure as `fmt::Error` where `format!` would panic; when miette's render
/// fails part-way — a source excerpt that cannot be read, say — the partial frame is
/// discarded and [`plain_text_fallback`] renders the report instead.
///
/// Note: idempotency is a property of [`mds::sanitize_control_chars`] (calling it
/// twice on already-sanitized input is a no-op), not of this function (each call
/// re-renders the `Report` from scratch).  That idempotency is what lets
/// `render_diag_human` keep sanitizing its own inputs — it must, because it also
/// neutralizes the source excerpt and filename, which this boundary cannot do.
fn render_error_sanitized(report: miette::Report) -> String {
    let report = sanitize_report(report);
    let mut rendered = String::new();
    match std::fmt::Write::write_fmt(&mut rendered, format_args!("{report:?}")) {
        Ok(()) => rendered,
        Err(std::fmt::Error) => plain_text_fallback(report.as_ref()),
    }
}

/// The plain-text form of a report whose frame miette could not render: its code, its
/// message and its help, each escaped, laid out like miette's frame without the source
/// excerpt. A surface whose `Display` fails is left out, as in [`SanitizedReport`].
fn plain_text_fallback(d: &dyn miette::Diagnostic) -> String {
    let mut out = String::new();
    if let Some(code) = d.code().and_then(|c| display_text(&*c)) {
        out.push_str(&mds::sanitize_control_chars_wire(&code));
        out.push_str("\n\n");
    }
    push_frame_block(&mut out, "  \u{00d7} ", &escape_message(d));
    if let Some(help) = escape_display(d.help()) {
        push_frame_block(&mut out, "  help: ", &help);
    }
    out
}

/// Push `lead` and `text`, continuing each further line of `text` under miette's
/// U+2502 rule, then end the line.
fn push_frame_block(out: &mut String, lead: &str, text: &str) {
    out.push_str(lead);
    out.push_str(&text.replace('\n', "\n  \u{2502} "));
    out.push('\n');
}

/// Render a miette `Report` to stderr — the single choke-point for all CLI error
/// output.
///
/// All per-file error handlers in `main`, `build`, `fmt`, `lint`, and `watch` route
/// error rendering through this helper, so there is exactly one site to audit for
/// escape-injection safety (architecture-6 / avoids PF-004: a check enforced on the
/// primary path silently absent on a sibling path).
///
/// **This function escapes the report's message, help, label text, and auxiliary
/// diagnostic graph itself**, via [`sanitize_report`], for every report it is given —
/// `MdsError` and CLI-authored `miette::miette!()` alike.  Callers do not need to
/// pre-sanitize prose.
///
/// What callers still owe, because this boundary cannot supply it: the
/// [`miette::NamedSource`] attached to the report, whose two halves need different
/// treatments (byte-length-preserving neutralization for the span-indexed source, WIRE
/// escaping for the single-line filename).  Build it with
/// [`mds::named_source_for_render`] — `MdsError::at()` (compiler path),
/// `check_equivalence` (formatter) and `render_diag_human` (lint path) all do.
///
/// miette's own ANSI SGR styling is passed through untouched — carets and box-drawing
/// survive intact.
///
/// Writes through `ewriteln!`, so a closed or failing stderr never panics (#157).
///
/// Note: status-line path display (`Clean:`, `Fixed:`, etc.) is handled by the
/// separate [`safe_path`] helper, not by this function.
pub(crate) fn eprint_error(report: miette::Report) {
    ewriteln!("{}", render_error_sanitized(report));
}

/// Print a CLI warning to stderr with HUMAN-mode escaping applied to the whole line
/// (CWE-150 / PF-004 / #176).
///
/// Applies [`mds::sanitize_control_chars`] (preserves `\n` and `\t`; escapes all other
/// C0 / DEL / C1 controls and bidi / separator / BOM characters to their six-character
/// `\uXXXX` literals) before printing, so a hostile warning message cannot inject ANSI
/// terminal commands into stderr.
///
/// # This helper alone is NOT sufficient — it is the prose half of the rule
///
/// HUMAN mode preserves `\n` on purpose, so that a multi-line warning body still renders
/// as multiple lines. That means routing a hostile *identifier* through this function
/// closes CWE-150 on it and leaves CWE-117 open: `lint.rs`'s unknown-`mds.json`-rule
/// warning already called this helper, and a rule name of
/// `x<LF>Clean: totally-real.mds<LF>0 problems found` still emitted three standalone
/// lines byte-identical in form to genuine status output.
///
/// The governing rule is per FIELD, not per surface (spec §7.5):
///
/// | Part of the line | Mode | Helper |
/// |------------------|------|--------|
/// | warning prose / body | HUMAN | this function |
/// | interpolated filename or path | WIRE | [`safe_path`] |
/// | interpolated identifier, config value, or error cause | WIRE | [`safe_inline`] |
///
/// So the correct shape is `eprint_warning(&format!("warning: … {} …", safe_path(p)))` —
/// both escapes, not either one.
///
/// # Enumeration is not a guarantee — the guard is
///
/// Two earlier revisions of this rustdoc asserted coverage by listing the sites that had
/// been routed. Each list was correct when written and stale by the next review: first
/// `lint.rs`'s rule warning was missed, then the walker's own depth-limit warning inside
/// this very file. A guarantee stated as a property of a code path is only ever a
/// property of the sites someone remembered to enumerate — that is PF-004.
///
/// The property is therefore no longer asserted in prose here. It is enforced by
/// `crates/mds-cli/tests/print_discipline.rs`, which fails if any print macro under
/// `crates/mds-cli/src/**` interpolates a value that is not passed through one of the
/// escape helpers, and which applies the same rule to `format!` invocations nested
/// inside `eprint_warning` calls. `watch.rs`'s lifecycle status lines — previously
/// carved out as a pre-existing gap — are in scope and now routed like everything else.
///
/// Writes through `ewriteln!`, so a closed or failing stderr never panics (#157).
pub(crate) fn eprint_warning(w: &str) {
    ewriteln!("{}", mds::sanitize_control_chars(w));
}

/// Neutralize hostile control bytes in source text for `--diff` preview output.
///
/// `--diff` preview output: neutralized on TTY, byte-faithful when piped.
///
/// **`--diff` only.** `--check` on its own emits no preview text — just `Would fix:` /
/// `Would reformat:` status lines, which are sanitized unconditionally via
/// [`safe_path`] and never reach this function. The only caller is
/// [`render_unified_diff`], shared by `mds fmt --diff` and `mds lint --fix --diff`.
///
/// When `writer_is_tty` is `true`, returns [`neutralize_source_for_render`]`(text)` —
/// a byte-length-preserving substitution that maps C0/DEL/C1 controls and the widened
/// bidi/format hazard class (added in #176) to `?` or U+00A0/U+FFFD so hostile template
/// source cannot inject ANSI terminal commands into the rendered diff (CWE-150).
///
/// When `writer_is_tty` is `false`, returns `Cow::Borrowed(text)` unchanged so
/// redirected diff output (e.g. `mds fmt --diff > patch.diff`) stays byte-faithful
/// and applicable by `patch`/tooling.
///
/// This is a pure, allocation-free helper for clean inputs; the TTY detection
/// (`std::io::stdout().is_terminal()`) is performed at the call site so this function
/// is testable without a real TTY (avoids PF-014: sanitize renderer inputs, not the
/// rendered frame).
///
/// # Byte-length invariant
///
/// `neutralize_source_for_render` is byte-length-preserving — every substitution
/// produces a replacement of the same UTF-8 byte count — so diff hunk byte offsets
/// remain coherent after substitution on the TTY path. Never use
/// [`mds::sanitize_control_chars`] here: it expands 1–2-byte controls to 6 bytes,
/// desynchronising all offsets that follow.
///
/// # Boundary table entry (consistent with `crates/mds-core/src/lint/diagnostic.rs`)
///
/// | Boundary | Mode | Content |
/// |----------|------|---------|
/// | `--diff` preview output | neutralized, TTY-gated | source excerpts via `neutralize_source_for_render`; piped path returns `Cow::Borrowed` |
#[must_use]
pub(crate) fn preview_text_for(writer_is_tty: bool, text: &str) -> Cow<'_, str> {
    if writer_is_tty {
        neutralize_source_for_render(text)
    } else {
        Cow::Borrowed(text)
    }
}

// ── Diff rendering ───────────────────────────────────────────────────────────

/// Render a unified diff between `original` and `modified` with optional colorization.
///
/// The single shared implementation used by both `fmt --diff` and `lint --diff`.
///
/// When stdout is a TTY, both strings are neutralized via [`preview_text_for`] before
/// diffing so hostile ESC/control bytes in template source cannot inject ANSI commands
/// into the rendered diff (CWE-150, security-11). When piped, the strings pass through
/// unchanged so redirected diff output remains byte-faithful and applicable by
/// `patch`/tooling.
#[must_use]
pub(crate) fn render_unified_diff(original: &str, modified: &str, label: &str) -> String {
    let is_tty = std::io::stdout().is_terminal();
    let original = preview_text_for(is_tty, original);
    let modified = preview_text_for(is_tty, modified);
    let diff = similar::TextDiff::from_lines(original.as_ref(), modified.as_ref());
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(label, label)
        .to_string();

    if unified.is_empty() || !is_tty {
        return unified;
    }
    colorize_unified_diff(&unified)
}

/// Colorize a unified diff string with ANSI codes.
///
/// Uses a state machine keyed on the first `@@` hunk marker to correctly color
/// removed (`-`) and added (`+`) lines inside hunks without miscoloring file-header
/// lines (`---`/`+++`) as hunk content when a removed or added line's own content
/// starts with `--` or `++`.
pub(crate) fn colorize_unified_diff(unified: &str) -> String {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const CYAN: &str = "\x1b[36m";
    const RESET: &str = "\x1b[0m";

    let mut out = String::with_capacity(unified.len() + 64);
    let mut in_hunk = false;
    for line in unified.split_inclusive('\n') {
        let color = if line.starts_with("@@") {
            in_hunk = true;
            CYAN
        } else if !in_hunk && (line.starts_with("---") || line.starts_with("+++")) {
            CYAN
        } else if in_hunk && line.starts_with('+') {
            GREEN
        } else if in_hunk && line.starts_with('-') {
            RED
        } else {
            ""
        };
        if color.is_empty() {
            out.push_str(line);
            continue;
        }
        out.push_str(color);
        match line.strip_suffix('\n') {
            Some(stripped) => {
                out.push_str(stripped);
                out.push_str(RESET);
                out.push('\n');
            }
            None => {
                out.push_str(line);
                out.push_str(RESET);
            }
        }
    }
    out
}

/// Sanitize a filesystem path for terminal display (CWE-150 / CWE-117 guard).
///
/// Converts the path to a display string and escapes it with
/// [`mds::escape_path_for_message`] — WIRE mode ([`mds::sanitize_control_chars_wire`])
/// plus `\t`, i.e. every forbidden path character (#265) — so hostile filenames cannot
/// inject ANSI terminal commands (e.g. `ESC[2J`) *or* forge additional status lines,
/// and no status line shows a raw character that a path may not carry.
///
/// # Why WIRE and not HUMAN
///
/// Status lines are single-line by construction: `Clean: {path}`, `Fixed: {path}`,
/// `Compiled to {path}` are emitted unframed and unindented, one per file. POSIX
/// permits a newline inside a filename, and the user never types the name — `mds lint .`
/// discovers it by directory walk. HUMAN mode preserves newlines (so that multi-line
/// diagnostic *messages* keep rendering), which would let a file whose name embeds
/// `<LF>Clean: real.mds<LF>OK: all-fine.mds` emit two attacker-authored lines
/// byte-identical in form to genuine output. A filename is never legitimately
/// multi-line, so escaping the newline to its six-character literal costs nothing and
/// closes the forgery.
///
/// This matches the treatment `files[].file` already gets on the JSON wire surface
/// (`LintResult::to_canonical_json`) and that miette frame headers get via
/// [`mds::named_source_for_render`] — the human status path was the inconsistent one.
///
/// All status-line path interpolations (`Clean:`, `Fixed:`, `Compiled to:`, etc.) in
/// `lint`, `fmt`, and `build` must route through this helper (avoids PF-004 /
/// security-5: unsanitized filename vector in status output).
///
/// Also the CLI's single choke-point for stripping a Windows verbatim prefix
/// (`\\?\C:\…`) before display (#409): a canonicalized `--out-dir` join, or any
/// other canonical path, is passed through [`mds::display_native_path`] first.
/// Off Windows that call is a no-op, so every call site gets the conventional
/// form unconditionally, on every host.
pub(crate) fn safe_path(p: &std::path::Path) -> String {
    safe_file_display(&mds::display_native_path(p).display().to_string())
}

/// [`safe_path`] for a filename that is already a `&str` (e.g. a `LintDiagnostic::file`
/// basename that never became a `Path`).
///
/// Exists so those sites cannot drift into open-coding a different escape mode — the
/// exact PF-004 shape that left `Clean: {filename}` on HUMAN mode while every other
/// status line was on WIRE.
pub(crate) fn safe_file_display(name: &str) -> String {
    mds::escape_path_for_message(name).into_owned()
}

/// WIRE-escape any untrusted value that is interpolated into a **single-line** status,
/// warning, or error line.
///
/// This is the general form of [`safe_path`] / [`safe_file_display`]: the same WIRE
/// escape (those two also escape `\t`, which a path may not carry), for values that are
/// neither a `Path` nor a filename — an `io::Error`
/// `Display` (which embeds a filesystem path), an `mds.json` rule name or config value,
/// a `--format` argument, a fix-rejection reason.
///
/// # Why WIRE, on human surfaces too
///
/// Per the governing per-field rule (spec §7.5): **on the diagnostic surfaces — the
/// `"version": 1` JSON wire, CLI status and warning lines, `[file:line:col]` frame
/// headers — untrusted identifiers, filenames and causes are WIRE-escaped, human output
/// included; only *prose* — a diagnostic message or help body — stays HUMAN.** (Source-map
/// paths and `CompileResult.dependencies` are the named carve-out: not diagnostics, not
/// escaped.) The discriminator is whether the
/// value is legitimately multi-line. A rule name, a path, a `--format` value and an
/// `io::Error` never are; a diagnostic body genuinely is. Leaving `\n` raw in the first
/// group buys nothing and lets the value forge a standalone status line that is
/// byte-identical in form to genuine output (CWE-117).
///
/// The surrounding warning prose stays HUMAN — pass the assembled string to
/// [`eprint_warning`], and WIRE-escape each interpolated value with this helper.
///
/// Idempotent (a property of [`mds::sanitize_control_chars_wire`]), so wrapping a value
/// that was already escaped at construction — e.g.
/// `mds::fix::FixOutcome::Rejected.reason` — is a no-op rather than a double escape.
/// That matters: it lets every call site apply the rule unconditionally instead of
/// tracking which values arrived pre-escaped (PF-004).
///
/// Enforced mechanically by `tests/print_discipline.rs`.
pub(crate) fn safe_inline(value: impl std::fmt::Display) -> String {
    mds::sanitize_control_chars_wire(&value.to_string()).into_owned()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // T-CLI-21 (unit): output_path_for with "json" / "md" extensions.
    #[test]
    fn output_path_for_json_extension_dir_mode() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "json");
        assert_eq!(result, PathBuf::from("/out/src/chat.json"));
    }

    #[test]
    fn output_path_for_md_extension_dir_mode() {
        let source = PathBuf::from("/root/src/page.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, PathBuf::from("/out/src/page.md"));
    }

    #[test]
    fn output_path_for_next_to_source() {
        let source = PathBuf::from("/root/src/page.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::NextToSource;
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, PathBuf::from("/root/src/page.md"));
    }

    #[test]
    fn output_path_for_next_to_source_json() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::NextToSource;
        let result = output_path_for(&source, &root, &base, "json");
        assert_eq!(result, PathBuf::from("/root/src/chat.json"));
    }

    // T-CLI-21 (unit): ..‑containment guard (AC-M7) still holds.
    // When source is outside root, output must be `base/stem.ext`, not escaped.
    #[test]
    fn output_path_for_outside_root_falls_back_to_flat() {
        let source = PathBuf::from("/other/page.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        // Must be inside /out, not escape to /other.
        assert!(
            result.starts_with("/out"),
            "output must be inside /out; got {result:?}"
        );
        assert_eq!(result, PathBuf::from("/out/page.md"));
    }

    /// Body of the first `fn` whose header starts with `header`, brace-matched from the
    /// first `{` after it. Used by the lexical guard below; `None` when the header is
    /// absent, which the caller turns into a non-vacuity failure.
    fn fn_body(src: &str, header: &str) -> Option<String> {
        let start = src.find(header)?;
        let open = start + src[start..].find('{')?;
        let mut depth = 0usize;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(src[open..open + i + 1].to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// #217: the `Dir(_)` oracles must be able to say WHICH arm produced a stem — the
    /// subtree mirror, or the out-of-root flatten that drops the subtree and lets two
    /// sources with the same file name collide.
    ///
    /// Both arms are asserted: an assertion that only ever observes `Flattened` would be
    /// satisfied by a classifier that returns it unconditionally.
    #[test]
    fn mirror_stem_classifies_out_of_root_as_flattened() {
        assert_eq!(
            mirror_stem(
                Path::new("/other/page.mds"),
                Path::new("/root"),
                Path::new("/out"),
            ),
            MirroredStem::Flattened(PathBuf::from("/out/page")),
            "a source outside the root loses its subtree and must say so"
        );
        assert_eq!(
            mirror_stem(
                Path::new("/root/a/page.mds"),
                Path::new("/root"),
                Path::new("/out"),
            ),
            MirroredStem::Mirrored(PathBuf::from("/out/a/page")),
            "a source below the root keeps its subtree and must NOT be reported"
        );
    }

    /// #217: a stem-less source must never hand `d.join` an absolute argument.
    ///
    /// `Path::new("/").file_stem()` is `None`, and the fallback used to be
    /// `source.as_os_str()` — `"/"`. `Path::new("/out").join("/")` re-roots to `"/"`,
    /// and the write oracle then built `"/.md"`. Neither is inside the out-dir, and
    /// neither is a path any source would legitimately compile to.
    ///
    /// `output_path_for_outside_root_falls_back_to_flat` above is the control: a source
    /// that HAS a stem still flattens to `<out-dir>/<stem>.<ext>`.
    #[test]
    fn stemless_source_never_escapes_out_dir() {
        let source = Path::new("/");
        let root = Path::new("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));

        assert_eq!(
            output_base_no_ext(source, root, &base),
            PathBuf::from("/out/output"),
            "the probe oracle must keep a stem-less source inside the out-dir"
        );
        assert_eq!(
            output_path_for(source, root, &base, "md"),
            PathBuf::from("/out/output.md"),
            "the write oracle must not join an absolute stem"
        );
    }

    /// #217: the out-of-root flatten is reported from the WRITE oracle only.
    ///
    /// `output_base_no_ext` is a probe: watch calls it to guess the output siblings of a
    /// source it is about to forget, repeatedly per batch and for sources that are never
    /// written. A warning there would fire on bookkeeping rather than on a write.
    /// `output_path_for` is called once per output path actually computed for a write,
    /// so that is where the report belongs.
    ///
    /// Lexical, because the property being pinned is exactly "which function contains
    /// the call". Both headers must be found, or the two negative assertions would pass
    /// on an empty string.
    #[test]
    fn flatten_warning_lives_in_the_write_oracle_only() {
        const SRC: &str = include_str!("output.rs");
        const NEEDLE: &str = "is outside the build root";

        let oracle = fn_body(SRC, "fn output_path_for(")
            .expect("non-vacuity: fn output_path_for must be present in this file");
        let probe = fn_body(SRC, "fn output_base_no_ext(")
            .expect("non-vacuity: fn output_base_no_ext must be present in this file");

        assert!(
            oracle.contains("eprint_warning("),
            "the write oracle must report the flattened arm; body: {oracle}"
        );
        assert!(
            oracle.contains(NEEDLE),
            "the write oracle's report must name the out-of-root condition; body: {oracle}"
        );
        assert!(
            !probe.contains("eprint_warning("),
            "the probe oracle must stay silent — it runs on bookkeeping, not on writes; \
             body: {probe}"
        );
        assert!(
            !probe.contains(NEEDLE),
            "the probe oracle must not carry the report text either; body: {probe}"
        );
    }

    /// The `.<name>.tmp-<pid>-<n>` temp files an atomic write leaves in flight must
    /// never be collected as sources. The suffix sits AFTER the `.mds`, so
    /// `Path::extension()` is the `tmp-…` component and the walker's extension gate
    /// rejects it — the same gate the dir-mode watch filter uses.
    ///
    /// The second half is the non-vacuity control: a name whose `.mds` is genuinely
    /// last IS collected, so the first assertion is not passing on an empty walk.
    #[test]
    fn collect_mds_files_ignores_write_atomic_temp_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("t.mds"), "real").unwrap();
        std::fs::write(dir.path().join(".t.mds.tmp-4242-7"), "in flight").unwrap();
        let files = collect_mds_files(dir.path(), 64, None);
        assert_eq!(files.len(), 1, "temp file must not be collected: {files:?}");
        // Non-vacuity: the inverted name IS collected.
        std::fs::write(dir.path().join(".tmp-4242-8.t.mds"), "wrong shape").unwrap();
        assert_eq!(collect_mds_files(dir.path(), 64, None).len(), 2);
    }

    #[test]
    fn is_partial_detects_underscore_prefix() {
        assert!(is_partial(Path::new("/dir/_partial.mds")));
        assert!(!is_partial(Path::new("/dir/main.mds")));
        assert!(!is_partial(Path::new("/dir/not_partial.mds")));
    }

    #[test]
    fn partials_only_answers() {
        assert_eq!(partials_only(&[]), None);
        assert_eq!(partials_only(&[PathBuf::from("_a.mds")]), Some(1));
        assert_eq!(
            partials_only(&[PathBuf::from("_a.mds"), PathBuf::from("b.mds")]),
            None
        );
        assert_eq!(
            partials_only(&[PathBuf::from("_a.mds"), PathBuf::from("_b.mds")]),
            Some(2)
        );
    }

    // ── is_default_excluded_dir ───────────────────────────────────────────────

    #[test]
    fn hidden_dir_is_excluded() {
        assert!(is_default_excluded_dir(".git"));
        assert!(is_default_excluded_dir(".cache"));
        assert!(is_default_excluded_dir(".hidden"));
    }

    #[test]
    fn node_modules_is_excluded() {
        assert!(is_default_excluded_dir("node_modules"));
    }

    #[test]
    fn ordinary_dirs_are_not_excluded() {
        assert!(!is_default_excluded_dir("src"));
        assert!(!is_default_excluded_dir("prompts"));
        assert!(!is_default_excluded_dir("templates"));
    }

    // ── is_within_default_excluded_dir ───────────────────────────────────────

    #[test]
    fn path_inside_node_modules_is_excluded() {
        assert!(is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/node_modules/foo.mds")
        ));
    }

    #[test]
    fn path_inside_git_dir_is_excluded() {
        assert!(is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/.git/config")
        ));
    }

    #[test]
    fn path_inside_hidden_subdir_is_excluded() {
        assert!(is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/.cache/something.mds")
        ));
    }

    #[test]
    fn normal_path_under_root_is_not_excluded() {
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/src/main.mds")
        ));
    }

    #[test]
    fn hidden_file_at_root_level_is_not_excluded() {
        // Hidden files at the top level are not inside an excluded DIR.
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/.dotfile.mds")
        ));
    }

    #[test]
    fn path_outside_root_is_not_excluded() {
        // Paths not under root at all are not affected by the root-relative check.
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/other/node_modules/foo.mds")
        ));
    }

    // ── collect_mds_files walker exclusions ───────────────────────────────────

    #[test]
    fn walker_skips_node_modules_subdir() {
        let dir = tempfile::tempdir().unwrap();
        // Create a normal .mds file and one inside node_modules.
        std::fs::write(dir.path().join("main.mds"), "hello").unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("lib.mds"), "lib").unwrap();

        let files = collect_mds_files(dir.path(), 64, None);
        assert_eq!(
            files.len(),
            1,
            "node_modules/lib.mds should be excluded; found: {files:?}"
        );
        assert!(files[0].ends_with("main.mds"));
    }

    #[test]
    fn walker_skips_hidden_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.mds"), "hello").unwrap();
        let hidden = dir.path().join(".git");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("config.mds"), "not a real file").unwrap();

        let files = collect_mds_files(dir.path(), 64, None);
        assert_eq!(
            files.len(),
            1,
            ".git/*.mds should be excluded; found: {files:?}"
        );
    }

    #[test]
    fn walker_collects_hidden_file_at_root_level() {
        // Hidden FILES (not directories) at the traversed level are still collected.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.mds"), "hello").unwrap();
        std::fs::write(dir.path().join(".dotfile.mds"), "dot").unwrap();

        let mut files = collect_mds_files(dir.path(), 64, None);
        files.sort();
        assert_eq!(
            files.len(),
            2,
            "hidden file should still be collected; found: {files:?}"
        );
    }

    #[test]
    fn walker_processes_explicitly_passed_hidden_root() {
        // The root dir itself is always processed even if its name starts with '.'.
        let dir = tempfile::tempdir().unwrap();
        let hidden_root = dir.path().join(".myhidden");
        std::fs::create_dir(&hidden_root).unwrap();
        std::fs::write(hidden_root.join("template.mds"), "hello").unwrap();

        let files = collect_mds_files(&hidden_root, 64, None);
        assert_eq!(
            files.len(),
            1,
            "explicitly-passed hidden root should be processed; found: {files:?}"
        );
    }

    // ── collect_mds_files_detailed / WalkResult ───────────────────────────────

    #[test]
    fn walk_result_empty_dir_has_zero_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let result = collect_mds_files_detailed(dir.path(), 64, None);
        assert_eq!(result.files.len(), 0);
        assert_eq!(
            result.excluded_by_default, 0,
            "genuinely empty dir must have 0 excluded"
        );
    }

    #[test]
    fn walk_result_all_excluded_counts_skipped_files() {
        let dir = tempfile::tempdir().unwrap();
        // All files inside a hidden dir → excluded_by_default > 0, files empty.
        let hidden = dir.path().join(".prompts");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("a.mds"), "a").unwrap();
        std::fs::write(hidden.join("b.mds"), "b").unwrap();

        let result = collect_mds_files_detailed(dir.path(), 64, None);
        assert_eq!(result.files.len(), 0, "no files should be in results");
        assert_eq!(
            result.excluded_by_default, 2,
            "excluded_by_default must equal the count of skipped .mds files; got {}",
            result.excluded_by_default
        );
    }

    #[test]
    fn walk_result_mixed_counts_excluded_and_collects_normal() {
        let dir = tempfile::tempdir().unwrap();
        // One normal file + one in node_modules.
        std::fs::write(dir.path().join("normal.mds"), "hello").unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("excluded.mds"), "lib").unwrap();

        let result = collect_mds_files_detailed(dir.path(), 64, None);
        assert_eq!(result.files.len(), 1, "only normal.mds should be collected");
        assert_eq!(
            result.excluded_by_default, 1,
            "one file in node_modules should be counted as excluded"
        );
    }

    // ── is_within_default_excluded_dir single-component edge case ─────────────

    #[test]
    fn single_component_path_is_not_inside_excluded_dir() {
        // rel = "foo.mds" (single component): rel.parent() = Some(""), which has
        // no file_name(), so the loop terminates without false-positive.
        assert!(!is_within_default_excluded_dir(
            Path::new("/root"),
            Path::new("/root/foo.mds")
        ));
    }

    #[test]
    fn output_base_no_ext_dir_mode() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_base_no_ext(&source, &root, &base);
        assert_eq!(result, PathBuf::from("/out/src/chat"));
    }

    #[test]
    fn output_base_no_ext_next_to_source() {
        let source = PathBuf::from("/root/src/chat.mds");
        let root = PathBuf::from("/root");
        let base = OutputBase::NextToSource;
        let result = output_base_no_ext(&source, &root, &base);
        assert_eq!(result, PathBuf::from("/root/src/chat"));
    }

    // ── preview_text_for: TTY-gated source neutralization (security-11) ──────────

    /// T-12a [security-11 / PF-013]: preview_text_for(true, hostile) neutralizes ESC
    /// and preserves byte length (so diff hunks remain coherent after substitution).
    ///
    /// Positive assertion required by PF-013: the neutralized form MUST be present,
    /// not merely asserted absent in the raw form.
    #[test]
    fn preview_text_for_tty_neutralizes_esc_preserves_byte_length() {
        let hostile = "hello\x1bworld"; // ESC (U+001B) is 1-byte C0
        let result = preview_text_for(true, hostile);
        // Byte-length invariant: neutralize_source_for_render is byte-length-preserving.
        assert_eq!(
            result.len(),
            hostile.len(),
            "byte length must be preserved on TTY path"
        );
        // ESC must be absent from TTY output (security gate).
        assert!(
            !result.contains('\x1b'),
            "raw ESC must be absent from TTY output; got: {result:?}"
        );
        // PF-013 positive assertion: the substituted '?' must be present.
        assert!(
            result.contains('?'),
            "ESC must be replaced with '?' on TTY path; got: {result:?}"
        );
    }

    /// T-12b [security-11 / PF-013]: preview_text_for(false, ...) returns Cow::Borrowed
    /// so piped diff output is byte-identical to the raw source (patch/tooling safety).
    #[test]
    fn preview_text_for_not_tty_returns_borrowed_passthrough() {
        let hostile = "hello\x1bworld";
        let result = preview_text_for(false, hostile);
        // Must be Cow::Borrowed — no allocation, byte-identical to input.
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "piped path must return Cow::Borrowed (no allocation); got Owned"
        );
        assert_eq!(
            result.as_ref(),
            hostile,
            "piped path must be byte-identical to input"
        );
    }

    /// T-12c [security-11 / PF-013]: preview_text_for(true, clean) returns unchanged content
    /// because neutralize_source_for_render returns Cow::Borrowed for clean inputs.
    #[test]
    fn preview_text_for_tty_clean_string_unchanged() {
        let clean = "hello world\n";
        let result = preview_text_for(true, clean);
        assert_eq!(
            result.as_ref(),
            clean,
            "clean string must be unchanged on TTY path"
        );
    }

    // ── safe_path: CWE-150 status-line guard (security-5/6) ──────────────────────

    /// T-11a [security-5 / PF-013]: safe_path escapes a raw ESC byte in a filename
    /// to the 6-char \\uXXXX literal so it cannot inject ANSI into a status line.
    #[test]
    fn safe_path_sanitizes_esc_byte() {
        let raw = format!("dir/fo{}o.mds", '\x1b');
        let p = std::path::Path::new(&raw);
        let result = safe_path(p);
        assert!(
            !result.contains('\x1b'),
            "raw ESC must be absent from safe_path output"
        );
        assert!(
            result.contains("\\u001B"),
            "ESC must be escaped to \\u001B; got: {result:?}"
        );
    }

    /// T-11b [security-5 / PF-013]: safe_path passes a clean path through unchanged
    /// (no unnecessary allocation or mutation).
    #[test]
    fn safe_path_passes_clean_path_unchanged() {
        let p = std::path::Path::new("dir/normal.mds");
        assert_eq!(safe_path(p), "dir/normal.mds");
    }

    /// #265: a path is escaped for the whole forbidden-path class, TAB included —
    /// WIRE mode alone leaves a TAB raw, and a status line showing a path must carry
    /// none of the 80 codepoints.
    #[test]
    fn safe_path_escapes_every_forbidden_char_tab_included() {
        for ch in ['\t', '\n', '\x1b', '\u{202E}'] {
            let raw = format!("out{ch}dir/in.md");
            let got = safe_path(std::path::Path::new(&raw));
            assert!(!got.contains(ch), "U+{:04X}: {got:?}", u32::from(ch));
            assert_eq!(
                got,
                format!("out\\u{:04X}dir/in.md", u32::from(ch)),
                "U+{:04X}",
                u32::from(ch)
            );
        }
    }

    // ── T-10a/b/c: neutralize_source_for_render + colour path (── PF-014) ────────

    /// T-10a [PF-014 / AC-F2]: neutralize_source_for_render removes C0 controls
    /// (except \n/\t), DEL, and 2-byte C1 controls while preserving total byte length
    /// so that miette span offsets remain valid after substitution.
    #[test]
    fn neutralize_source_removes_c0_del_c1_preserving_byte_length() {
        // ASCII NUL (C0), DEL (0x7F), and U+0085 NEL (C1, 2-byte UTF-8) are hostile.
        // \n and \t are allowed through unchanged.
        let raw = "a\x00b\x7fc\u{0085}d\ne\tf";
        let out = neutralize_source_for_render(raw);
        // Byte length must be identical (span safety invariant).
        assert_eq!(out.len(), raw.len(), "byte length preserved");
        // Hostile bytes are replaced; safe bytes survive.
        assert!(!out.contains('\x00'), "NUL removed");
        assert!(!out.contains('\x7f'), "DEL removed");
        assert!(!out.contains('\u{0085}'), "C1 NEL removed");
        assert!(out.contains('\n'), "LF preserved");
        assert!(out.contains('\t'), "TAB preserved");
    }

    /// T-10b [reliability-8 / PF-014]: When the span starts AFTER a control character
    /// the substituted byte at that position must still form a valid char boundary so
    /// miette can slice the excerpt without panicking.
    #[test]
    fn neutralize_source_caret_alignment_with_span_after_control_char() {
        use miette::{GraphicalReportHandler, GraphicalTheme, NamedSource, SourceSpan};

        // "a<ESC>bc" where span covers 'b' (byte offset 2..3, AFTER the ESC byte).
        // If neutralization breaks the byte-length invariant, miette panics here.
        let raw = "a\x1bbc";
        let clean = neutralize_source_for_render(raw);
        assert_eq!(clean.len(), raw.len(), "byte length invariant");

        // Build a minimal miette report whose source excerpt exercises the span.
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("test")]
        struct SpanErr {
            #[source_code]
            src: NamedSource<String>,
            #[label("here")]
            span: SourceSpan,
        }

        let report = miette::Report::new(SpanErr {
            src: NamedSource::new("test.mds", clean.into_owned()),
            span: (2, 1).into(), // byte 2..3 = 'b'
        });

        let mut buf = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .render_report(&mut buf, report.as_ref())
            .expect("render must not panic after neutralization");
        // The caret must point at 'b', not produce garbage.
        assert!(buf.contains('b'), "caret points at b");
        // No raw ESC byte survives into the rendered output.
        assert!(!buf.contains('\x1b'), "no raw ESC in rendered output");
    }

    /// T-10c [testing-2 / PF-014]: miette's own SGR colour codes survive
    /// (render_error_sanitized never post-processes the frame) while a hostile
    /// OSC sequence embedded in the source is neutralised at the input stage.
    #[test]
    fn colour_path_miette_sgr_survives_hostile_osc_is_removed() {
        use miette::{GraphicalReportHandler, GraphicalTheme, NamedSource, SourceSpan};

        // A 2-byte C1 U+009D = OSC opener (hostile). After neutralize it becomes
        // U+00A0 NBSP, same byte length; no OSC survives into the render.
        // U+009D in UTF-8 is 0xC2 0x9D (2 bytes). We use the char directly.
        let hostile_char = '\u{009D}'; // C1 OSC opener
        let raw = format!("good{}text", hostile_char);
        let clean = neutralize_source_for_render(&raw);
        assert_eq!(clean.len(), raw.len(), "byte length preserved");
        assert!(
            !clean.contains(hostile_char),
            "C1 OSC neutralised in source"
        );

        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("colour test")]
        struct ColourErr {
            #[source_code]
            src: NamedSource<String>,
            #[label("here")]
            span: SourceSpan,
        }

        let src_len = clean.len();
        let report = miette::Report::new(ColourErr {
            src: NamedSource::new("colour.mds", clean.into_owned()),
            span: (0, src_len).into(),
        });

        // Colour-enabled renderer — miette will emit real SGR codes.
        let mut coloured = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode())
            .render_report(&mut coloured, report.as_ref())
            .expect("render must not panic");

        // miette's own ANSI codes must survive in the coloured output.
        assert!(
            coloured.contains('\x1b'),
            "miette SGR codes present in coloured output"
        );
        // But the hostile C1 byte is gone from the rendered string.
        assert!(
            !coloured.contains(hostile_char),
            "hostile C1 absent from rendered output"
        );
    }

    // ── sanitize_report: the CLI human message/help boundary (CWE-150 / #176) ────
    //
    // These are in-process unit tests so they can pin the COLOUR path, which the
    // subprocess e2e tests in `tests/security.rs` are structurally blind to (they set
    // `NO_COLOR=1` and pipe stderr).  Per the H4 test-determinism decision, colour is
    // selected explicitly via `GraphicalTheme` rather than inherited from the
    // environment.

    /// Render `report` through a colour-neutral handler — the deterministic in-process
    /// equivalent of what `render_error_sanitized` emits under `NO_COLOR=1`.
    fn render_nocolor(report: &miette::Report) -> String {
        use miette::{GraphicalReportHandler, GraphicalTheme};
        let mut buf = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .render_report(&mut buf, report.as_ref())
            .expect("render must not panic");
        buf
    }

    /// T-ESC-1 [security-11 / PF-013 / #176]: an `MdsError` whose message *and* help
    /// both interpolate attacker-controlled text is escaped on both surfaces before
    /// miette renders it.
    ///
    /// `UndefinedVariable` is the vector because `name` lands in the `#[error(...)]`
    /// message and in the `#[diagnostic(help(...))]` text, so one input exercises both.
    #[test]
    fn sanitize_report_escapes_message_and_help() {
        let hostile_name = format!("bad{}[31mNAME", '\u{1b}');
        let report = sanitize_report(miette::Report::new(mds::MdsError::UndefinedVariable {
            name: hostile_name,
            span: None,
            src: None,
        }));
        let rendered = render_nocolor(&report);

        // Non-vacuity: the diagnostic actually rendered, with its help line.
        assert!(
            rendered.contains("undefined variable"),
            "non-vacuity: expected the undefined-variable message; got: {rendered:?}"
        );
        assert!(
            rendered.contains("help:"),
            "non-vacuity: expected a help line; got: {rendered:?}"
        );
        // Negative.
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must not survive into the rendered frame; got: {rendered:?}"
        );
        // Positive: escaped on BOTH surfaces, so the count is at least two.
        assert!(
            rendered.matches("\\u001B").count() >= 2,
            "ESC must be escaped in the message AND the help text; got: {rendered:?}"
        );
    }

    /// T-ESC-2 [security-11 / PF-004 / #176]: a CLI-authored `miette::miette!()` report
    /// — which does NOT downcast to `MdsError` — is escaped by the same boundary.
    #[test]
    fn sanitize_report_escapes_cli_authored_miette_message() {
        let hostile = format!("cannot write fo{}[2Jo.mds", '\u{1b}');
        let report = sanitize_report(miette::miette!("{hostile}"));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("cannot write"),
            "non-vacuity: expected the miette!() message; got: {rendered:?}"
        );
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must not survive a miette!() report; got: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u001B"),
            "ESC must be escaped to the \\u001B literal; got: {rendered:?}"
        );
    }

    /// T-ESC-3 [security-11 / #176]: the widened hazard class (bidi controls, Trojan
    /// Source CVE-2021-42574) is escaped on this boundary too, not just C0/DEL/C1.
    #[test]
    fn sanitize_report_escapes_bidi_override_in_message() {
        let hostile = format!("alias{}reversed", '\u{202e}');
        let report = sanitize_report(miette::miette!("{hostile}"));
        let rendered = render_nocolor(&report);

        assert!(
            !rendered.contains('\u{202e}'),
            "raw U+202E must not survive; got: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u202E"),
            "U+202E must be escaped to the \\u202E literal; got: {rendered:?}"
        );
    }

    /// T-ESC-4 [PF-014 / #176]: HUMAN mode — a real newline in the message survives, so
    /// multi-line diagnostic frames stay readable.  This is the one deliberate
    /// difference from the WIRE boundary (`MdsError::serialize`), which escapes `\n`.
    #[test]
    fn sanitize_report_preserves_newline_in_message() {
        let report = sanitize_report(miette::miette!("line one\nline two"));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("line one"),
            "non-vacuity: message must render; got: {rendered:?}"
        );
        assert!(
            !rendered.contains("\\u000A"),
            "HUMAN mode must NOT escape newlines; got: {rendered:?}"
        );
    }

    /// T-ESC-5 [security-11 / #176]: label TEXT is escaped while the label's byte SPAN
    /// is forwarded verbatim, so the caret still lands on the right columns.
    ///
    /// `LintDiagnostic::labels()` uses its own message as the label text, so this is a
    /// real untrusted-text surface and not a hypothetical one.
    #[test]
    fn sanitize_report_escapes_label_text_but_keeps_span() {
        let hostile_label = format!("here{}[31m", '\u{1b}');
        let diag = miette::MietteDiagnostic::new("outer message")
            .with_label(miette::LabeledSpan::at(2..5, hostile_label));
        let report =
            sanitize_report(miette::Report::new(diag).with_source_code("abcdefgh".to_string()));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("here"),
            "non-vacuity: the label must render; got: {rendered:?}"
        );
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must not survive in label text; got: {rendered:?}"
        );
        assert!(
            rendered.contains("\\u001B"),
            "label ESC must be escaped; got: {rendered:?}"
        );
        // Span geometry preserved: the source line and its caret still render.
        assert!(
            rendered.contains("abcdefgh"),
            "the labelled source excerpt must still render; got: {rendered:?}"
        );
    }

    /// T-ESC-6 [PF-014 / #176]: the regression this boundary exists to avoid.
    ///
    /// Rendering the sanitized report with a COLOUR theme must leave miette's own ANSI
    /// SGR codes intact (real ESC bytes present) while the hostile input is escaped to
    /// a literal.  An implementation that sanitized the rendered frame instead would
    /// strip miette's SGR into literal `\u001b[...` noise — this test fails loudly in
    /// that case, and the subprocess e2e tests cannot (they pin `NO_COLOR=1`).
    #[test]
    fn sanitize_report_colour_path_keeps_miette_sgr_and_escapes_hostile_input() {
        use miette::{GraphicalReportHandler, GraphicalTheme};

        let hostile = format!("hostile{}[31mtext", '\u{1b}');
        let report = sanitize_report(miette::miette!("{hostile}"));

        let mut coloured = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode())
            .render_report(&mut coloured, report.as_ref())
            .expect("render must not panic");

        // miette's own SGR codes survive — the frame was never post-processed.
        assert!(
            coloured.contains('\u{1b}'),
            "miette's own SGR codes must survive on the colour path; got: {coloured:?}"
        );
        // The hostile input is still escaped.
        assert!(
            coloured.contains("\\u001B"),
            "hostile ESC must be escaped even on the colour path; got: {coloured:?}"
        );
        // And miette's SGR was NOT itself escaped: the only escaped literal is the one
        // hostile byte, not the ~10 SGR sequences the frame contains.
        assert_eq!(
            coloured.matches("\\u001B").count(),
            1,
            "exactly one escaped literal (the hostile byte) — more means miette's own \
             SGR codes were escaped, which is the PF-014 regression; got: {coloured:?}"
        );
    }

    /// T-ESC-7 [#176]: clean input renders byte-identically with and without the
    /// sanitizing wrapper, so the boundary is inert on the overwhelmingly common path.
    #[test]
    fn sanitize_report_is_inert_on_clean_input() {
        let raw = miette::Report::new(mds::MdsError::UndefinedVariable {
            name: "user_name".to_string(),
            span: None,
            src: None,
        });
        let before = render_nocolor(&raw);
        let after = render_nocolor(&sanitize_report(raw));
        assert_eq!(
            before, after,
            "sanitizing must not alter the render of a clean diagnostic"
        );
    }

    // ── sanitize_report: the auxiliary diagnostic graph (PF-005 / #176) ─────────
    //
    // `source()` / `related()` / `diagnostic_source()` used to be reported as absent,
    // guarded only by a `debug_assert!` that no CLI error populates them. `debug_assert!`
    // is compiled out of release, so that invariant was real in tests and ABSENT in the
    // shipped binary: the first error type to grow a `#[source]` field would have had
    // its cause chain silently dropped from release stderr while CI stayed green.
    //
    // These tests are pure functional assertions on the `Diagnostic` impl, so they hold
    // identically in debug and release — which is the point.

    /// A two-link cause chain: an outer diagnostic whose `#[source]` carries hostile text.
    #[derive(Debug, thiserror::Error, miette::Diagnostic)]
    #[error("outer failure")]
    struct OuterWithCause {
        #[source]
        cause: InnerCause,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("inner cause: {0}")]
    struct InnerCause(String);

    /// A diagnostic that hangs a hostile `related` diagnostic off itself.
    #[derive(Debug, thiserror::Error, miette::Diagnostic)]
    #[error("parent diagnostic")]
    struct ParentWithRelated {
        #[related]
        related: Vec<RelatedChild>,
    }

    #[derive(Debug, thiserror::Error, miette::Diagnostic)]
    #[error("related child: {0}")]
    #[diagnostic(help("child help: {0}"))]
    struct RelatedChild(String);

    /// The hostile fragment shared by the auxiliary-graph tests: one C0 byte, one
    /// 3-byte bidi control, and the 2-byte bidi control #176 added.
    fn hostile_fragment() -> String {
        format!("bad{}[31m{}{}end", '\u{1b}', '\u{202e}', '\u{061c}')
    }

    /// Negative + positive assertions shared by T-AUX-1/2/3.
    fn assert_aux_text_escaped(rendered: &str, surface: &str) {
        for raw in ['\u{1b}', '\u{202e}', '\u{061c}'] {
            assert!(
                !rendered.contains(raw),
                "{surface}: raw U+{:04X} must not survive into the rendered frame; \
                 got: {rendered:?}",
                raw as u32
            );
        }
        // Uppercase literals from a lowercase-hex source vector: proof the byte really
        // decoded and was really escaped rather than passing through as literal text.
        for escaped in ["\\u001B", "\\u202E", "\\u061C"] {
            assert!(
                rendered.contains(escaped),
                "{surface}: {escaped} must appear in the rendered frame; got: {rendered:?}"
            );
        }
    }

    /// T-AUX-1 [PF-005 / security-11 / PF-013 / #176]: a report carrying a `#[source]`
    /// cause chain renders that cause, escaped — neither leaked raw nor dropped.
    #[test]
    fn sanitize_report_escapes_and_preserves_the_cause_chain() {
        let report = sanitize_report(miette::Report::new(OuterWithCause {
            cause: InnerCause(hostile_fragment()),
        }));
        let rendered = render_nocolor(&report);

        // Non-vacuity: the cause is actually rendered. This is the assertion that fails
        // if `source()` reverts to returning `None` — the release-build silent drop.
        assert!(
            rendered.contains("inner cause"),
            "non-vacuity: the cause chain must still render; got: {rendered:?}"
        );
        assert!(
            rendered.contains("outer failure"),
            "non-vacuity: the outer message must still render; got: {rendered:?}"
        );
        assert_aux_text_escaped(&rendered, "cause chain");
    }

    /// T-AUX-2 [PF-005 / #176]: `related()` diagnostics — message AND help — are
    /// escaped and preserved.
    #[test]
    fn sanitize_report_escapes_and_preserves_related_diagnostics() {
        let report = sanitize_report(miette::Report::new(ParentWithRelated {
            related: vec![RelatedChild(hostile_fragment())],
        }));
        let rendered = render_nocolor(&report);

        assert!(
            rendered.contains("parent diagnostic"),
            "non-vacuity: the parent message must render; got: {rendered:?}"
        );
        assert!(
            rendered.contains("related child"),
            "non-vacuity: the related diagnostic must still render; got: {rendered:?}"
        );
        assert!(
            rendered.contains("child help"),
            "non-vacuity: the related diagnostic's help must still render; got: {rendered:?}"
        );
        assert_aux_text_escaped(&rendered, "related diagnostics");
    }

    /// T-AUX-3 [PF-005 / #176]: the walk is depth-bounded, so a self-referential
    /// `source()` cannot hang or overflow the stack during rendering.
    ///
    /// `Cycle::source()` returns `self`, an infinite chain. Construction must terminate.
    #[test]
    fn sanitize_report_bounds_a_cyclic_cause_chain() {
        #[derive(Debug)]
        struct Cycle;
        impl std::fmt::Display for Cycle {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("cyclic cause")
            }
        }
        impl std::error::Error for Cycle {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&Cycle)
            }
        }
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("outer")]
        struct Outer(#[source] Cycle);

        let wrapped = SanitizedReport::new(miette::Report::new(Outer(Cycle)));

        // Walk the materialised chain and assert it is finite and within the bound.
        let mut depth = 0usize;
        let mut node = wrapped.source.as_ref();
        while let Some(n) = node {
            depth += 1;
            assert!(
                depth <= MAX_AUX_DEPTH,
                "the cause-chain walk must be bounded by MAX_AUX_DEPTH"
            );
            node = n.source.as_deref();
        }
        // Non-vacuity: the bound was actually reached, so this is not passing because
        // the chain was empty.
        assert_eq!(
            depth, MAX_AUX_DEPTH,
            "an infinite chain must be truncated at exactly MAX_AUX_DEPTH"
        );
    }

    // ── eprint_warning: the CLI warning sanitization boundary (CWE-150 / PF-004 / #176) ──
    //
    // eprint_warning is a thin wrapper around mds::sanitize_control_chars + ewriteln!.
    // The tests below exercise the transformation directly (the pure function that the
    // wrapper applies) to keep assertions deterministic without capturing stderr.
    //
    // Test strategy — corrected.
    //
    // An earlier revision of this block claimed that "the only warning that interpolates
    // untrusted text is the resolver warning" (which needs a MAX_SOURCEMAP_SEGMENTS
    // overflow in a hostile-named module, so an e2e vector for it would be contrived),
    // and concluded that no e2e test was required because "every warning print in
    // main.rs and build.rs now calls eprint_warning". Both halves were false, and the
    // unreachability argument was the load-bearing one:
    //
    //   * lint.rs's unknown-`mds.json`-rule warning (added in e145e41) interpolates an
    //     arbitrary JSON object key — reachable in about thirty seconds by writing an
    //     mds.json into any linted directory. It is covered e2e by T-ESC-RULE-1 in
    //     tests/security.rs, whose vector now carries newlines as well as C0 and bidi
    //     controls, because routing it through eprint_warning (HUMAN, `\n` preserved)
    //     closed CWE-150 on it while leaving the CWE-117 line forgery open.
    //   * The walker's own depth-limit warning, ~40 lines up in THIS file, printed
    //     `dir.display()` through a bare eprintln!. It is neither main.rs nor build.rs,
    //     so the enumeration above walked straight past it. Covered e2e by T-ESC-WALK-1.
    //
    // The unit tests below stay — they pin the transformation deterministically without
    // capturing stderr — but they are no longer offered as a substitute for e2e vectors.
    //
    // Coverage is now enforced rather than enumerated: tests/print_discipline.rs fails if
    // ANY print macro under crates/mds-cli/src interpolates a value that is not passed
    // through safe_path / safe_file_display / safe_inline / sanitize_control_chars*, and
    // applies the same rule to `format!`s nested inside eprint_warning calls. That is
    // what makes a claim about warning-path coverage checkable instead of remembered.
    //
    // Guard-removal RED evidence (T-WARN-1):
    //   Replace `mds::sanitize_control_chars(&hostile)` with
    //   `std::borrow::Cow::Borrowed(hostile.as_str())` in the test body.
    //   The test fails with:
    //     assertion `left == right` failed: raw ESC must be absent from warning output
    //     left: true
    //     right: false
    //   because the unsanitized string still contains '\u{1b}'.
    //   Restoring the call makes the test green.

    /// T-WARN-1 [security-11 / PF-004 / PF-013 / #176]: a hostile warning string
    /// (one that interpolates a filename containing a raw ESC byte) is sanitized to
    /// its `\uXXXX` literal form — raw ESC is absent and the escaped form is present.
    ///
    /// This tests the exact transformation that `eprint_warning` applies before printing.
    #[test]
    fn eprint_warning_sanitizes_hostile_control_chars() {
        // Simulate the only real-world hostile vector: the resolver warning that
        // interpolates an untrusted module filename (mds-core/src/resolver.rs).
        let hostile = format!(
            "MAX_SOURCEMAP_SEGMENTS exceeded in imported module 'lib{}[2Jbar.mds'",
            '\u{1b}'
        );
        // This is exactly what eprint_warning applies before calling ewriteln!.
        let result = mds::sanitize_control_chars(&hostile);

        // Non-vacuity: the plain-text content is preserved.
        assert!(
            result.contains("MAX_SOURCEMAP_SEGMENTS"),
            "non-vacuity: warning text must be preserved; got: {result:?}"
        );
        assert!(
            result.contains("lib"),
            "non-vacuity: filename prefix must be preserved; got: {result:?}"
        );
        // Negative: raw ESC absent.
        assert!(
            !result.contains('\u{1b}'),
            "raw ESC must be absent from warning output; hostile = {hostile:?}"
        );
        // Positive: escaped form present.
        assert!(
            result.contains("\\u001B"),
            "ESC must be escaped to \\u001B literal; got: {result:?}"
        );
    }

    /// T-WARN-2 [PF-013 / #176]: a clean warning string passes through unchanged —
    /// the sanitization is inert on the overwhelmingly common path.
    ///
    /// This corresponds to the "clean string passes through unchanged" requirement from
    /// the task brief: eprint_warning must never mutate a warning that contains no
    /// hazardous bytes.
    #[test]
    fn eprint_warning_clean_string_passes_through_unchanged() {
        let clean = "MAX_SOURCEMAP_SEGMENTS exceeded in imported module 'lib.mds'";
        let result = mds::sanitize_control_chars(clean);
        assert_eq!(
            result.as_ref(),
            clean,
            "clean warning must pass through unchanged; got: {result:?}"
        );
    }

    /// T-WARN-3 [PF-013 / #176]: the widened hazard class (bidi controls, Trojan
    /// Source CVE-2021-42574) is escaped by eprint_warning too — not just C0/ESC.
    #[test]
    fn eprint_warning_sanitizes_bidi_control_in_warning_text() {
        // U+202E RIGHT-TO-LEFT OVERRIDE: injecting this into a warning message
        // reverses how the rest of the terminal line renders in bidi-aware terminals.
        let hostile = format!("warning: module 'lib{}evil.mds'", '\u{202e}');
        let result = mds::sanitize_control_chars(&hostile);
        assert!(
            !result.contains('\u{202e}'),
            "raw U+202E must be absent from warning output; got: {result:?}"
        );
        assert!(
            result.contains("\\u202E"),
            "U+202E must be escaped to \\u202E literal; got: {result:?}"
        );
    }

    // ── render_unified_diff / colorize_unified_diff ───────────────────────────

    #[test]
    fn render_unified_diff_empty_for_identical_input() {
        let rendered = render_unified_diff("same\n", "same\n", "label");
        assert!(rendered.is_empty());
    }

    #[test]
    fn render_unified_diff_contains_unified_markers_for_changed_input() {
        let rendered = render_unified_diff("a\n", "b\n", "label");
        assert!(rendered.contains("---"));
        assert!(rendered.contains("+++"));
        assert!(rendered.contains("-a"));
        assert!(rendered.contains("+b"));
    }

    #[test]
    fn colorize_unified_diff_wraps_add_remove_lines_with_ansi_when_requested() {
        let unified = "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n";
        let colorized = colorize_unified_diff(unified);
        assert!(colorized.contains("\x1b[32m+new\x1b[0m"));
        assert!(colorized.contains("\x1b[31m-old\x1b[0m"));
        assert!(colorized.contains("\x1b[36m--- a\x1b[0m"));
    }

    #[test]
    fn colorize_unified_diff_correctly_colors_content_starting_with_dashes_or_pluses() {
        // Regression: a removed line whose content starts with "-- " produces
        // "--- ..." in the rendered unified diff. The old global prefix check
        // matched `starts_with("---")` and mis-colored it CYAN (file header)
        // instead of RED (removal). Same defect for "++" content → "+++ " →
        // mis-colored CYAN instead of GREEN.
        let unified = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n--- dashes content\n+++ plus content\n";
        let colorized = colorize_unified_diff(unified);
        // Inside the hunk: removal of a line whose content starts with "-- "
        assert!(colorized.contains("\x1b[31m--- dashes content\x1b[0m"));
        // Inside the hunk: addition of a line whose content starts with "++"
        assert!(colorized.contains("\x1b[32m+++ plus content\x1b[0m"));
        // File headers (before @@) must still be CYAN
        assert!(colorized.contains("\x1b[36m--- a\x1b[0m"));
        assert!(colorized.contains("\x1b[36m+++ b\x1b[0m"));
    }

    // ── atomic_write_file ─────────────────────────────────────────────────────

    /// Names of leftover `.mds-tmp-*` entries directly inside `dir`.
    fn temp_residue(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".mds-tmp-"))
            .collect()
    }

    /// Creates a symlink for a test, tolerating Windows' unprivileged restriction.
    ///
    /// Mirrors `crates/mds-core/src/lib.rs`'s crate-internal helper of the same
    /// name and contract (#147): Unix needs no privilege; Windows needs Developer
    /// Mode or an elevated process (GitHub's `windows-latest` runners have
    /// Developer Mode enabled, so a failure there is a genuine regression and
    /// must panic), and only the unprivileged local case — `CI` unset plus raw
    /// OS error 1314 (`ERROR_PRIVILEGE_NOT_HELD`) — is a skip. Duplicated rather
    /// than shared because this crate has no unit-test-scope helper module.
    fn make_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let result = if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
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
                        ewriteln!(
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

    /// T-U1: `mds build` writes artifacts that do not exist yet (#227). The
    /// primitive must create the target instead of failing the existence probe.
    #[test]
    fn atomic_write_file_creates_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("fresh.md");
        assert!(!target.exists(), "precondition: target must be absent");

        atomic_write_file(&target, "CREATED", Durability::Fsync)
            .expect("writing an absent target must succeed");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "CREATED");
        let residue = temp_residue(dir.path());
        assert!(
            residue.is_empty(),
            "no .mds-tmp- residue may survive a successful write; got {residue:?}"
        );
    }

    /// T-U9: `Durability::RenameOnly` changes ONLY whether the temp file is fsynced.
    /// Everything the callers rely on — the content, the mode of a freshly created
    /// artifact, the symlink refusal, and leaving no temp residue — must be identical
    /// to `Fsync` (#227). The fsync itself is not observable from a passing process;
    /// what this pins is that skipping it did not quietly relax anything else.
    #[test]
    fn atomic_write_file_rename_only_matches_fsync_contract() {
        let dir = tempfile::tempdir().unwrap();

        // Fresh target: created, with the same content and mode as the Fsync sibling.
        let quick = dir.path().join("quick.md");
        let synced = dir.path().join("synced.md");
        atomic_write_file(&quick, "DERIVED", Durability::RenameOnly).unwrap();
        atomic_write_file(&synced, "DERIVED", Durability::Fsync).unwrap();
        assert_eq!(std::fs::read_to_string(&quick).unwrap(), "DERIVED");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&quick).unwrap().permissions().mode() & 0o777,
                std::fs::metadata(&synced).unwrap().permissions().mode() & 0o777,
                "RenameOnly must not change the mode a fresh artifact is created with"
            );
        }

        // Existing target: replaced, previous content gone.
        atomic_write_file(&quick, "REBUILT", Durability::RenameOnly).unwrap();
        assert_eq!(std::fs::read_to_string(&quick).unwrap(), "REBUILT");

        // Symlink target: still refused (the fsync is not what enforces this).
        {
            let real = dir.path().join("real.md");
            std::fs::write(&real, "REAL").unwrap();
            let link = dir.path().join("link.md");
            if !make_symlink(&real, &link) {
                return;
            }
            let err = atomic_write_file(&link, "NEW", Durability::RenameOnly)
                .expect_err("RenameOnly must still refuse a symlink target");
            assert!(
                err.to_string().contains("symlink"),
                "the refusal must say why; got: {err}"
            );
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "REAL");
        }

        let residue = temp_residue(dir.path());
        assert!(
            residue.is_empty(),
            "RenameOnly must leave no .mds-tmp- residue; got {residue:?}"
        );
    }

    /// T-U2: a freshly created artifact must carry the same mode `std::fs::write`
    /// would have produced (`0666 & !umask`), not `tempfile`'s owner-only 0600.
    /// The sibling control makes the assertion umask-independent.
    ///
    /// `#[cfg(unix)]`: Unix permission mode bits (`PermissionsExt::mode`) have no
    /// Windows equivalent — the permission model differs (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_new_file_mode_matches_std_fs_write() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.md");
        let ctl = dir.path().join("ctl.md");

        atomic_write_file(&out, "X", Durability::Fsync).unwrap();
        std::fs::write(&ctl, "X").unwrap();

        let mode_out = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        let mode_ctl = std::fs::metadata(&ctl).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode_out, mode_ctl,
            "new-file mode must match std::fs::write; got 0{mode_out:o} vs control 0{mode_ctl:o}"
        );
    }

    /// T-U3: an existing file keeps its mode across the replace-by-rename cycle.
    ///
    /// `#[cfg(unix)]`: Unix permission mode bits have no Windows equivalent (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_existing_mode_0640_preserved() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("src.mds");
        std::fs::write(&target, "OLD").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();

        atomic_write_file(&target, "NEW", Durability::Fsync).unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode, 0o640,
            "existing mode must be preserved; got 0{mode:o}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW");
    }

    /// T-U4: a symlink at the target is refused, never written through. The
    /// control writes the symlink's own target directly and must succeed, so the
    /// refusal is not passing on an unrelated failure.
    #[test]
    fn atomic_write_file_refuses_live_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        std::fs::write(&real, "REAL").unwrap();
        if !make_symlink(&real, &link) {
            return;
        }

        let err = atomic_write_file(&link, "NEW", Durability::Fsync)
            .expect_err("writing through a symlink must be refused");
        assert!(
            matches!(err, mds::MdsError::Io { .. }),
            "a refused write is mds::io, exit 2 (#157); got {err:?}"
        );
        let err = err.to_string();
        assert!(
            err.contains("symlink"),
            "expected a symlink refusal; got {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "REAL",
            "the symlink's target must not be written through"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must survive the refusal"
        );

        // CONTROL: the same directory and content, addressed at the real file.
        atomic_write_file(&real, "NEW", Durability::Fsync)
            .expect("writing the real file must succeed");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "NEW");
    }

    /// T-U5: a dangling symlink is still a symlink — refuse it rather than
    /// materialising the missing file it points at.
    #[test]
    fn atomic_write_file_refuses_dangling_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.md");
        let link = dir.path().join("link.md");
        if !make_symlink(&missing, &link) {
            return;
        }

        let err = atomic_write_file(&link, "NEW", Durability::Fsync)
            .expect_err("writing through a dangling symlink must be refused")
            .to_string();
        assert!(
            err.contains("symlink"),
            "expected a symlink refusal; got {err}"
        );
        assert!(
            !missing.exists(),
            "the dangling link's target must not be created"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must survive the refusal"
        );
    }

    /// T-U6: a failed write leaves the original inode, bytes and mtime untouched
    /// and drops the temp file. The control proves the same call succeeds once
    /// the directory is writable again, and that success DOES replace the inode.
    ///
    /// `#[cfg(unix)]`: provokes the failure via chmod (Unix permission bits) and
    /// asserts on `MetadataExt::ino()`, neither of which exists on Windows —
    /// the read-only attribute there does not block creating files in a
    /// directory, so the same setup would not provoke a write failure (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_failure_preserves_original_and_leaves_no_temp() {
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("locked.mds");
        std::fs::write(&target, "OLD").unwrap();

        let before = std::fs::metadata(&target).unwrap();
        let (ino, mtime) = (before.ino(), before.modified().unwrap());

        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = atomic_write_file(&target, "NEW", Durability::Fsync);
        // Restore before asserting so a failed assertion cannot leave an
        // undeletable tempdir behind.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = result
            .expect_err("a read-only parent directory must fail the write")
            .to_string();
        assert!(
            err.contains("locked.mds"),
            "error must name the target; got {err}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "OLD");
        let after = std::fs::metadata(&target).unwrap();
        assert_eq!(
            after.ino(),
            ino,
            "a failed write must not replace the inode"
        );
        assert_eq!(
            after.modified().unwrap(),
            mtime,
            "a failed write must not touch the mtime"
        );
        let residue = temp_residue(&sub);
        assert!(
            residue.is_empty(),
            "failed write left temp residue: {residue:?}"
        );

        // CONTROL: writable again — the same call succeeds and swaps the inode.
        atomic_write_file(&target, "NEW", Durability::Fsync)
            .expect("write must succeed once the dir is writable");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "NEW");
        assert_ne!(
            std::fs::metadata(&target).unwrap().ino(),
            ino,
            "replace-by-rename must produce a new inode"
        );
    }

    /// T-U7: a directory at the target is an error, not a clobber, and leaves no
    /// temp file behind in the parent.
    #[test]
    fn atomic_write_file_directory_target_refused_without_residue() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("adir");
        std::fs::create_dir(&target).unwrap();

        let err = atomic_write_file(&target, "X", Durability::Fsync)
            .expect_err("a directory target must not be written")
            .to_string();
        assert!(
            err.contains("adir"),
            "error must name the target; got {err}"
        );
        assert!(target.is_dir(), "the directory must survive the refusal");
        let residue = temp_residue(dir.path());
        assert!(
            residue.is_empty(),
            "refused write left temp residue: {residue:?}"
        );
    }

    /// T-U8: a stat failure that is NOT `NotFound` is a hard error — never a
    /// warning followed by a write with a guessed mode (#225).
    ///
    /// `#[cfg(unix)]`: provokes the stat failure with a `0o000`-mode parent
    /// directory; Windows' permission model does not block traversal the same
    /// way, so this setup would not provoke the failure there (#147).
    #[cfg(unix)]
    #[test]
    fn atomic_write_file_unreadable_parent_is_hard_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nosearch");
        std::fs::create_dir(&p).unwrap();
        // Planted before the chmod so the root probe below has something to stat.
        let probe = p.join("probe");
        std::fs::write(&probe, "").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::metadata(&probe).is_ok() {
            // Root bypasses the mode bits; EACCES cannot be provoked here.
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            ewriteln!("running as root; cannot exercise EACCES");
            return;
        }

        let result = atomic_write_file(&p.join("x.md"), "X", Durability::Fsync);
        // Restore before asserting so tempdir cleanup always succeeds.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = result
            .expect_err("an unstattable target must be a hard error")
            .to_string();
        assert!(
            err.contains("cannot stat"),
            "expected a stat error; got {err}"
        );
        assert!(
            err.contains("x.md"),
            "error must name the target; got {err}"
        );
    }

    // ── render_error_sanitized never panics on a failing Display (#157) ─────────

    /// The six-character escape `sanitize_control_chars` writes for ESC, built at runtime
    /// so no escape sequence is written into this source.
    fn escaped_esc() -> String {
        format!("{}u001B", '\\')
    }

    /// A `Display` that always fails, standing in for a buggy third-party impl.
    struct Unformattable;

    impl std::fmt::Display for Unformattable {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
    }

    /// Source text that serves a label's surrounding context but fails the exact-span
    /// read miette makes next, so miette's own render returns `fmt::Error` part-way
    /// through the frame.
    #[derive(Debug)]
    struct FailsNarrowReads(String);

    impl miette::SourceCode for FailsNarrowReads {
        fn read_span<'a>(
            &'a self,
            span: &miette::SourceSpan,
            context_lines_before: usize,
            context_lines_after: usize,
        ) -> std::result::Result<Box<dyn miette::SpanContents<'a> + 'a>, miette::MietteError>
        {
            if context_lines_before == 0 && context_lines_after == 0 {
                return Err(miette::MietteError::OutOfBounds);
            }
            miette::SourceCode::read_span(&self.0, span, context_lines_before, context_lines_after)
        }
    }

    /// A cause whose message cannot be formatted.
    #[derive(Debug)]
    struct UnformattableCause;

    impl std::fmt::Display for UnformattableCause {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            Err(std::fmt::Error)
        }
    }

    impl std::error::Error for UnformattableCause {}

    /// A diagnostic whose surfaces fail to format one at a time, as each test asks.
    #[derive(Debug, Default)]
    struct Probe {
        message: String,
        message_fails: bool,
        code_fails: bool,
        help_fails: bool,
        cause: Option<UnformattableCause>,
        source: Option<FailsNarrowReads>,
    }

    impl std::fmt::Display for Probe {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            if self.message_fails {
                return Err(std::fmt::Error);
            }
            f.write_str(&self.message)
        }
    }

    impl std::error::Error for Probe {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_ref()
                .map(|c| c as &(dyn std::error::Error + 'static))
        }
    }

    impl miette::Diagnostic for Probe {
        fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
            if self.code_fails {
                Some(Box::new(Unformattable))
            } else {
                Some(Box::new("mds::probe"))
            }
        }

        fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
            if self.help_fails {
                Some(Box::new(Unformattable))
            } else {
                Some(Box::new("probe help"))
            }
        }

        fn source_code(&self) -> Option<&dyn miette::SourceCode> {
            self.source.as_ref().map(|s| s as &dyn miette::SourceCode)
        }

        fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
            self.source.as_ref()?;
            Some(Box::new(std::iter::once(
                miette::LabeledSpan::new_primary_with_span(Some("here".to_string()), (0, 5)),
            )))
        }
    }

    /// A report whose own message cannot be formatted still renders — its code and help
    /// in the usual frame, a fixed placeholder where the message would be.
    #[test]
    fn a_report_whose_message_cannot_be_formatted_renders_a_placeholder() {
        let rendered = render_error_sanitized(miette::Report::new(Probe {
            message_fails: true,
            ..Probe::default()
        }));

        assert!(
            rendered.contains("mds::probe") && rendered.contains("probe help"),
            "the rest of the frame must render; got {rendered:?}"
        );
        assert!(
            rendered.contains("could not be formatted"),
            "the message's place must say it could not be formatted; got {rendered:?}"
        );
    }

    /// A code, help or cause whose `Display` fails costs only that surface: the message
    /// still renders, escaped, in the usual frame.
    #[test]
    fn a_report_whose_code_help_or_cause_cannot_be_formatted_keeps_its_message() {
        let rendered = render_error_sanitized(miette::Report::new(Probe {
            message: format!("cannot write fo{}[2Jo.mds", '\x1b'),
            code_fails: true,
            help_fails: true,
            cause: Some(UnformattableCause),
            ..Probe::default()
        }));

        assert!(
            rendered.contains("cannot write fo") && rendered.contains(&escaped_esc()),
            "the message must render, with ESC escaped; got {rendered:?}"
        );
        assert!(
            !rendered.contains('\x1b'),
            "no raw ESC may reach the rendered text; got {rendered:?}"
        );
        assert!(
            rendered.contains("could not be formatted"),
            "the unformattable cause must leave a placeholder, not vanish; got {rendered:?}"
        );
    }

    /// When miette's own render fails part-way (here: a source read fails after the
    /// frame has started), the whole frame is replaced by escaped plain text — never a
    /// panic, never half a frame.
    #[test]
    fn a_render_failure_falls_back_to_escaped_plain_text() {
        let rendered = render_error_sanitized(miette::Report::new(Probe {
            message: format!("cannot write fo{}[2Jo.mds\nsecond line", '\x1b'),
            source: Some(FailsNarrowReads("Hello world\n".to_string())),
            ..Probe::default()
        }));

        let esc = escaped_esc();
        let want = format!(
            "mds::probe\n\n  \u{00d7} cannot write fo{esc}[2Jo.mds\n  \u{2502} second line\n  help: probe help\n"
        );
        assert_eq!(
            rendered, want,
            "a failed render must fall back to the plain-text form, escaped"
        );
    }

    // ── The stderr writer, the stdout outcome and the exit code (#157) ──────────

    /// What a [`Sink`]'s `write` does.
    #[derive(Clone, Copy)]
    enum OnWrite {
        Accept,
        Fail(std::io::ErrorKind),
        /// Accept nothing: `write` returns `Ok(0)`, which `write_all` reports as
        /// `WriteZero`.
        Zero,
    }

    /// An in-memory stream whose `write` and `flush` fail as a test asks, counting the
    /// `write` calls so a dropped write can be told apart from a failed one.
    struct Sink {
        on_write: OnWrite,
        flush_fails: Option<std::io::ErrorKind>,
        writes: usize,
        bytes: Vec<u8>,
    }

    impl Sink {
        fn new(on_write: OnWrite) -> Self {
            Self {
                on_write,
                flush_fails: None,
                writes: 0,
                bytes: Vec::new(),
            }
        }

        fn failing_flush(kind: std::io::ErrorKind) -> Self {
            Self {
                flush_fails: Some(kind),
                ..Self::new(OnWrite::Accept)
            }
        }
    }

    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            match self.on_write {
                OnWrite::Accept => {
                    self.bytes.extend_from_slice(buf);
                    Ok(buf.len())
                }
                OnWrite::Fail(kind) => Err(std::io::Error::from(kind)),
                OnWrite::Zero => Ok(0),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self.flush_fails {
                Some(kind) => Err(std::io::Error::from(kind)),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn stderr_writer_writes_the_whole_text_and_records_nothing() {
        let state = OutputState::new();
        let mut sink = Sink::new(OnWrite::Accept);
        write_stderr_to(&state, &mut sink, format_args!("OK: {}\n", "x.mds"));

        assert_eq!(sink.bytes, b"OK: x.mds\n");
        assert_eq!(sink.writes, 1, "the rendered text goes out in one write");
        assert!(!state.stderr_closed() && !state.io_failed());
    }

    #[test]
    fn stderr_writer_treats_a_closed_pipe_as_sticky_and_drops_later_writes() {
        let state = OutputState::new();
        let mut closed = Sink::new(OnWrite::Fail(std::io::ErrorKind::BrokenPipe));
        write_stderr_to(&state, &mut closed, format_args!("first\n"));
        assert!(state.stderr_closed(), "a closed pipe must be recorded");
        assert!(!state.io_failed(), "a closed pipe is not an I/O failure");

        let mut later = Sink::new(OnWrite::Accept);
        write_stderr_to(&state, &mut later, format_args!("second\n"));
        assert_eq!(
            later.writes, 0,
            "every write after the pipe closed must be dropped"
        );
    }

    #[test]
    fn stderr_writer_records_any_other_failure_and_keeps_writing() {
        let state = OutputState::new();
        let mut full = Sink::new(OnWrite::Fail(std::io::ErrorKind::StorageFull));
        write_stderr_to(&state, &mut full, format_args!("first\n"));
        assert!(state.io_failed(), "a non-pipe failure must be recorded");
        assert!(!state.stderr_closed(), "a full disk is not a closed pipe");

        let mut later = Sink::new(OnWrite::Accept);
        write_stderr_to(&state, &mut later, format_args!("second\n"));
        assert_eq!(
            later.bytes, b"second\n",
            "an I/O failure must not stop later writes"
        );
        assert!(state.io_failed(), "the failure stays recorded");
    }

    #[test]
    fn stderr_writer_counts_a_short_write_and_a_failed_flush() {
        let short = OutputState::new();
        write_stderr_to(&short, &mut Sink::new(OnWrite::Zero), format_args!("x\n"));
        assert!(short.io_failed() && !short.stderr_closed());

        let flush_full = OutputState::new();
        let mut sink = Sink::failing_flush(std::io::ErrorKind::StorageFull);
        write_stderr_to(&flush_full, &mut sink, format_args!("x\n"));
        assert!(flush_full.io_failed() && !flush_full.stderr_closed());

        let flush_closed = OutputState::new();
        let mut sink = Sink::failing_flush(std::io::ErrorKind::BrokenPipe);
        write_stderr_to(&flush_closed, &mut sink, format_args!("x\n"));
        assert!(flush_closed.stderr_closed() && !flush_closed.io_failed());
    }

    #[test]
    fn stderr_writer_survives_a_display_that_fails() {
        let state = OutputState::new();
        let mut sink = Sink::new(OnWrite::Accept);
        write_stderr_to(
            &state,
            &mut sink,
            format_args!("before {} after\n", Unformattable),
        );

        assert_eq!(
            sink.bytes, b"before ",
            "the text ends where the Display failed"
        );
        assert!(
            !state.stderr_closed() && !state.io_failed(),
            "a formatting bug is not an output failure"
        );
    }

    #[test]
    fn write_stdout_is_written_only_when_the_write_and_the_flush_succeed() {
        let state = OutputState::new();
        let mut ok = Sink::new(OnWrite::Accept);
        assert!(matches!(
            write_stdout_to(&state, &mut ok, b"compiled\n"),
            StdoutOutcome::Written
        ));
        assert_eq!(ok.bytes, b"compiled\n");
        assert!(
            !state.stdout_closed() && !state.io_failed(),
            "a write that succeeds records nothing"
        );

        let cases = [
            (
                "write hits a closed pipe",
                Sink::new(OnWrite::Fail(std::io::ErrorKind::BrokenPipe)),
                None,
            ),
            (
                "flush hits a closed pipe",
                Sink::failing_flush(std::io::ErrorKind::BrokenPipe),
                None,
            ),
            (
                "write fails",
                Sink::new(OnWrite::Fail(std::io::ErrorKind::StorageFull)),
                Some(std::io::ErrorKind::StorageFull),
            ),
            (
                "write is short",
                Sink::new(OnWrite::Zero),
                Some(std::io::ErrorKind::WriteZero),
            ),
            (
                "flush fails",
                Sink::failing_flush(std::io::ErrorKind::StorageFull),
                Some(std::io::ErrorKind::StorageFull),
            ),
        ];
        for (what, mut sink, failed_kind) in cases {
            let outcome = write_stdout_to(&OutputState::new(), &mut sink, b"compiled\n");
            match (failed_kind, &outcome) {
                (None, StdoutOutcome::Closed) => {}
                (Some(want), StdoutOutcome::Failed(e)) if e.kind() == want => {}
                _ => panic!("{what}: want Closed or Failed({failed_kind:?}); got {outcome:?}"),
            }
        }
    }

    /// Once stdout's reader is gone, nothing more is written to it: a later write, to
    /// the same stream or to one that would accept it, is `Closed` without a byte
    /// reaching the stream (#157).
    #[test]
    fn write_stdout_writes_nothing_once_stdout_is_closed() {
        let state = OutputState::new();
        let mut closed = Sink::new(OnWrite::Fail(std::io::ErrorKind::BrokenPipe));
        let first = write_stdout_to(&state, &mut closed, b"first\n");
        assert!(state.stdout_closed(), "a closed stdout must be recorded");
        assert!(!state.io_failed(), "a closed pipe is not an I/O failure");
        let second = write_stdout_to(&state, &mut closed, b"second\n");
        let mut later = Sink::new(OnWrite::Accept);
        let third = write_stdout_to(&state, &mut later, b"third\n");
        assert_eq!(
            (closed.writes, later.writes),
            (1, 0),
            "no write may follow a closed pipe; outcomes {first:?}, {second:?}, {third:?}"
        );
        assert!(
            matches!(
                (&first, &second, &third),
                (
                    StdoutOutcome::Closed,
                    StdoutOutcome::Closed,
                    StdoutOutcome::Closed
                )
            ),
            "got {first:?}, {second:?}, {third:?}"
        );
    }

    /// A stdout that fails for another reason is reported once while it keeps failing:
    /// the first failed write is `Failed`, and a later one that fails again is not, so a
    /// batch run reports the failure once. Every write is still attempted (#157).
    #[test]
    fn write_stdout_reports_only_the_first_other_failure() {
        let state = OutputState::new();
        // A success first: it must not use up the one report.
        let _ = write_stdout_to(&state, &mut Sink::new(OnWrite::Accept), b"ok\n");
        let mut full = Sink::new(OnWrite::Fail(std::io::ErrorKind::StorageFull));
        let first = write_stdout_to(&state, &mut full, b"first\n");
        let second = write_stdout_to(&state, &mut full, b"second\n");
        assert!(
            matches!(&first, StdoutOutcome::Failed(e) if e.kind() == std::io::ErrorKind::StorageFull),
            "the first failure is reported; got {first:?}"
        );
        assert!(
            !matches!(second, StdoutOutcome::Failed(_)),
            "a second failure must not be reported again; got {second:?}"
        );
        assert_eq!(full.writes, 2, "a write after a failure is still attempted");
        assert!(
            second.into_batch_result().is_ok(),
            "a batch run does not report a repeated failure"
        );

        let mut ok = Sink::new(OnWrite::Accept);
        let third = write_stdout_to(&state, &mut ok, b"third\n");
        assert!(
            matches!(third, StdoutOutcome::Written) && ok.bytes == b"third\n",
            "a write that succeeds after a failure is written; got {third:?}"
        );
        assert!(
            !state.stdout_closed() && !state.io_failed(),
            "the writer records only the stdout failure; the caller's report records the \
             I/O failure for the exit code"
        );
    }

    /// A stdout failure is reported once per failure episode: a write that lands ends
    /// the episode, so the next failure is new and reported again — a `mds watch -o -`
    /// session must not go silent on a stdout that failed, recovered and failed again.
    /// A write of no bytes proves nothing and ends nothing. The recorded I/O failure
    /// that lifts a batch run's exit code outlives the recovery (#157).
    #[test]
    fn write_stdout_reports_a_new_failure_after_stdout_recovers() {
        let state = OutputState::new();
        let mut full = Sink::new(OnWrite::Fail(std::io::ErrorKind::StorageFull));
        let first = write_stdout_to(&state, &mut full, b"one\n");
        assert!(
            matches!(&first, StdoutOutcome::Failed(e) if e.kind() == std::io::ErrorKind::StorageFull),
            "the first failure is reported; got {first:?}"
        );
        // What fmt and lint do with a reported failure: record it for the exit code.
        state.note_io_failure();
        let repeat = write_stdout_to(&state, &mut full, b"two\n");
        assert!(
            matches!(repeat, StdoutOutcome::FailedAgain),
            "a repeat before stdout recovers is not reported; got {repeat:?}"
        );

        let mut empty = Sink::new(OnWrite::Accept);
        let nothing = write_stdout_to(&state, &mut empty, b"");
        assert!(matches!(nothing, StdoutOutcome::Written), "got {nothing:?}");
        let still = write_stdout_to(&state, &mut full, b"three\n");
        assert!(
            matches!(still, StdoutOutcome::FailedAgain),
            "a write of no bytes does not show that stdout recovered; got {still:?}"
        );

        let mut ok = Sink::new(OnWrite::Accept);
        let recovered = write_stdout_to(&state, &mut ok, b"four\n");
        assert!(
            matches!(recovered, StdoutOutcome::Written) && ok.bytes == b"four\n",
            "stdout recovers; got {recovered:?}"
        );

        let again = write_stdout_to(&state, &mut full, b"five\n");
        assert!(
            matches!(&again, StdoutOutcome::Failed(e) if e.kind() == std::io::ErrorKind::StorageFull),
            "a failure after stdout recovered is a new one, reported again; got {again:?}"
        );
        let repeat_again = write_stdout_to(&state, &mut full, b"six\n");
        assert!(
            matches!(repeat_again, StdoutOutcome::FailedAgain),
            "the new failure's repeat is not reported; got {repeat_again:?}"
        );
        assert_eq!(full.writes, 5, "every write is attempted");

        assert!(
            state.io_failed() && !state.stdout_closed(),
            "the recovery clears neither the recorded I/O failure nor anything else"
        );
        assert_eq!(
            final_exit_code(0, &state, ExitPolicy::Batch),
            IO_FAILURE_EXIT,
            "a batch run that lost a stdout write still exits 2 after stdout recovered"
        );
    }

    /// A batch run keeps going past a closed stdout, and reports any other stdout
    /// failure as one `mds::io` error naming stdout (#157).
    #[test]
    fn a_batch_run_ignores_a_closed_stdout_and_reports_any_other_failure() {
        assert!(StdoutOutcome::Written.into_batch_result().is_ok());
        assert!(StdoutOutcome::Closed.into_batch_result().is_ok());
        assert!(
            StdoutOutcome::FailedAgain.into_batch_result().is_ok(),
            "a repeated failure was already reported"
        );

        let failed = StdoutOutcome::Failed(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "no space left",
        ))
        .into_batch_result();
        match failed {
            Err(mds::MdsError::Io { message }) => {
                assert_eq!(message, "cannot write to stdout: no space left");
            }
            other => panic!("want Err(MdsError::Io {{ .. }}); got {other:?}"),
        }
    }

    /// Every combination of verdict, recorded facts and policy that matters, with the
    /// code each one must exit with — written out, not recomputed.
    #[test]
    fn final_exit_code_table() {
        struct Case {
            verdict: i32,
            closed: bool,
            failed: bool,
            policy: ExitPolicy,
            want: i32,
        }
        const fn case(
            verdict: i32,
            closed: bool,
            failed: bool,
            policy: ExitPolicy,
            want: i32,
        ) -> Case {
            Case {
                verdict,
                closed,
                failed,
                policy,
                want,
            }
        }
        use ExitPolicy::{Batch, WatchSession};
        let cases = [
            // Nothing recorded: the verdict stands.
            case(0, false, false, Batch, 0),
            case(1, false, false, Batch, 1),
            case(2, false, false, Batch, 2),
            case(3, false, false, Batch, 3),
            case(101, false, false, Batch, 101),
            // A closed pipe never changes the code.
            case(0, true, false, Batch, 0),
            case(1, true, false, Batch, 1),
            case(3, true, false, Batch, 3),
            // Any other failure lifts a batch run to at least 2.
            case(0, false, true, Batch, 2),
            case(1, false, true, Batch, 2),
            case(2, false, true, Batch, 2),
            case(3, false, true, Batch, 3),
            case(101, false, true, Batch, 101),
            case(0, true, true, Batch, 2),
            case(1, true, true, Batch, 2),
            // A live watch session keeps its verdict whatever was recorded.
            case(0, false, false, WatchSession, 0),
            case(1, false, false, WatchSession, 1),
            case(0, true, false, WatchSession, 0),
            case(0, false, true, WatchSession, 0),
            case(1, false, true, WatchSession, 1),
            case(3, false, true, WatchSession, 3),
            case(0, true, true, WatchSession, 0),
        ];
        for c in cases {
            let state = OutputState::new();
            if c.closed {
                state.note_stderr_closed();
            }
            if c.failed {
                state.note_io_failure();
            }
            assert_eq!(
                final_exit_code(c.verdict, &state, c.policy),
                c.want,
                "verdict {} closed={} failed={} {:?}",
                c.verdict,
                c.closed,
                c.failed,
                c.policy
            );
        }
    }

    /// A run exits under the batch rule until a watch session goes live, and under the
    /// session rule from then on — whenever its output failure was recorded (#157).
    #[test]
    fn the_session_rule_applies_only_once_a_watch_session_is_live() {
        let state = OutputState::new();
        assert_eq!(
            state.exit_policy(),
            ExitPolicy::Batch,
            "a fresh run is a batch"
        );
        state.note_io_failure();
        assert_eq!(
            final_exit_code(0, &state, state.exit_policy()),
            2,
            "before a session goes live, an output failure lifts the exit code"
        );

        state.note_watch_live();
        assert_eq!(state.exit_policy(), ExitPolicy::WatchSession);
        assert_eq!(
            final_exit_code(0, &state, state.exit_policy()),
            0,
            "a live session keeps its verdict, the failure recorded before it included"
        );
        state.note_io_failure();
        state.note_watch_live();
        assert_eq!(
            final_exit_code(1, &state, state.exit_policy()),
            1,
            "going live is sticky, and a failure recorded after it changes nothing"
        );
    }

    // ── Panics (#389) ─────────────────────────────────────────────────────────

    /// A panic makes the run exit 101, whatever its verdict, whatever else it recorded,
    /// and under either policy — a live watch session's included.
    #[test]
    fn a_panic_exits_101_whatever_else_was_recorded() {
        use ExitPolicy::{Batch, WatchSession};
        // (verdict, stderr closed, I/O failed, policy)
        let cases = [
            (0, false, false, Batch),
            (1, false, false, Batch),
            (2, false, true, Batch),
            (3, false, true, Batch),
            (0, true, false, Batch),
            (0, false, false, WatchSession),
            (1, true, true, WatchSession),
            (3, false, true, WatchSession),
        ];
        for (verdict, closed, failed, policy) in cases {
            let state = OutputState::new();
            if closed {
                state.note_stderr_closed();
            }
            if failed {
                state.note_io_failure();
            }
            assert!(
                final_exit_code(verdict, &state, policy) < 101,
                "control: verdict {verdict} closed={closed} failed={failed} {policy:?} \
                 exits below 101 until a panic is recorded"
            );
            state.note_panicked();
            assert!(state.panicked());
            assert_eq!(
                final_exit_code(verdict, &state, policy),
                101,
                "verdict {verdict} closed={closed} failed={failed} {policy:?}"
            );
        }
    }

    /// `RUST_BACKTRACE` asks for a backtrace when it is set to anything but `0`, and for
    /// every frame when it is `full` — as std reads it.
    #[test]
    fn rust_backtrace_picks_the_backtrace_a_panic_prints() {
        let cases = [
            (None, None),
            (Some("0"), None),
            (Some("1"), Some(BacktraceStyle::Short)),
            (Some("full"), Some(BacktraceStyle::Full)),
            (Some(""), Some(BacktraceStyle::Short)),
            (Some("short"), Some(BacktraceStyle::Short)),
        ];
        for (value, want) in cases {
            assert_eq!(
                BacktraceStyle::from_env(value.map(OsStr::new)),
                want,
                "RUST_BACKTRACE={value:?}"
            );
        }
    }

    /// The text is two fixed lines: that the CLI failed, and the issue tracker of the
    /// repository its manifest names.
    #[test]
    fn the_internal_compiler_error_text_names_the_issue_tracker() {
        let lines: Vec<&str> = ICE_TEXT.lines().collect();
        assert_eq!(lines.len(), 2, "{ICE_TEXT:?}");
        assert!(ICE_TEXT.ends_with('\n'), "{ICE_TEXT:?}");
        assert_eq!(lines[0], "mds: internal compiler error");
        let tracker = concat!(env!("CARGO_PKG_REPOSITORY"), "/issues");
        assert!(
            tracker.starts_with("https://github.com/") && tracker.len() > 26,
            "the manifest names a GitHub repository: {tracker}"
        );
        assert!(lines[1].ends_with(tracker), "{ICE_TEXT:?}");
        assert!(
            ICE_TEXT.chars().all(|c| c == '\n' || !c.is_control()),
            "{ICE_TEXT:?}"
        );
    }

    /// Each line of a backtrace is WIRE-escaped on its own and keeps its line break: an
    /// ESC, a CR or a bidi override inside a line becomes its escape; a tab stays.
    #[test]
    fn a_backtrace_is_escaped_line_by_line() {
        let esc = char::from(0x1b);
        let cr = char::from(0x0d);
        let rlo = char::from_u32(0x202E).expect("U+202E");
        let code = |c: char| format!("{}u{:04X}", '\\', u32::from(c));
        let text = format!("frame {esc}[2J\n\tat {rlo}file.rs{cr}\nlast");
        assert_eq!(
            escape_each_line(&text),
            format!(
                "frame {}[2J\n\tat {}file.rs{}\nlast\n",
                code(esc),
                code(rlo),
                code(cr)
            )
        );
        // Control: clean text comes back as it is, each line ended by its line break.
        assert_eq!(escape_each_line("a\nb\n"), "a\nb\n");
    }

    /// `write_backtrace` writes a header and this thread's frames — the writer's own among
    /// them — every line escaped, in one write; a write that fails is not retried and does
    /// not panic.
    #[test]
    fn write_backtrace_writes_escaped_frames_and_ignores_a_failed_write() {
        for style in [BacktraceStyle::Short, BacktraceStyle::Full] {
            let mut sink = Sink::new(OnWrite::Accept);
            write_backtrace(&mut sink, style);
            let text = String::from_utf8(sink.bytes).expect("a backtrace is UTF-8");
            assert!(text.starts_with("stack backtrace:\n"), "{style:?}: {text}");
            assert!(
                text.contains("write_backtrace"),
                "{style:?}: the frames include the writer's own; {text}"
            );
            assert!(
                text.split_terminator('\n')
                    .all(|line| mds::sanitize_control_chars_wire(line) == line),
                "{style:?}: every line is escaped; {text}"
            );
            assert_eq!(sink.writes, 1, "{style:?}: one write");
            for kind in [
                std::io::ErrorKind::BrokenPipe,
                std::io::ErrorKind::StorageFull,
            ] {
                let mut failing = Sink::new(OnWrite::Fail(kind));
                write_backtrace(&mut failing, style);
                assert_eq!(
                    failing.writes, 1,
                    "{style:?}: a failed write is not retried"
                );
            }
        }
    }

    /// The trigger's payload is one no panic output may show: the sentinel, a raw ESC and
    /// the absolute path of the working directory.
    #[cfg(debug_assertions)]
    #[test]
    fn the_trigger_s_payload_carries_a_sentinel_an_esc_and_an_absolute_path() {
        let payload = panic_trigger::payload();
        let here = std::env::current_dir().expect("the working directory");
        assert!(here.is_absolute(), "{here:?}");
        assert!(payload.contains(panic_trigger::SENTINEL), "{payload:?}");
        assert!(payload.contains(char::from(0x1b)), "{payload:?}");
        assert!(payload.contains(&here.display().to_string()), "{payload:?}");
    }

    /// The variable that makes a test run as the child [`panic_in_a_child`] starts.
    const PANIC_CHILD: &str = "MDS_OUTPUT_PANIC_CHILD";

    /// Run the test `name` again as a child — this test binary, that one test — with
    /// [`PANIC_CHILD`] set, so that it installs the panic hook in a process of its own.
    /// The child's `RUST_BACKTRACE` is removed: CI sets it for every job.
    fn panic_in_a_child(name: &str) -> std::process::Output {
        let binary = std::env::current_exe().expect("the test binary's path");
        std::process::Command::new(binary)
            .args([name, "--exact", "--nocapture", "--test-threads=1"])
            .env(PANIC_CHILD, name)
            .env_remove("RUST_BACKTRACE")
            .output()
            .expect("the child runs")
    }

    fn in_the_child(name: &str) -> bool {
        std::env::var_os(PANIC_CHILD).is_some_and(|value| value == name)
    }

    /// The child's streams: what the harness and the test wrote to stdout, and stderr.
    fn streams(child: &std::process::Output) -> (String, String) {
        (
            String::from_utf8_lossy(&child.stdout).into_owned(),
            String::from_utf8_lossy(&child.stderr).into_owned(),
        )
    }

    /// A panic that unwinds to [`catch_panic`] is reported once and caught, and the run
    /// goes on: the thread is not left unwinding, so a second panic is caught as well,
    /// and the run then ends through [`exit`] with 101 (#389).
    ///
    /// Control: the child really ran the test (the harness announced it), and ended at the
    /// funnel, not in the harness.
    #[test]
    fn a_caught_panic_is_reported_once_and_the_run_goes_on() {
        const NAME: &str = "output::tests::a_caught_panic_is_reported_once_and_the_run_goes_on";
        if in_the_child(NAME) {
            install_panic_hook();
            let first = catch_panic(|| {
                panic!("first");
            });
            let second = catch_panic(|| {
                panic!("second");
            });
            if first.is_err() && second.is_err() {
                let _ = write_stdout(b"after both catches\n");
            }
            exit(0);
        }
        let child = panic_in_a_child(NAME);
        let (report, shown) = streams(&child);
        assert!(
            report.contains("running 1 test"),
            "control: the child ran the test; stdout:\n{report}"
        );
        assert_eq!(
            child.status.code(),
            Some(101),
            "a run that caught a panic exits 101; stdout:\n{report}\nstderr:\n{shown}"
        );
        assert!(
            report.contains("after both catches") && !report.contains("test result"),
            "both panics were caught and the run went on to the exit; stdout:\n{report}"
        );
        assert_eq!(shown, ICE_TEXT.repeat(2), "one text per panic");
    }

    /// A panic that nothing catches ends the process at once with 101, after the text —
    /// before the test harness, which would catch it, sees it.
    #[test]
    fn a_panic_nothing_catches_ends_the_run_at_once_with_101() {
        const NAME: &str = "output::tests::a_panic_nothing_catches_ends_the_run_at_once_with_101";
        if in_the_child(NAME) {
            install_panic_hook();
            panic!("uncaught");
        }
        let child = panic_in_a_child(NAME);
        let (report, shown) = streams(&child);
        assert!(
            report.contains("running 1 test"),
            "control: the child ran the test; stdout:\n{report}"
        );
        assert_eq!(
            child.status.code(),
            Some(101),
            "stdout:\n{report}\nstderr:\n{shown}"
        );
        assert!(
            !report.contains("test result"),
            "the process ended at the panic, before the harness could report it; \
             stdout:\n{report}"
        );
        assert_eq!(shown, ICE_TEXT, "the text, once");
    }

    /// A panic while the thread is unwinding from another — a value that panics when it
    /// is dropped — ends the process at once with 101 instead of the abort std would make
    /// of it; each panic prints the text.
    #[test]
    fn a_panic_while_unwinding_ends_the_run_with_101_not_an_abort() {
        const NAME: &str =
            "output::tests::a_panic_while_unwinding_ends_the_run_with_101_not_an_abort";
        struct PanicsWhenDropped;
        impl Drop for PanicsWhenDropped {
            fn drop(&mut self) {
                panic!("second");
            }
        }
        if in_the_child(NAME) {
            install_panic_hook();
            let _ = catch_panic(|| {
                let _armed = PanicsWhenDropped;
                panic!("first");
            });
            let _ = write_stdout(b"the run went on\n");
            exit(0);
        }
        let child = panic_in_a_child(NAME);
        let (report, shown) = streams(&child);
        assert!(
            report.contains("running 1 test"),
            "control: the child ran the test; stdout:\n{report}"
        );
        assert_eq!(
            child.status.code(),
            Some(101),
            "exit 101, not an abort (a signal: None); stdout:\n{report}\nstderr:\n{shown}"
        );
        assert!(
            !report.contains("the run went on") && !report.contains("test result"),
            "the process ended at the second panic; stdout:\n{report}"
        );
        assert_eq!(shown, ICE_TEXT.repeat(2), "one text per panic");
    }
}
