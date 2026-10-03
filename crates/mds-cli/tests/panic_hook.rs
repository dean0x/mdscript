//! The CLI's panic output (#389).
//!
//! A panic in `mds` is an internal compiler error. The run prints two fixed lines on
//! stderr — that it failed, and where to report it — and exits 101. The panic's message
//! and location are never shown: a panic message can carry a user's text or a build
//! machine's absolute paths. With `RUST_BACKTRACE` set to anything but `0`, a backtrace
//! follows the two lines, every line escaped; it still never shows the message.
//!
//! # The trigger
//!
//! A debug build of `mds` panics on purpose when `MDS_TEST_PANIC` asks it to: `main`
//! panics in the command's dispatch, `thread` in a thread the dispatch starts and waits
//! for, `compile:<stem>` in the compile of a file with that file stem — one file of a
//! directory run or a watch session, which goes on without it — and `notify` / `ctrlc`
//! in `mds watch`'s file-event callback / Ctrl-C handler, each on a thread of its own.
//! The payload carries a sentinel word, a raw ESC and the absolute path of the working
//! directory — a fresh temporary directory in every test here — so a test can tell that
//! none of it reached stderr. A release build compiles the trigger out
//! (`the_trigger_is_compiled_only_into_debug_builds`).
//!
//! # `RUST_BACKTRACE`
//!
//! CI sets `RUST_BACKTRACE=1` for every job, and a panic's output depends on it, so every
//! run here sets it or removes it (`every_run_sets_or_removes_rust_backtrace`).
//!
//! # With the `debug-panics` feature
//!
//! The never-shipped `debug-panics` feature prints the panic's message and location after
//! the two lines. The tests that pin the default output (`default_output`) are compiled
//! out under it, and `with_debug_panics` runs instead. It is the positive control for the
//! payload: the text the default tests look for is really in the panic they trigger.

mod common;
use common::{closed_pipe, mds_bin};

use std::io::Read;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What `mds` prints when it panics, built as the CLI builds it: the issue tracker is the
/// manifest's repository followed by `/issues`.
const ICE_TEXT: &str = concat!(
    "mds: internal compiler error\n",
    "note: this is a bug in mds; please report it at ",
    env!("CARGO_PKG_REPOSITORY"),
    "/issues\n",
);

/// The word the trigger's payload carries (`panic_trigger` in `src/output.rs`).
const SENTINEL: &str = "mds-test-panic-payload";

/// The variable that asks a debug build of `mds` to panic.
const TRIGGER: &str = "MDS_TEST_PANIC";

/// The variable a panic's backtrace depends on.
const RUST_BACKTRACE: &str = "RUST_BACKTRACE";

/// Failure bound for one run. A run takes milliseconds; this only stops a hung child from
/// hanging the suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a panic may take to end the run — on any thread, with stderr open or closed.
const PANIC_EXIT_BOUND: Duration = Duration::from_secs(10);

/// How often [`wait_bounded`] polls the child for exit.
const POLL: Duration = Duration::from_millis(5);

/// How a run sets `RUST_BACKTRACE`.
#[derive(Clone, Copy, Debug)]
enum Backtrace {
    Unset,
    Set(&'static str),
}

/// Where a run's stderr goes.
#[derive(Clone, Copy, Debug)]
enum Stderr {
    Piped,
    /// A pipe whose reader is gone before the run starts.
    Closed,
}

/// What one run left behind.
struct Run {
    code: Option<i32>,
    stdout: String,
    /// Empty when stderr was closed.
    stderr: String,
    took: Duration,
}

/// A fresh working directory holding `ok.mds`, which `mds check` passes.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ok.mds"), "Hello\n").expect("write ok.mds");
    dir
}

/// Run `mds check ok.mds` in `dir`, asking for a panic at `panic_at` (none for `None`),
/// with `RUST_BACKTRACE` as `backtrace` says and stderr as `stderr` says.
fn run_mds(dir: &Path, panic_at: Option<&str>, backtrace: Backtrace, stderr: Stderr) -> Run {
    run_args(dir, &["check", "ok.mds"], panic_at, backtrace, stderr)
}

/// Run `mds <args>` in `dir`, asking for a panic at `panic_at` (none for `None`), with
/// `RUST_BACKTRACE` as `backtrace` says and stderr as `stderr` says.
fn run_args(
    dir: &Path,
    args: &[&str],
    panic_at: Option<&str>,
    backtrace: Backtrace,
    stderr: Stderr,
) -> Run {
    let mut cmd = mds_bin();
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    match panic_at {
        Some(at) => cmd.env(TRIGGER, at),
        None => cmd.env_remove(TRIGGER),
    };
    match backtrace {
        Backtrace::Unset => cmd.env_remove(RUST_BACKTRACE),
        Backtrace::Set(value) => cmd.env(RUST_BACKTRACE, value),
    };
    match stderr {
        Stderr::Piped => cmd.stderr(Stdio::piped()),
        Stderr::Closed => cmd.stderr(closed_pipe()),
    };
    let started = Instant::now();
    let mut child = cmd.spawn().expect("spawn mds");
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let code = wait_bounded(&mut child);
    Run {
        code,
        took: started.elapsed(),
        stdout: joined(out),
        stderr: joined(err),
    }
}

/// Drain one child pipe on its own thread so neither pipe can fill and stall the child.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<JoinHandle<Vec<u8>>> {
    pipe.map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    })
}

fn joined(handle: Option<JoinHandle<Vec<u8>>>) -> String {
    let bytes = handle
        .map(|h| h.join().expect("pipe drain thread"))
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Wait for `child` to exit, killing it if [`RUN_TIMEOUT`] passes first.
fn wait_bounded(child: &mut Child) -> Option<i32> {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("poll the mds child") {
            return status.code();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("mds did not exit within {RUN_TIMEOUT:?}");
        }
        std::thread::sleep(POLL);
    }
}

// ── Behaviour ─────────────────────────────────────────────────────────────────

/// A panic with stderr closed still exits 101 — not by a signal, not by an abort: the
/// hook's write fails and the hook carries on. Main thread and another thread, with and
/// without a backtrace.
///
/// Control: with stderr open the same run reaches the hook and prints the text.
#[test]
fn a_panic_with_stderr_closed_exits_101_not_by_a_signal() {
    let dir = fixture();
    for panic_at in ["main", "thread"] {
        let open = run_mds(dir.path(), Some(panic_at), Backtrace::Unset, Stderr::Piped);
        assert_eq!(
            open.code,
            Some(101),
            "control ({panic_at}): with stderr open a panic exits 101; stderr:\n{}",
            open.stderr
        );
        assert!(
            open.stderr.starts_with(ICE_TEXT),
            "control ({panic_at}): with stderr open the hook prints the text; it was:\n{}",
            open.stderr
        );
        for backtrace in [Backtrace::Unset, Backtrace::Set("1")] {
            let closed = run_mds(dir.path(), Some(panic_at), backtrace, Stderr::Closed);
            assert_eq!(
                closed.code,
                Some(101),
                "a panic ({panic_at}, {backtrace:?}) with stderr closed exits 101, not by a \
                 signal (None)"
            );
            assert_eq!(closed.stdout, "", "a panic writes nothing to stdout");
            assert!(
                closed.took < PANIC_EXIT_BOUND,
                "the run ends within {PANIC_EXIT_BOUND:?}; it took {:?}",
                closed.took
            );
        }
    }
}

/// The default output: exactly the text, and with `RUST_BACKTRACE` a backtrace. Compiled
/// out under `debug-panics`, which prints the panic's message and location as well.
#[cfg(not(feature = "debug-panics"))]
mod default_output {
    use super::*;
    use crate::common::count_occurrences;
    #[cfg(unix)]
    use crate::common::{
        spawn_watch_ready, spawn_watch_unsynchronized, wait_for_tap_count, write_atomic,
        ChildGuard, PipeTap,
    };

    /// The first line of the backtrace `RUST_BACKTRACE` adds.
    const BACKTRACE_HEADER: &str = "stack backtrace:\n";

    /// What a run without a backtrace must never show on top of the payload: the default
    /// panic message's source location (`src/output.rs:12:5`).
    const LOCATION_NEEDLES: &[&str] = &["src/", "src\\", ".rs:"];

    /// What a panic's output must never show of the trigger's payload: its sentinel, its
    /// raw ESC, and the working directory it names — as the test made it and as the OS
    /// resolves it — and the default panic message's shape: `panicked at` and a thread
    /// name.
    fn payload_needles(dir: &Path) -> Vec<String> {
        let canonical = dir
            .canonicalize()
            .expect("canonicalize the working directory");
        // Non-vacuity: the two paths name the directory the run worked in.
        assert!(dir.is_absolute() && dir.is_dir() && canonical.is_dir());
        vec![
            SENTINEL.to_string(),
            char::from(0x1b).to_string(),
            dir.display().to_string(),
            canonical.display().to_string(),
            "panicked at".to_string(),
            "thread '".to_string(),
        ]
    }

    fn location_needles() -> Vec<String> {
        LOCATION_NEEDLES.iter().map(|s| (*s).to_string()).collect()
    }

    /// Assert that `text` shows none of `needles`.
    fn assert_shows_none(text: &str, needles: &[String], what: &str) {
        for needle in needles {
            assert!(!needle.is_empty(), "non-vacuity: a needle is never empty");
            assert!(
                !text.contains(needle.as_str()),
                "{what}: stderr must not show {needle:?}; it was:\n{text}"
            );
        }
    }

    /// Whether `line` holds no character the CLI escapes: WIRE escaping leaves it as it is.
    fn is_plain(line: &str) -> bool {
        mds::sanitize_control_chars_wire(line) == line
    }

    /// The frame a real backtrace of a panic names: the CLI's panic hook.
    const HOOK_FRAME: &str = "mds::output::on_panic";

    /// The fewest numbered frames a real backtrace shows.
    const MIN_FRAMES: usize = 3;

    /// `line` without the crate hashes of v0 symbol names — a `[`, one or more hex digits
    /// and a `]`, right after a crate's name — so that
    /// `mds[1fc92abd4b2f6a9a]::output::on_panic` reads `mds::output::on_panic`, as the
    /// legacy name `mds::output::on_panic::h…` does. rustc emits either scheme, and
    /// `RUST_BACKTRACE=full` prints a v0 name's hashes.
    fn without_crate_hashes(line: &str) -> String {
        let mut parts = line.split('[');
        let mut clean = parts.next().unwrap_or_default().to_string();
        for part in parts {
            let hex = part.len()
                - part
                    .trim_start_matches(|c: char| c.is_ascii_hexdigit())
                    .len();
            let after_a_name = clean.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
            match part[hex..].strip_prefix(']') {
                Some(rest) if hex > 0 && after_a_name => clean.push_str(rest),
                _ => {
                    clean.push('[');
                    clean.push_str(part);
                }
            }
        }
        clean
    }

    /// Whether a line of `backtrace` names [`HOOK_FRAME`], in either symbol-name scheme.
    fn names_the_hook(backtrace: &str) -> bool {
        backtrace
            .lines()
            .any(|line| without_crate_hashes(line).contains(HOOK_FRAME))
    }

    /// How many lines of `backtrace` start a frame: its number, right-aligned, then `: `
    /// (`   4: mds::output::on_panic`). A frame's `at <file>:<line>` line and a function
    /// inlined into it carry no number.
    fn numbered_frames(backtrace: &str) -> usize {
        backtrace
            .lines()
            .filter(|line| {
                let numbered = line.trim_start_matches(' ');
                let digits = numbered.len()
                    - numbered
                        .trim_start_matches(|c: char| c.is_ascii_digit())
                        .len();
                digits > 0 && numbered[digits..].starts_with(": ")
            })
            .count()
    }

    /// The backtrace check reads both of rustc's symbol-name schemes. A legacy name ends in
    /// a hash (`mds::output::on_panic::h0123456789abcdef`); a v0 name printed in full keeps
    /// its crate's hash (`mds[1fc92abd4b2f6a9a]::output::on_panic`, as CI's rustc prints
    /// it with `RUST_BACKTRACE=full`), so as it stands it never reads `mds::`.
    ///
    /// Controls: the v0 line fails a plain `mds::` check; a bracket that is not a crate
    /// hash is kept; another frame, of std or of the CLI, does not name the hook; a frame's
    /// `at` line and a function inlined into a frame are not frames, so two frames with
    /// their `at` lines fall short of [`MIN_FRAMES`] though they are more than two lines.
    #[test]
    fn the_backtrace_check_reads_both_symbol_name_schemes() {
        const V0: &str = "   5:     0x55f9020bb0b0 - mds[1fc92abd4b2f6a9a]::output::on_panic";
        const LEGACY: &str = "   5:     0x55f9020bb0b0 - mds::output::on_panic::h0123456789abcdef";
        assert!(
            !V0.contains("mds::"),
            "control: the v0 line fails a plain `mds::` check"
        );
        for line in [V0, LEGACY] {
            assert!(names_the_hook(line), "{line:?} names the hook's frame");
        }
        assert_eq!(
            without_crate_hashes(V0),
            "   5:     0x55f9020bb0b0 - mds::output::on_panic"
        );
        assert_eq!(
            without_crate_hashes(LEGACY),
            LEGACY,
            "a legacy name has no crate hash"
        );
        assert_eq!(
            without_crate_hashes("<std[6c98fd8553dbae28]::backtrace::Backtrace>::create"),
            "<std::backtrace::Backtrace>::create",
            "every crate hash on a line goes"
        );
        for kept in ["<[f32]>::len", "x[]", "x[1f", "x[1fz]", "x[0x1f]", "[1f]"] {
            assert_eq!(without_crate_hashes(kept), kept, "not a crate hash");
        }
        for other in [
            "   8:     0x55f902661992 - std[6c98fd8553dbae28]::panicking::panic",
            "  14:     0x55f90206a7f4 - mds[1fc92abd4b2f6a9a]::run",
        ] {
            assert!(
                !names_the_hook(other),
                "control: {other:?} is not the hook's frame"
            );
        }

        let frames = [
            "stack backtrace:",
            "   4: mds::output::write_backtrace",
            "             at ./crates/mds-cli/src/output.rs:604:21",
            "      mds::output::inlined_into_frame_4",
            "  12:     0x55f90206a7f4 - mds[1fc92abd4b2f6a9a]::run",
            "                               at /w/crates/mds-cli/src/main.rs:500:5",
            "1000: main",
        ];
        assert_eq!(
            numbered_frames(&frames.join("\n")),
            3,
            "only a numbered line starts a frame"
        );
        let two_frames = frames[..6].join("\n");
        assert!(
            two_frames.lines().count() > 2 && numbered_frames(&two_frames) < MIN_FRAMES,
            "control: two frames with their `at` lines fall short of {MIN_FRAMES} frames"
        );
    }

    /// A panic prints exactly the internal compiler error text and exits 101: never the
    /// payload's sentinel, ESC or absolute path, never `panicked at`, a source location or
    /// a thread name.
    ///
    /// Control: without the trigger the same command passes, so the panic is the
    /// trigger's; the exact text is the positive half of the absence checks.
    #[test]
    fn a_panic_prints_only_the_internal_compiler_error_text_and_exits_101() {
        let dir = fixture();
        let control = run_mds(dir.path(), None, Backtrace::Unset, Stderr::Piped);
        assert_eq!(
            (control.code, control.stderr.as_str()),
            (Some(0), "OK: ok.mds\n"),
            "control: without the trigger `mds check ok.mds` passes"
        );

        let run = run_mds(dir.path(), Some("main"), Backtrace::Unset, Stderr::Piped);
        assert_eq!(
            run.code,
            Some(101),
            "a panic exits 101; stderr:\n{}",
            run.stderr
        );
        assert_eq!(
            run.stderr, ICE_TEXT,
            "stderr is exactly the internal compiler error text"
        );
        assert_eq!(run.stdout, "", "a panic writes nothing to stdout");
        assert_shows_none(&run.stderr, &payload_needles(dir.path()), "a panic");
        assert_shows_none(&run.stderr, &location_needles(), "a panic");
    }

    /// `RUST_BACKTRACE` set to `1` or `full` adds a backtrace after the text, and only
    /// that: every line of it escaped, and still nothing of the payload. `0` adds nothing.
    ///
    /// Controls: the line check finds a raw ESC; the backtrace is a real one, naming the
    /// panic hook's frame in either symbol-name scheme among at least [`MIN_FRAMES`]
    /// numbered frames (`the_backtrace_check_reads_both_symbol_name_schemes`).
    #[test]
    fn rust_backtrace_adds_only_an_escaped_backtrace() {
        let dir = fixture();
        assert!(
            !is_plain(&format!("frame {}[2J", char::from(0x1b))),
            "control: the line check finds a raw ESC"
        );

        let off = run_mds(dir.path(), Some("main"), Backtrace::Set("0"), Stderr::Piped);
        assert_eq!(
            (off.code, off.stderr.as_str()),
            (Some(101), ICE_TEXT),
            "RUST_BACKTRACE=0 asks for no backtrace"
        );

        for value in ["1", "full"] {
            let run = run_mds(
                dir.path(),
                Some("main"),
                Backtrace::Set(value),
                Stderr::Piped,
            );
            assert_eq!(
                run.code,
                Some(101),
                "RUST_BACKTRACE={value}: a panic exits 101; stderr:\n{}",
                run.stderr
            );
            let backtrace = run.stderr.strip_prefix(ICE_TEXT).unwrap_or_else(|| {
                panic!(
                    "RUST_BACKTRACE={value}: stderr starts with the internal compiler error \
                     text; it was:\n{}",
                    run.stderr
                )
            });
            assert!(
                backtrace.starts_with(BACKTRACE_HEADER),
                "RUST_BACKTRACE={value}: the backtrace follows the text; stderr:\n{}",
                run.stderr
            );
            assert!(
                names_the_hook(backtrace),
                "RUST_BACKTRACE={value}: a real backtrace names the panic hook's frame, \
                 {HOOK_FRAME}; it was:\n{backtrace}"
            );
            assert!(
                numbered_frames(backtrace) >= MIN_FRAMES,
                "RUST_BACKTRACE={value}: a real backtrace shows at least {MIN_FRAMES} \
                 numbered frames; it was:\n{backtrace}"
            );
            for line in backtrace.split_terminator('\n') {
                assert!(
                    is_plain(line),
                    "RUST_BACKTRACE={value}: every backtrace line is escaped; got {line:?}"
                );
            }
            assert_shows_none(
                &run.stderr,
                &payload_needles(dir.path()),
                &format!("RUST_BACKTRACE={value}"),
            );
        }
    }

    /// A panic on a thread other than the one running the command ends the run: the text
    /// once, exit 101, within [`PANIC_EXIT_BOUND`]. Without the hook the thread would die
    /// alone, and the command would go on and pass.
    ///
    /// Control: without the trigger the same command passes.
    #[test]
    fn a_panic_on_another_thread_ends_the_run_with_the_text_once_and_exit_101() {
        let dir = fixture();
        let control = run_mds(dir.path(), None, Backtrace::Unset, Stderr::Piped);
        assert_eq!(
            (control.code, control.stderr.as_str()),
            (Some(0), "OK: ok.mds\n"),
            "control: without the trigger `mds check ok.mds` passes"
        );

        let run = run_mds(dir.path(), Some("thread"), Backtrace::Unset, Stderr::Piped);
        assert_eq!(
            run.code,
            Some(101),
            "a panic on another thread ends the run with 101; stderr:\n{}",
            run.stderr
        );
        assert_eq!(run.stderr, ICE_TEXT, "stderr is exactly the text");
        assert_eq!(
            count_occurrences(&run.stderr, "internal compiler error"),
            1,
            "the text is printed once"
        );
        assert!(
            run.took < PANIC_EXIT_BOUND,
            "the run ends within {PANIC_EXIT_BOUND:?} of the panic; it took {:?}",
            run.took
        );
        assert_eq!(run.stdout, "");
        assert_shows_none(&run.stderr, &payload_needles(dir.path()), "a thread panic");
        assert_shows_none(&run.stderr, &location_needles(), "a thread panic");
    }

    // ── A batch goes on past a file whose compile panics ──────────────────────
    //
    // Directory runs of `mds build`, `check`, `fmt` and `lint` over `d/a.mds`, `d/b.mds`
    // and `d/c.mds`, with the trigger panicking in `b.mds`'s compile, format or analysis.

    /// A working directory holding `d/a.mds`, `d/b.mds` and `d/c.mds`, each `source(name)`.
    fn batch(source: impl Fn(&str) -> String) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path().join("d");
        std::fs::create_dir(&d).expect("create d");
        for name in ["a", "b", "c"] {
            std::fs::write(d.join(format!("{name}.mds")), source(name)).expect("write a source");
        }
        dir
    }

    /// `mds <args>` in `dir`, panicking in `b.mds` when `panic` is set.
    fn run_batch(dir: &Path, args: &[&str], panic: bool) -> Run {
        let panic_at = panic.then_some("compile:b");
        run_args(dir, args, panic_at, Backtrace::Unset, Stderr::Piped)
    }

    /// The text of `path`.
    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// What a batch run with one panic shows of it: exit 101, the text exactly once, and
    /// nothing of the payload on either stream.
    fn assert_one_panic(run: &Run, dir: &Path, what: &str) {
        assert_eq!(
            run.code,
            Some(101),
            "{what}: a panic in one file makes the run exit 101; stderr:\n{}",
            run.stderr
        );
        assert_eq!(
            count_occurrences(&run.stderr, ICE_TEXT),
            1,
            "{what}: the text is printed once, for the one panic; stderr:\n{}",
            run.stderr
        );
        assert_shows_none(&run.stderr, &payload_needles(dir), what);
        assert_shows_none(&run.stdout, &payload_needles(dir), what);
    }

    /// The lines of `stderr` that are neither a line of the text nor a line starting with
    /// one of `status`: what is left to report a panic a second time.
    fn other_lines<'a>(stderr: &'a str, status: &[&str]) -> Vec<&'a str> {
        let ice: Vec<&str> = ICE_TEXT.lines().collect();
        stderr
            .lines()
            .filter(|line| !ice.contains(line) && !status.iter().any(|s| line.starts_with(s)))
            .collect()
    }

    /// A source with one warning: a frontmatter key its body never uses.
    fn warned(name: &str) -> String {
        format!("---\nunused: 1\n---\nHello {name}\n")
    }

    /// `mds build <dir>` goes on past a file whose compile panics (#389): the other two are
    /// compiled and written, the one that panicked is not and counts as failed, and the text
    /// is the panic's one report.
    ///
    /// Control: without the trigger all three are written.
    #[test]
    fn a_directory_build_goes_on_past_a_file_whose_compile_panics() {
        let control_dir = batch(|name| format!("Hello {name}\n"));
        let control = run_batch(control_dir.path(), &["build", "d"], false);
        assert_eq!(
            control.code,
            Some(0),
            "control: stderr:\n{}",
            control.stderr
        );
        assert!(
            control.stderr.ends_with("3 built, 0 failed\n"),
            "control: stderr:\n{}",
            control.stderr
        );
        for name in ["a", "b", "c"] {
            let output = control_dir.path().join("d").join(format!("{name}.md"));
            assert_eq!(read(&output), format!("Hello {name}\n"), "control");
        }

        let dir = batch(|name| format!("Hello {name}\n"));
        let run = run_batch(dir.path(), &["build", "d"], true);
        assert_one_panic(&run, dir.path(), "build");
        let d = dir.path().join("d");
        for name in ["a", "c"] {
            assert_eq!(
                read(&d.join(format!("{name}.md"))),
                format!("Hello {name}\n"),
                "the other files are compiled and written"
            );
        }
        assert!(
            !d.join("b.md").exists(),
            "the file whose compile panicked is not written"
        );
        let compiled = run
            .stderr
            .lines()
            .filter(|line| line.starts_with("Compiled to "))
            .count();
        assert_eq!(compiled, 2, "stderr:\n{}", run.stderr);
        assert_eq!(
            other_lines(&run.stderr, &["Compiled to "]),
            ["2 built, 1 failed"],
            "the file counts as failed, and nothing but the text reports the panic"
        );
    }

    /// `mds check <dir>` goes on past a file whose check panics (#389): the other two are
    /// checked, the one that panicked counts as failed, and the text is the panic's one
    /// report.
    ///
    /// Control: without the trigger all three pass.
    #[test]
    fn a_directory_check_goes_on_past_a_file_whose_check_panics() {
        let dir = batch(|name| format!("Hello {name}\n"));
        let control = run_batch(dir.path(), &["check", "d"], false);
        assert_eq!(
            (control.code, control.stderr.as_str()),
            (Some(0), "3 passed, 0 failed\n"),
            "control"
        );

        let run = run_batch(dir.path(), &["check", "d"], true);
        assert_one_panic(&run, dir.path(), "check");
        assert_eq!(
            run.stderr,
            format!("{ICE_TEXT}2 passed, 1 failed\n"),
            "the other files are checked, the file counts as failed, and nothing but the \
             text reports the panic"
        );
    }

    /// `mds fmt <dir>` goes on past a file whose format panics (#389): the other two are
    /// rewritten, the one that panicked is left as it was and counts as failed, and the
    /// text is the panic's one report.
    ///
    /// Control: without the trigger all three are rewritten.
    #[test]
    fn a_directory_fmt_goes_on_past_a_file_whose_format_panics() {
        let unformatted = |name: &str| format!("Hello {name}\r\n");
        let control_dir = batch(unformatted);
        let control = run_batch(control_dir.path(), &["fmt", "d"], false);
        assert_eq!(
            control.code,
            Some(0),
            "control: stderr:\n{}",
            control.stderr
        );
        assert!(
            control
                .stderr
                .ends_with("3 formatted, 0 unchanged, 0 failed\n"),
            "control: stderr:\n{}",
            control.stderr
        );
        for name in ["a", "b", "c"] {
            let source = control_dir.path().join("d").join(format!("{name}.mds"));
            assert_eq!(read(&source), format!("Hello {name}\n"), "control");
        }

        let dir = batch(unformatted);
        let run = run_batch(dir.path(), &["fmt", "d"], true);
        assert_one_panic(&run, dir.path(), "fmt");
        let d = dir.path().join("d");
        for name in ["a", "c"] {
            assert_eq!(
                read(&d.join(format!("{name}.mds"))),
                format!("Hello {name}\n"),
                "the other files are formatted"
            );
        }
        assert_eq!(
            read(&d.join("b.mds")),
            unformatted("b"),
            "the file whose format panicked is left as it was"
        );
        assert_eq!(
            other_lines(&run.stderr, &["Formatted: "]),
            ["2 formatted, 0 unchanged, 1 failed"],
            "the file counts as failed, and nothing but the text reports the panic"
        );
    }

    /// `mds lint <dir>` goes on past a file whose analysis panics (#389): the other two are
    /// linted and show their findings, the one that panicked counts under "with errors",
    /// and nothing names it — the text is the panic's one report.
    ///
    /// Control: without the trigger each file's warning is shown, naming its file.
    #[test]
    fn a_directory_lint_goes_on_past_a_file_whose_analysis_panics() {
        let dir = batch(warned);
        let control = run_batch(dir.path(), &["lint", "d"], false);
        assert_eq!(
            control.code,
            Some(1),
            "control: stderr:\n{}",
            control.stderr
        );
        assert!(
            control
                .stderr
                .ends_with("0 clean, 3 with warnings, 0 with errors, 0 resource-limited\n"),
            "control: stderr:\n{}",
            control.stderr
        );
        for name in ["a.mds", "b.mds", "c.mds"] {
            assert!(
                control.stderr.contains(name),
                "control: a finding's frame names {name}; stderr:\n{}",
                control.stderr
            );
        }

        let run = run_batch(dir.path(), &["lint", "d"], true);
        assert_one_panic(&run, dir.path(), "lint");
        assert!(
            run.stderr
                .ends_with("0 clean, 2 with warnings, 1 with errors, 0 resource-limited\n"),
            "the file counts under \"with errors\"; stderr:\n{}",
            run.stderr
        );
        for name in ["a.mds", "c.mds"] {
            assert!(
                run.stderr.contains(name),
                "the other files are linted; stderr:\n{}",
                run.stderr
            );
        }
        for needle in ["b.mds", "mds::internal"] {
            assert!(
                !run.stderr.contains(needle),
                "nothing but the text reports the panic: {needle:?}; stderr:\n{}",
                run.stderr
            );
        }
    }

    /// `mds lint --format json <dir>` records a file whose analysis panicked as an internal
    /// error, in the one document the run prints, and counts it under "with errors" (#389):
    /// `{"file": …, "error": {"code": "mds::internal", "message": "internal compiler
    /// error", …}}` — nothing of the panic itself. The other two files are linted.
    ///
    /// Control: without the trigger each file's entry holds its warning.
    #[test]
    fn a_directory_lint_json_records_a_file_whose_analysis_panics_as_an_internal_error() {
        let dir = batch(warned);
        let args = ["lint", "--format", "json", "d"];
        let control = run_batch(dir.path(), &args, false);
        assert_eq!(
            (control.code, control.stderr.as_str()),
            (
                Some(1),
                "0 clean, 3 with warnings, 0 with errors, 0 resource-limited\n"
            ),
            "control"
        );
        let document: serde_json::Value =
            serde_json::from_str(&control.stdout).expect("control: stdout is one document");
        let files = document["files"].as_array().expect("control: files[]");
        assert_eq!(files.len(), 3, "control: {document}");
        for (entry, name) in files.iter().zip(["a.mds", "b.mds", "c.mds"]) {
            assert_eq!(entry["file"], name, "control: {document}");
            assert_eq!(
                entry["diagnostics"][0]["rule"], "unused-variable",
                "control: {document}"
            );
        }

        let run = run_batch(dir.path(), &args, true);
        assert_one_panic(&run, dir.path(), "lint --format json");
        assert_eq!(
            run.stderr,
            format!("{ICE_TEXT}0 clean, 2 with warnings, 1 with errors, 0 resource-limited\n"),
            "the file counts under \"with errors\", and stderr reports the panic only with \
             the text"
        );
        assert_eq!(
            run.stdout.lines().count(),
            1,
            "one document; stdout:\n{}",
            run.stdout
        );
        let document: serde_json::Value =
            serde_json::from_str(&run.stdout).expect("stdout is one document");
        assert_eq!(document["version"], 1, "{document}");
        assert_eq!(document["truncated"], false, "{document}");
        let files = document["files"].as_array().expect("files[]");
        assert_eq!(files.len(), 3, "an entry per file: {document}");
        assert_eq!(
            files[1],
            serde_json::json!({
                "file": "b.mds",
                "error": {
                    "code": "mds::internal",
                    "message": "internal compiler error",
                    "help": null,
                    "span": null,
                },
            }),
            "the file whose analysis panicked is an internal error: {document}"
        );
        for (entry, name) in [(&files[0], "a.mds"), (&files[2], "c.mds")] {
            assert_eq!(entry["file"], name, "{document}");
            assert_eq!(
                entry["diagnostics"][0]["rule"], "unused-variable",
                "the other files are linted: {document}"
            );
        }
    }

    /// `mds lint --fix`'s fix pipeline lints its candidate again, inside a catch (#389): a
    /// panic there leaves the file as it was and the run exits 101. Under `--format json`
    /// the run's one document is the error document with the internal error; in human
    /// output the text is the panic's one report. A file argument reaches the pipeline's
    /// catch: its own lint is not caught, and the trigger fires only in a catch.
    ///
    /// Control: without the trigger the fix is written.
    #[test]
    fn a_panic_in_the_fix_pipeline_leaves_the_file_as_it_was_and_reports_only_the_text() {
        const FIXABLE: &str = "@if \"a\" == \"a\":\n@end\n\nHello\n";
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("x.mds");
        std::fs::write(&source, FIXABLE).expect("write x.mds");
        let control = run_args(
            dir.path(),
            &["lint", "--fix", "x.mds"],
            None,
            Backtrace::Unset,
            Stderr::Piped,
        );
        assert_eq!(
            (control.code, control.stderr.as_str()),
            (Some(0), "Fixed: x.mds\n"),
            "control"
        );
        assert_eq!(read(&source), "\nHello\n", "control: the fix is written");

        for format in ["json", "human"] {
            std::fs::write(&source, FIXABLE).expect("write x.mds");
            let run = run_args(
                dir.path(),
                &["lint", "--fix", "--format", format, "x.mds"],
                Some("compile:x"),
                Backtrace::Unset,
                Stderr::Piped,
            );
            assert_one_panic(&run, dir.path(), format);
            assert_eq!(
                run.stderr, ICE_TEXT,
                "{format}: the text is the panic's one report"
            );
            assert_eq!(
                read(&source),
                FIXABLE,
                "{format}: the file is left as it was"
            );
            let expected = match format {
                "json" => concat!(
                    "{\"error\":{\"code\":\"mds::internal\",\"help\":null,",
                    "\"message\":\"internal compiler error\",\"span\":null},\"version\":1}\n"
                ),
                _ => "",
            };
            assert_eq!(run.stdout, expected, "{format}: stdout");
        }
    }

    // ── A watch session goes on past a compile that panics ────────────────────

    /// Failure bound for one step of a watch session: going live, or what an edit leads
    /// to. A step takes milliseconds.
    #[cfg(unix)]
    const WATCH_STEP: Duration = Duration::from_secs(20);

    /// `mds watch --quiet --poll-interval 0 <args>` in `dir`, asking for a panic at
    /// `panic_at`, once it is live. `--quiet` leaves the panic's text the only thing on
    /// stderr, and without an idle tick only an edit starts a rebuild.
    #[cfg(unix)]
    fn watch_live(dir: &Path, args: &[&str], panic_at: Option<&str>) -> (ChildGuard, PipeTap) {
        let mut cmd = watch_command(dir, args, panic_at);
        cmd.env_remove(RUST_BACKTRACE);
        let (child, stderr, _) = spawn_watch_ready(&mut cmd);
        (ChildGuard(child), stderr)
    }

    /// [`watch_live`]'s session, not waited for: `ready` is the file it creates once it is
    /// live.
    #[cfg(unix)]
    fn watch_starting(
        dir: &Path,
        args: &[&str],
        panic_at: Option<&str>,
        ready: &Path,
    ) -> (ChildGuard, PipeTap) {
        let mut cmd = watch_command(dir, args, panic_at);
        cmd.env_remove(RUST_BACKTRACE).env("MDS_TEST_READY", ready);
        let (child, stderr, _) = spawn_watch_unsynchronized(&mut cmd);
        (ChildGuard(child), stderr)
    }

    /// The `mds watch` command [`watch_live`] and [`watch_starting`] spawn.
    #[cfg(unix)]
    fn watch_command(dir: &Path, args: &[&str], panic_at: Option<&str>) -> std::process::Command {
        let mut cmd = mds_bin();
        cmd.args(["watch", "--quiet", "--poll-interval", "0"])
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        match panic_at {
            Some(at) => cmd.env(TRIGGER, at),
            None => cmd.env_remove(TRIGGER),
        };
        cmd
    }

    /// Ctrl-C the session, and return its exit code. Panics when it has not ended within
    /// [`WATCH_STEP`].
    #[cfg(unix)]
    fn interrupt(session: &mut ChildGuard) -> Option<i32> {
        let pid = libc::pid_t::try_from(session.id()).expect("a pid fits pid_t");
        // SAFETY: `kill` only sends a signal, to the child this test spawned and has not
        // reaped, so the pid names no other process.
        let sent = unsafe { libc::kill(pid, libc::SIGINT) };
        assert_eq!(sent, 0, "send SIGINT to the session");
        exit_code_within(session, WATCH_STEP)
    }

    /// The session's exit code once it ends, within `bound`; panics when it has not.
    #[cfg(unix)]
    fn exit_code_within(session: &mut ChildGuard, bound: Duration) -> Option<i32> {
        let deadline = Instant::now() + bound;
        loop {
            if let Some(status) = session.0.try_wait().expect("poll the session") {
                return status.code();
            }
            assert!(
                Instant::now() < deadline,
                "the session did not end within {bound:?}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// Whether the session is still running.
    #[cfg(unix)]
    fn running(session: &mut ChildGuard) -> bool {
        session.0.try_wait().expect("poll the session").is_none()
    }

    /// Wait until `path` holds `text`, within [`WATCH_STEP`].
    #[cfg(unix)]
    fn wait_for_file(path: &Path, text: &str) -> bool {
        let deadline = Instant::now() + WATCH_STEP;
        loop {
            if std::fs::read_to_string(path).is_ok_and(|now| now == text) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// A quiet session's whole stderr after `panics` or more panics: the text, once per
    /// panic, and nothing else.
    #[cfg(unix)]
    fn assert_only_the_text(stderr: &str, panics: usize, what: &str) {
        let printed = count_occurrences(stderr, ICE_TEXT);
        assert!(
            printed >= panics,
            "{what}: the text for each of at least {panics} panics; stderr:\n{stderr}"
        );
        assert_eq!(
            stderr,
            ICE_TEXT.repeat(printed),
            "{what}: the text once per panic, and nothing else"
        );
    }

    /// `mds watch <file>` whose compile panics goes on watching (#389): the startup compile
    /// prints the text, an edit starts a rebuild that prints it again, the session stays
    /// live, and it exits 101 when stopped. No output is written, and nothing but the text
    /// reports a panic.
    ///
    /// Control: without the trigger the session writes the output, rebuilds it on the edit
    /// and exits 0 when stopped.
    #[cfg(unix)]
    #[test]
    fn a_watched_file_whose_compile_panics_is_watched_on_and_the_session_exits_101() {
        let control_dir = tempfile::tempdir().expect("tempdir");
        let source = control_dir.path().join("x.mds");
        std::fs::write(&source, "Hello\n").expect("write x.mds");
        let (mut control, stderr) = watch_live(control_dir.path(), &["x.mds"], None);
        let output = control_dir.path().join("x.md");
        assert!(wait_for_file(&output, "Hello\n"), "control: the output");
        write_atomic(&source, "Hello again\n");
        assert!(
            wait_for_file(&output, "Hello again\n"),
            "control: the edit rebuilds"
        );
        assert_eq!(interrupt(&mut control), Some(0), "control: exit 0");
        assert_eq!(stderr.finish_text(&mut control), "", "control: --quiet");

        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("x.mds");
        std::fs::write(&source, "Hello\n").expect("write x.mds");
        let (mut session, stderr) = watch_live(dir.path(), &["x.mds"], Some("compile:x"));
        let before = count_occurrences(
            &wait_for_tap_count(&stderr, ICE_TEXT, 1, WATCH_STEP),
            ICE_TEXT,
        );
        write_atomic(&source, "Hello again\n");
        wait_for_tap_count(&stderr, ICE_TEXT, before + 1, WATCH_STEP);
        assert!(
            running(&mut session),
            "a compile that panics does not end the session"
        );
        assert_eq!(
            interrupt(&mut session),
            Some(101),
            "stopped, a session that caught a panic exits 101"
        );
        assert_only_the_text(&stderr.finish_text(&mut session), before + 1, "file");
        assert!(
            !dir.path().join("x.md").exists(),
            "a compile that panicked writes no output"
        );
    }

    /// `mds watch <dir>` goes on past a source whose compile panics (#389): the startup
    /// writes the other source's output, an edit to it rebuilds it, the session stays live,
    /// and it exits 101 when stopped. Nothing but the text reports a panic.
    ///
    /// Control: without the trigger both outputs are written, the edit rebuilds, and the
    /// session exits 0 when stopped.
    #[cfg(unix)]
    #[test]
    fn a_watched_directory_goes_on_past_a_source_whose_compile_panics_and_exits_101() {
        let sources = || {
            let dir = tempfile::tempdir().expect("tempdir");
            let d = dir.path().join("d");
            std::fs::create_dir(&d).expect("create d");
            for name in ["a", "b"] {
                std::fs::write(d.join(format!("{name}.mds")), format!("Hello {name}\n"))
                    .expect("write a source");
            }
            dir
        };

        let control_dir = sources();
        let d = control_dir.path().join("d");
        let (mut control, stderr) = watch_live(control_dir.path(), &["d"], None);
        for name in ["a", "b"] {
            assert!(
                wait_for_file(&d.join(format!("{name}.md")), &format!("Hello {name}\n")),
                "control: {name}.md"
            );
        }
        write_atomic(&d.join("b.mds"), "Hello again b\n");
        assert!(
            wait_for_file(&d.join("b.md"), "Hello again b\n"),
            "control: the edit rebuilds"
        );
        assert_eq!(interrupt(&mut control), Some(0), "control: exit 0");
        assert_eq!(stderr.finish_text(&mut control), "", "control: --quiet");

        let dir = sources();
        let d = dir.path().join("d");
        let (mut session, stderr) = watch_live(dir.path(), &["d"], Some("compile:a"));
        wait_for_tap_count(&stderr, ICE_TEXT, 1, WATCH_STEP);
        assert!(
            wait_for_file(&d.join("b.md"), "Hello b\n"),
            "the startup compiles the other source"
        );
        write_atomic(&d.join("b.mds"), "Hello again b\n");
        assert!(
            wait_for_file(&d.join("b.md"), "Hello again b\n"),
            "an edit after the panic rebuilds"
        );
        assert!(
            running(&mut session),
            "a compile that panics does not end the session"
        );
        assert_eq!(
            interrupt(&mut session),
            Some(101),
            "stopped, a session that caught a panic exits 101"
        );
        assert_only_the_text(&stderr.finish_text(&mut session), 1, "directory");
        assert!(
            !d.join("a.md").exists(),
            "a compile that panicked writes no output"
        );
    }

    // ── A panic on a watch session's own threads ends it at once ──────────────

    /// How `mds watch` is started for a thread-trigger test: on a file, or on a directory.
    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum Watched {
        File,
        Directory,
    }

    /// A working directory for a [`Watched`] session: `x.mds`, or `d/x.mds`.
    #[cfg(unix)]
    struct WatchFixture {
        dir: tempfile::TempDir,
        watched: Watched,
    }

    #[cfg(unix)]
    impl WatchFixture {
        fn new(watched: Watched) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let fixture = Self { dir, watched };
            if let Some(parent) = fixture.source().parent() {
                std::fs::create_dir_all(parent).expect("create the source's directory");
            }
            std::fs::write(fixture.source(), "Hello\n").expect("write the source");
            fixture
        }

        fn dir(&self) -> &Path {
            self.dir.path()
        }

        /// What `mds watch` is given.
        fn args(&self) -> &'static [&'static str] {
            match self.watched {
                Watched::File => &["x.mds"],
                Watched::Directory => &["d"],
            }
        }

        fn source(&self) -> PathBuf {
            match self.watched {
                Watched::File => self.dir().join("x.mds"),
                Watched::Directory => self.dir().join("d").join("x.mds"),
            }
        }

        fn output_file(&self) -> PathBuf {
            self.source().with_extension("md")
        }
    }

    /// A panic in `mds watch`'s Ctrl-C handler, which runs on a thread of its own, ends the
    /// session at once (#389): the text once and exit 101 — not the 0 of a session Ctrl-C
    /// stopped — within [`PANIC_EXIT_BOUND`].
    ///
    /// Control: without the trigger Ctrl-C stops the session with 0.
    #[cfg(unix)]
    #[test]
    fn a_panic_in_the_watch_ctrl_c_handler_ends_the_session_with_101() {
        for watched in [Watched::File, Watched::Directory] {
            let fixture = WatchFixture::new(watched);
            let (mut control, stderr) = watch_live(fixture.dir(), fixture.args(), None);
            assert_eq!(interrupt(&mut control), Some(0), "control ({watched:?})");
            assert_eq!(
                stderr.finish_text(&mut control),
                "",
                "control ({watched:?})"
            );

            let (mut session, stderr) = watch_live(fixture.dir(), fixture.args(), Some("ctrlc"));
            let started = Instant::now();
            assert_eq!(
                interrupt(&mut session),
                Some(101),
                "{watched:?}: a panic in the Ctrl-C handler ends the session with 101"
            );
            assert!(
                started.elapsed() < PANIC_EXIT_BOUND,
                "{watched:?}: at once; it took {:?}",
                started.elapsed()
            );
            assert_eq!(
                stderr.finish_text(&mut session),
                ICE_TEXT,
                "{watched:?}: the text, once"
            );
        }
    }

    /// A panic in `mds watch`'s file-event callback, which runs on notify's thread, ends the
    /// session at once (#389): the text once and exit 101, within [`PANIC_EXIT_BOUND`] of
    /// the event. Without that the thread would die alone, and the session would watch on
    /// without its events. The session's own startup write can be the first event;
    /// otherwise an edit is.
    ///
    /// Control: without the trigger an edit reaches the callback and rebuilds.
    #[cfg(unix)]
    #[test]
    fn a_panic_in_the_watch_file_event_callback_ends_the_session_with_101() {
        for watched in [Watched::File, Watched::Directory] {
            let fixture = WatchFixture::new(watched);
            let (mut control, stderr) = watch_live(fixture.dir(), fixture.args(), None);
            assert!(
                wait_for_file(&fixture.output_file(), "Hello\n"),
                "control ({watched:?})"
            );
            write_atomic(&fixture.source(), "Hello again\n");
            assert!(
                wait_for_file(&fixture.output_file(), "Hello again\n"),
                "control ({watched:?}): the edit reaches the callback"
            );
            assert_eq!(interrupt(&mut control), Some(0), "control ({watched:?})");
            assert_eq!(
                stderr.finish_text(&mut control),
                "",
                "control ({watched:?})"
            );

            let fixture = WatchFixture::new(watched);
            let ready_dir = tempfile::tempdir().expect("tempdir");
            let ready = ready_dir.path().join("watch-ready");
            let (mut session, stderr) =
                watch_starting(fixture.dir(), fixture.args(), Some("notify"), &ready);
            let deadline = Instant::now() + WATCH_STEP;
            let mut since = Instant::now();
            let mut edited = false;
            let code = loop {
                if let Some(status) = session.0.try_wait().expect("poll the session") {
                    break status.code();
                }
                if !edited && ready.exists() {
                    write_atomic(&fixture.source(), "Hello again\n");
                    since = Instant::now();
                    edited = true;
                }
                assert!(
                    Instant::now() < deadline,
                    "{watched:?}: a panic in the file-event callback must end the session; \
                     it was still running after {WATCH_STEP:?} (edited: {edited}); \
                     stderr:\n{}",
                    stderr.text()
                );
                std::thread::sleep(POLL);
            };
            assert!(
                since.elapsed() < PANIC_EXIT_BOUND,
                "{watched:?}: at once; it took {:?}",
                since.elapsed()
            );
            assert_eq!(
                code,
                Some(101),
                "{watched:?}: a panic in the file-event callback ends the session with 101"
            );
            assert_eq!(
                stderr.finish_text(&mut session),
                ICE_TEXT,
                "{watched:?}: the text, once"
            );
        }
    }
}

/// With the never-shipped `debug-panics` feature: the panic's message and location follow
/// the text, escaped.
#[cfg(feature = "debug-panics")]
mod with_debug_panics {
    use super::*;

    /// The six-character escape the CLI writes in place of `ch` (a backslash, `u` and four
    /// hex digits), built at runtime.
    fn escaped(ch: char) -> String {
        format!("{}u{:04X}", '\\', u32::from(ch))
    }

    /// The payload the default tests look for is the one the trigger panics with: its
    /// sentinel, its ESC (escaped) and the working directory's path follow the text, with
    /// the location of the panic.
    #[test]
    fn the_message_and_location_follow_the_text_escaped() {
        let dir = fixture();
        let run = run_mds(dir.path(), Some("main"), Backtrace::Unset, Stderr::Piped);
        assert_eq!(run.code, Some(101), "stderr:\n{}", run.stderr);
        let detail = run.stderr.strip_prefix(ICE_TEXT).unwrap_or_else(|| {
            panic!(
                "stderr starts with the internal compiler error text; it was:\n{}",
                run.stderr
            )
        });
        let canonical = dir.path().canonicalize().expect("canonicalize");
        for needle in [
            SENTINEL.to_string(),
            escaped(char::from(0x1b)),
            canonical.display().to_string(),
            "output.rs".to_string(),
        ] {
            assert!(
                detail.contains(needle.as_str()),
                "the detail shows {needle:?}; it was:\n{detail}"
            );
        }
        assert!(
            !run.stderr.contains(char::from(0x1b)),
            "the detail is escaped; stderr:\n{}",
            run.stderr
        );
    }
}

// ── Lexical pins ──────────────────────────────────────────────────────────────

/// Every run here sets or removes `RUST_BACKTRACE`: the function holding a `.spawn(` /
/// `.output(` / `.status(` call, or a call of a shared helper that spawns `mds watch`
/// ([`SPAWN_HELPERS`]), also calls `env(RUST_BACKTRACE, …)` or
/// `env_remove(RUST_BACKTRACE)` in code, not in a comment. CI sets `RUST_BACKTRACE=1`
/// globally, which would otherwise flip every arm that expects no backtrace.
#[test]
fn every_run_sets_or_removes_rust_backtrace() {
    let own = read_source("tests/panic_hook.rs");
    let found = runs_without_rust_backtrace(&own);
    assert!(
        found.runs >= 1,
        "non-vacuity: panic_hook.rs starts a process somewhere"
    );
    assert!(
        found.stray.is_empty(),
        "a run in panic_hook.rs leaves RUST_BACKTRACE to the environment, at lines {:?}",
        found.stray
    );

    // Controls: a run that never names it, and one that names it only in a comment, are
    // reported; a run that sets it or removes it is not.
    let bare = "fn f() {\n    let _ = mds_bin().spawn();\n}\n";
    assert_eq!(runs_without_rust_backtrace(bare).stray, vec![2]);
    let commented =
        "fn f() {\n    // cmd.env_remove(RUST_BACKTRACE);\n    let _ = cmd.output();\n}\n";
    assert_eq!(runs_without_rust_backtrace(commented).stray, vec![3]);
    let set = "fn f() {\n    cmd.env(RUST_BACKTRACE, \"1\");\n    let _ = cmd.status();\n}\n";
    assert_eq!(runs_without_rust_backtrace(set).stray, Vec::<usize>::new());
    let removed = "fn f() {\n    cmd.env_remove(RUST_BACKTRACE);\n    let _ = cmd.spawn();\n}\n";
    assert_eq!(
        runs_without_rust_backtrace(removed).stray,
        Vec::<usize>::new()
    );
    for helper in SPAWN_HELPERS {
        let bare = format!("fn f() {{\n    let _ = {helper}&mut cmd);\n}}\n");
        assert_eq!(
            runs_without_rust_backtrace(&bare).stray,
            vec![2],
            "a bare `{helper}` must be reported"
        );
        let removed = format!(
            "fn f() {{\n    cmd.env_remove(RUST_BACKTRACE);\n    let _ = {helper}&mut cmd);\n}}\n"
        );
        assert_eq!(
            runs_without_rust_backtrace(&removed).stray,
            Vec::<usize>::new()
        );
    }
}

/// The helpers of `tests/common` that spawn `mds watch`, as a call starts.
const SPAWN_HELPERS: &[&str] = &[
    "spawn_watch_ready(",
    "spawn_watch_unsynchronized(",
    "spawn_watch_ready_stderr_untapped(",
];

/// The panic hook — `on_panic` in `src/output.rs` — records the panic before anything
/// else, then writes one constant and nothing about the panic: exactly one `write_all`,
/// of `ICE_TEXT`; no print or format macro, no miette, no `.payload()` or `.location()`,
/// nothing that can panic; the hook's `PanicHookInfo` named only on the lines the
/// `debug-panics` feature gates. `main` installs it first. The panic machinery stays
/// where [`CLI_PANIC_MACHINERY`] and [`CORE_PANIC_MACHINERY`] list it: only
/// `install_panic_hook` sets a hook and only `catch_panic` catches a panic, so no catch
/// site can report one a second time, and nothing unwinds without the hook.
///
/// Controls: each of those, planted into the real hook's body or the real sources, is
/// reported.
#[test]
fn the_hook_writes_one_constant_and_nothing_about_the_panic() {
    let output = read_source("src/output.rs");
    let found = hook_findings(&output);
    assert!(
        found.is_empty(),
        "the panic hook must write only the internal compiler error text:\n{}",
        found.join("\n")
    );

    const ICE_WRITE: &str = "let _ = stderr.write_all(ICE_TEXT.as_bytes());";
    assert!(
        output.contains(ICE_WRITE),
        "precondition: the hook writes the text with `{ICE_WRITE}`"
    );
    for plant in [
        "eprintln!(\"planted\");",
        "crate::output::ewriteln!(\"planted\");",
        "let _ = format!(\"planted\");",
        "let _ = std::fmt::Write::write_fmt(&mut text, format_args!(\"planted\"));",
        "miette::miette!(\"planted\");",
        "let _ = info.payload();",
        "let _ = info.location();",
        "let _ = stderr.write_all(b\"planted\");",
        "let _ = std::io::stderr().lock().unwrap();",
    ] {
        let planted = output.replacen(ICE_WRITE, &format!("{ICE_WRITE}\n    {plant}"), 1);
        assert!(
            !hook_findings(&planted).is_empty(),
            "`{plant}` planted in the hook must be reported"
        );
    }
    let not_constant = output.replacen(ICE_WRITE, "let _ = stderr.write_all(text.as_bytes());", 1);
    assert!(
        !hook_findings(&not_constant).is_empty(),
        "a hook that writes anything but ICE_TEXT must be reported"
    );
    // The panic recorded after the write: a hook blocked in the write would leave the
    // run to exit without it.
    const RECORD: &str = "OUTPUT_STATE.note_panicked();\n";
    let record_last = output.replacen(RECORD, "", 1).replacen(
        ICE_WRITE,
        &format!("{ICE_WRITE}\n    {RECORD}"),
        1,
    );
    assert_ne!(
        record_last, output,
        "precondition: the hook records the panic with `{RECORD}`"
    );
    assert!(
        !hook_findings(&record_last).is_empty(),
        "a hook that writes before it records the panic must be reported"
    );

    // Installed first in `main`.
    let main = read_source("src/main.rs");
    let main_code = blank(&main, true);
    let body = fn_body(&main_code, "main").expect("main.rs defines `fn main`");
    let first = main_code[*body.start() + 1..].trim_start();
    assert!(
        first.starts_with("output::install_panic_hook();"),
        "`main` installs the panic hook before anything else; its body starts:\n{}",
        &first[..first.len().min(120)]
    );

    // The panic machinery, in mds-cli and in mds-core, whose code runs inside the CLI's
    // catch.
    let cli = crate_sources();
    let core = core_sources();
    for (krate, sources, allowed) in [
        ("mds-cli", &cli, CLI_PANIC_MACHINERY),
        ("mds-core", &core, CORE_PANIC_MACHINERY),
    ] {
        let found = machinery_findings(sources, allowed);
        assert!(
            found.is_empty(),
            "{krate}: the panic machinery must stay where it is listed:\n{}",
            found.join("\n")
        );
    }
    let plant = |sources: &[(String, String)], file: &str, body: &str| {
        let mut planted = sources.to_vec();
        let (_, text) = planted
            .iter_mut()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("precondition: the sources hold {file}"));
        text.push_str(&format!("\nfn planted() {{\n    {body}\n}}\n"));
        planted
    };
    let missed: Vec<&str> = [
        (
            machinery_findings(
                &plant(&cli, "main.rs", "std::panic::resume_unwind(Box::new(0u8));"),
                CLI_PANIC_MACHINERY,
            ),
            "a `resume_unwind` in main.rs",
        ),
        (
            machinery_findings(
                &plant(
                    &cli,
                    "output.rs",
                    "let _ = std::panic::catch_unwind(|| ());",
                ),
                CLI_PANIC_MACHINERY,
            ),
            "a second `catch_unwind` in output.rs",
        ),
        (
            machinery_findings(
                &plant(&cli, "watch.rs", "let _ = std::panic::take_hook();"),
                CLI_PANIC_MACHINERY,
            ),
            "a `take_hook` in watch.rs",
        ),
        (
            machinery_findings(
                &plant(&core, "lib.rs", "let _ = std::panic::catch_unwind(|| ());"),
                CORE_PANIC_MACHINERY,
            ),
            "a `catch_unwind` in mds-core's lib.rs",
        ),
        (
            machinery_findings(
                &plant(&core, "lib.rs", "std::panic::set_hook(Box::new(|_| ()));"),
                CORE_PANIC_MACHINERY,
            ),
            "a `set_hook` in mds-core's lib.rs",
        ),
        (
            machinery_findings(
                &plant(&core, "lib.rs", "std::panic::resume_unwind(Box::new(0u8));"),
                CORE_PANIC_MACHINERY,
            ),
            "a `resume_unwind` in mds-core's lib.rs",
        ),
        (
            machinery_findings(
                &cli,
                &[
                    CLI_PANIC_MACHINERY,
                    &[("take_hook", "output.rs", "install_panic_hook")],
                ]
                .concat(),
            ),
            "a listed place that does not name its function",
        ),
    ]
    .into_iter()
    .filter(|(found, _)| found.is_empty())
    .map(|(_, what)| what)
    .collect();
    assert!(
        missed.is_empty(),
        "each of these must be reported; missed: {missed:?}"
    );
}

/// The trigger compiles only into a debug build: every mention of `MDS_TEST_PANIC` and
/// every `panic_any` in the crate sit inside `mod panic_trigger`, whose attributes hold
/// `#[cfg(debug_assertions)]`; each of its functions ([`TRIGGER_FNS`]) has a release
/// build's stub that does nothing. The payload the module builds carries the sentinel
/// these tests look for.
///
/// Controls: the module without its `cfg`, a mention outside it, a release stub that does
/// something, for the dispatch's trigger and for the compile's, and a trigger function
/// without a release stub are each reported.
#[test]
fn the_trigger_is_compiled_only_into_debug_builds() {
    let output = read_source("src/output.rs");
    let sources = crate_sources();
    let found = trigger_findings(&sources);
    assert!(
        found.is_empty(),
        "the panic trigger must be compiled only into debug builds:\n{}",
        found.join("\n")
    );

    let with_literals = blank(&output, false);
    let module = mod_body(&blank(&output, true), "panic_trigger")
        .expect("output.rs holds `mod panic_trigger`");
    let module_text = &with_literals[module];
    for needle in [
        format!("\"{SENTINEL}\""),
        "0x1b".to_string(),
        "current_dir".to_string(),
    ] {
        assert!(
            module_text.contains(needle.as_str()),
            "the trigger's payload is built from {needle}"
        );
    }

    let with = |from: &str, to: &str| -> Vec<(String, String)> {
        let planted = output.replacen(from, to, 1);
        assert_ne!(planted, output, "precondition: output.rs holds {from:?}");
        sources
            .iter()
            .map(|(name, src)| {
                let src = if name == "output.rs" { &planted } else { src };
                (name.clone(), src.clone())
            })
            .collect()
    };
    let ungated = with(
        "#[cfg(debug_assertions)]\nmod panic_trigger",
        "mod panic_trigger",
    );
    assert!(
        !trigger_findings(&ungated).is_empty(),
        "a trigger module without `#[cfg(debug_assertions)]` must be reported"
    );
    let outside = with(
        "pub(crate) fn panic_on_request() {}",
        "pub(crate) fn panic_on_request() {\n    let _ = std::env::var_os(\"MDS_TEST_PANIC\");\n}",
    );
    assert!(
        !trigger_findings(&outside).is_empty(),
        "a trigger outside the module must be reported"
    );
    let busy_stub = with(
        "pub(crate) fn panic_on_request() {}",
        "pub(crate) fn panic_on_request() {\n    let _ = 1;\n}",
    );
    assert!(
        !trigger_findings(&busy_stub).is_empty(),
        "a release stub that does anything must be reported"
    );
    let busy_compile_stub = with(
        "fn panic_on_compile(_label: &Path) {}",
        "fn panic_on_compile(_label: &Path) {\n    let _ = 1;\n}",
    );
    assert!(
        !trigger_findings(&busy_compile_stub).is_empty(),
        "a compile trigger's release stub that does anything must be reported"
    );
    let no_stub = with("pub(crate) fn panic_in_handler(_handler: &str) {}", "");
    assert!(
        !trigger_findings(&no_stub).is_empty(),
        "a trigger function without a release stub must be reported"
    );
}

/// The pause between a rewrite's read and its replace (#160) compiles only into a debug
/// build, as the panic trigger does: every mention of `MDS_TEST_PAUSE_BEFORE_REPLACE` sits
/// inside `mod pause_trigger`, whose attributes hold `#[cfg(debug_assertions)]`, and
/// `pause_before_replace` has a release build's stub that does nothing.
///
/// Controls: the module without its `cfg`, a mention outside it, a release stub that does
/// something, and no release stub are each reported.
#[test]
fn the_pause_before_a_replace_is_compiled_only_into_debug_builds() {
    let write = read_source("src/write.rs");
    let sources = crate_sources();
    let found = gate_findings(&sources, &PAUSE_TRIGGER);
    assert!(
        found.is_empty(),
        "the pause before a replace must be compiled only into debug builds:\n{}",
        found.join("\n")
    );

    let with = |from: &str, to: &str| -> Vec<(String, String)> {
        let planted = write.replacen(from, to, 1);
        assert_ne!(planted, write, "precondition: write.rs holds {from:?}");
        sources
            .iter()
            .map(|(name, src)| {
                let src = if name == "write.rs" { &planted } else { src };
                (name.clone(), src.clone())
            })
            .collect()
    };
    let stub = "pub(crate) fn pause_before_replace() {}";
    for (planted, what) in [
        (
            with(
                "#[cfg(debug_assertions)]\nmod pause_trigger",
                "mod pause_trigger",
            ),
            "a pause module without `#[cfg(debug_assertions)]`",
        ),
        (
            with(
                stub,
                "pub(crate) fn pause_before_replace() {\n    \
                 let _ = std::env::var_os(\"MDS_TEST_PAUSE_BEFORE_REPLACE\");\n}",
            ),
            "the pause's variable outside its module",
        ),
        (
            with(
                stub,
                "pub(crate) fn pause_before_replace() {\n    let _ = 1;\n}",
            ),
            "a release stub that does something",
        ),
        (with(stub, ""), "a pause without a release stub"),
    ] {
        assert!(
            !gate_findings(&planted, &PAUSE_TRIGGER).is_empty(),
            "{what} must be reported"
        );
    }
}

/// A panic in one file's compile fails that file alone (#389), so each per-file catch — a
/// `catch_compile` call — wraps that compile and nothing else: its closure is
/// `AssertUnwindSafe(|| <one call>)`, the call is one of [`COMPILE_CALLS`] as written
/// there, and no argument of it writes, deletes or changes state. Each of those mds-cli
/// defines is defined once, in its file, and neither it nor a function of mds-cli it
/// reaches writes, renames or deletes a file, writes to stdout or ends the process
/// ([`compile_reach_findings`]). A panic abandons the closure part-way, so only what the
/// closure holds is left unfinished. The catches sit where [`COMPILE_CATCHES`] lists
/// them, one per compile of a batch, and `catch_panic`, which catches around anything,
/// only where [`PANIC_CATCHES`] lists it.
///
/// The check is lexical: it follows calls by name, and does not see into a macro,
/// mds-core, or a call through a function pointer or a trait object.
///
/// Controls, each planted into the real sources and reported for its own reason: a state
/// change beside the compile, a write in its place, an argument that changes state, a
/// mutable borrow, a call chained onto the compile, a closure not wrapped in
/// `AssertUnwindSafe`, a catch gone from a listed place, one in an unlisted place, a
/// `catch_panic` outside its places, a helper named like the compile in its place, a
/// write inside a compile, a write in a function it calls, a delete in a method it calls,
/// an exit inside a compile, and a second compile of the same name. A write that no catch
/// reaches is not reported.
#[test]
fn each_catch_wraps_one_compile_call_and_nothing_else() {
    let sources = crate_sources();
    let found = catch_findings(&sources);
    assert!(
        found.is_empty(),
        "each catch must wrap one compile call and nothing else:\n{}",
        found.join("\n")
    );

    let replaced = |file: &str, from: &str, to: &str| -> Vec<(String, String)> {
        let mut planted = sources.to_vec();
        let (_, text) = planted
            .iter_mut()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("precondition: the sources hold {file}"));
        let changed = text.replacen(from, to, 1);
        assert_ne!(&changed, text, "precondition: {file} holds {from:?}");
        *text = changed;
        planted
    };
    let appended = |file: &str, body: &str| -> Vec<(String, String)> {
        let mut planted = sources.to_vec();
        let (_, text) = planted
            .iter_mut()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("precondition: the sources hold {file}"));
        text.push_str(&format!("\nfn planted(src: &Path) {{\n    {body}\n}}\n"));
        planted
    };
    let defined = |file: &str, item: &str| -> Vec<(String, String)> {
        let mut planted = sources.to_vec();
        let (_, text) = planted
            .iter_mut()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("precondition: the sources hold {file}"));
        text.push_str(&format!("\n{item}\n"));
        planted
    };
    let plants = [
        (
            replaced(
                "build.rs",
                "AssertUnwindSafe(|| compile_to_content(file, runtime_vars.clone(), quiet, opts))",
                "AssertUnwindSafe(|| { fail_count += 1; compile_to_content(file, \
                 runtime_vars.clone(), quiet, opts) })",
            ),
            "more than one call",
            "a state change beside the compile",
        ),
        (
            replaced(
                "fmt.rs",
                "AssertUnwindSafe(|| format_source_named(&source, base_dir, &file_name))",
                "AssertUnwindSafe(|| atomic_write_file(file, &source, Durability::Fsync))",
            ),
            "not a compile",
            "a write in place of the compile",
        ),
        (
            replaced(
                "lint.rs",
                "mds::lint(path, ctx.runtime_vars.clone(), &config)",
                "mds::lint(path, ctx.base_dir_cache.borrow_mut().remove(path), &config)",
            ),
            "changes state",
            "an argument that changes state",
        ),
        (
            replaced(
                "build.rs",
                "compile_to_content(file, runtime_vars.clone(), quiet, opts)",
                "compile_to_content(file, runtime_vars.clone(), quiet, &mut opts)",
            ),
            "changes state",
            "a mutable borrow",
        ),
        (
            replaced(
                "main.rs",
                "mds::check_collecting_warnings(file, runtime_vars.clone()))",
                "mds::check_collecting_warnings(file, runtime_vars.clone()).map(|c| c))",
            ),
            "more than its one call",
            "a call chained onto the compile",
        ),
        (
            replaced(
                "main.rs",
                "AssertUnwindSafe(|| mds::check_collecting_warnings(file, runtime_vars.clone()))",
                "|| mds::check_collecting_warnings(file, runtime_vars.clone())",
            ),
            "AssertUnwindSafe",
            "a closure not wrapped in `AssertUnwindSafe`",
        ),
        (
            replaced("fmt.rs", "= catch_compile(", "= uncaught("),
            "must hold one",
            "a catch gone from a listed place",
        ),
        (
            appended(
                "watch.rs",
                "let _ = crate::output::catch_compile(src, AssertUnwindSafe(|| \
                 compile_to_content(src, None, true, mds::CompileOptions::default())));",
            ),
            "not a listed place",
            "a catch in an unlisted place",
        ),
        (
            appended("watch.rs", "let _ = crate::output::catch_panic(|| src);"),
            "outside its places",
            "a `catch_panic` outside its places",
        ),
        (
            replaced(
                "lint.rs",
                "AssertUnwindSafe(|| mds::lint(path, ctx.runtime_vars.clone(), &config))",
                "AssertUnwindSafe(|| lint(path, ctx.runtime_vars.clone(), &config))",
            ),
            "not a compile",
            "a helper named like the compile in its place",
        ),
        (
            replaced(
                "build.rs",
                "let kind = OutputKind::from(&result.output);",
                "crate::output::atomic_write_file(input, \"\", Durability::Fsync)?;\n    \
                 let kind = OutputKind::from(&result.output);",
            ),
            "which calls `atomic_write_file`",
            "a write inside the compile a catch calls",
        ),
        (
            replaced(
                "build.rs",
                "CompiledOutput::Markdown(s) => Ok(s),",
                "CompiledOutput::Markdown(s) => { std::fs::write(\"x\", &s).ok(); Ok(s) }",
            ),
            "which calls `fs::write`",
            "a write in a function the compile calls",
        ),
        (
            replaced(
                "lint.rs",
                "let residual = mds::lint_str_named(",
                "std::fs::remove_file(self.base_dir).ok();\n        \
                 let residual = mds::lint_str_named(",
            ),
            "which calls `remove_file`",
            "a delete in a method the compile calls",
        ),
        (
            replaced(
                "fmt.rs",
                "let changed = formatted != source;",
                "crate::output::exit(1);\n    let changed = formatted != source;",
            ),
            "which calls `exit`",
            "an exit in the formatter a catch calls",
        ),
        (
            defined(
                "watch.rs",
                "fn compile_to_content(input: &Path) -> Result<()> {\n    \
                 crate::output::atomic_write_file(input, \"\", Durability::Fsync)\n}",
            ),
            "must be defined once",
            "a second compile of the same name",
        ),
    ];
    let mut missed = Vec::new();
    for (planted, reason, what) in &plants {
        let found = catch_findings(planted);
        if !found.iter().any(|finding| finding.contains(reason)) {
            missed.push(format!("{what} (expected {reason:?}; found {found:?})"));
        }
    }
    assert!(
        missed.is_empty(),
        "each of these must be reported:\n{}",
        missed.join("\n")
    );

    let unreached = defined(
        "watch.rs",
        "fn planted_unreached() {\n    std::fs::write(\"x\", \"\").ok();\n}",
    );
    let found = catch_findings(&unreached);
    assert!(
        found.is_empty(),
        "control: a write that no catch reaches is not reported; found {found:?}"
    );
}

/// `debug-panics` is declared, enables nothing, and is off unless asked for: nothing in
/// `mds-cli`'s manifest names it as a value, so no `default` feature and no other feature
/// turns it on, however the list is written.
///
/// Controls: each way of turning it on, a declaration that enables something, and a
/// manifest that does not declare it are reported; a mention in a comment is not.
#[test]
fn debug_panics_is_never_on_by_default() {
    let manifest = read_source("Cargo.toml");
    let found = feature_findings(&manifest);
    assert!(
        found.is_empty(),
        "mds-cli's `debug-panics` feature must stay off by default:\n{}",
        found.join("\n")
    );
    let declared = "debug-panics = []";
    let after_declared =
        |extra: &str| manifest.replacen(declared, &format!("{declared}\n{extra}"), 1);
    let plants = [
        (
            after_declared("default = [\"debug-panics\"]"),
            "a default that enables it",
        ),
        (
            after_declared("default = [\n    \"debug-panics\",\n]"),
            "a default over several lines that enables it",
        ),
        (
            after_declared("default = ['debug-panics']"),
            "a default that names it in a literal string",
        ),
        (
            after_declared("verbose = [\"startup-race-probe\", \"debug-panics\"]"),
            "another feature that enables it",
        ),
        (
            after_declared("verbose = [\"a#b\", \"debug-panics\"]"),
            "a feature that enables it after a `#` inside a string",
        ),
        (
            format!("features.default = [\"debug-panics\"]\n{manifest}"),
            "a dotted `features.default` that enables it",
        ),
        (
            manifest.replacen(declared, "debug-panics = [\"startup-race-probe\"]", 1),
            "a declaration that enables another feature",
        ),
        (
            manifest.replacen(declared, "", 1),
            "a manifest that does not declare it",
        ),
    ];
    let mut missed = Vec::new();
    for (planted, what) in &plants {
        assert_ne!(
            planted, &manifest,
            "precondition: the plant changes Cargo.toml ({what})"
        );
        if feature_findings(planted).is_empty() {
            missed.push(*what);
        }
    }
    assert!(
        missed.is_empty(),
        "each of these must be reported; missed: {missed:?}"
    );
    let commented = after_declared("# default = [\"debug-panics\"]");
    assert_eq!(
        feature_findings(&commented),
        Vec::<String>::new(),
        "a mention in a comment turns nothing on"
    );
}

// ── Lexical helpers ───────────────────────────────────────────────────────────

/// A file of `crates/mds-cli`, by its path under the package.
fn read_source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `crates/mds-cli/src/`, by its path below `src/`, with its text.
fn crate_sources() -> Vec<(String, String)> {
    rust_sources("src", "main.rs", 8)
}

/// Every `.rs` file under `crates/mds-core/src/`, by its path below `src/` (`lib.rs`,
/// `lint/mod.rs`), with its text.
fn core_sources() -> Vec<(String, String)> {
    rust_sources("../mds-core/src", "lib.rs", 20)
}

/// Every `.rs` file under `relative` (from this package's directory), by its path below
/// it with `/` between names, with its text. Non-vacuity: the walk found `root_file` and
/// at least `at_least` files.
fn rust_sources(relative: &str, root_file: &str, at_least: usize) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let mut pending: Vec<PathBuf> = vec![root.clone()];
    let mut files = Vec::new();
    for _ in 0..64 {
        let Some(dir) = pending.pop() else { break };
        for entry in std::fs::read_dir(&dir).expect("read a source directory") {
            let path = entry.expect("read a source directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let below = path.strip_prefix(&root).expect("a path below the root");
                let name = below
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let text = std::fs::read_to_string(&path).expect("read a source");
                files.push((name, text));
            }
        }
    }
    assert!(
        pending.is_empty(),
        "{relative} nests deeper than the walk's bound"
    );
    assert!(
        files.iter().any(|(name, _)| name == root_file) && files.len() >= at_least,
        "non-vacuity: the walk of {relative} found {root_file} and at least {at_least} \
         files; found {}",
        files.len()
    );
    files.sort();
    files
}

/// The std functions that change what a panic does: set or take the panic hook, catch a
/// panic, or unwind without calling the hook at all.
const PANIC_MACHINERY: &[&str] = &["set_hook", "take_hook", "catch_unwind", "resume_unwind"];

/// Where mds-cli's sources name the [`PANIC_MACHINERY`], as (function, file, the `fn` it
/// is named in): the hook is set in one place and a panic is caught in one place, so no
/// catch site reports a panic a second time. `resume_unwind` has no place: it unwinds
/// without calling the hook, so its panic prints nothing and records nothing for the
/// exit code, and on a thread that nothing catches it ends that thread alone.
const CLI_PANIC_MACHINERY: &[(&str, &str, &str)] = &[
    ("set_hook", "output.rs", "install_panic_hook"),
    ("catch_unwind", "output.rs", "catch_panic"),
];

/// Where mds-core's sources name the [`PANIC_MACHINERY`]: two unit tests, each catching
/// the panic of a check that debug builds compile in. mds-core's library code runs inside
/// the CLI's catch, where a hook or a catch of its own would take a panic away from the
/// CLI's hook, and an unwind without the hook would report nothing.
const CORE_PANIC_MACHINERY: &[(&str, &str, &str)] = &[
    (
        "catch_unwind",
        "resolver_tests.rs",
        "attach_import_span_non_boundary_offset_degrades_to_zero_len_span",
    ),
    (
        "catch_unwind",
        "resolver_tests.rs",
        "check_child_only_blocks_non_boundary_offset_degrades",
    ),
];

/// What is wrong with the panic machinery in `sources` (see
/// [`the_hook_writes_one_constant_and_nothing_about_the_panic`]); empty when nothing is:
/// every [`PANIC_MACHINERY`] name in code outside the places `allowed` lists, and every
/// listed place that does not name its function exactly once.
fn machinery_findings(sources: &[(String, String)], allowed: &[(&str, &str, &str)]) -> Vec<String> {
    let mut found = Vec::new();
    let mut named = vec![0usize; allowed.len()];
    for (name, src) in sources {
        let code = blank(src, true);
        for needle in PANIC_MACHINERY {
            for (at, _) in code.match_indices(needle) {
                let inside = innermost_fn(&code, at).map(|(fn_name, _)| fn_name);
                let place = allowed.iter().position(|(function, file, in_fn)| {
                    function == needle && file == name && inside.as_deref() == Some(*in_fn)
                });
                match place {
                    Some(place) => named[place] += 1,
                    None => found.push(format!(
                        "{name}:{}: `{needle}` in `{}`",
                        line_of(&code, at),
                        inside.as_deref().unwrap_or("-")
                    )),
                }
            }
        }
    }
    for ((function, file, in_fn), count) in allowed.iter().zip(named) {
        if count != 1 {
            found.push(format!(
                "{file}: `{in_fn}` must name `{function}` once; it names it {count} times"
            ));
        }
    }
    found
}

/// Where each per-file catch sits in mds-cli's sources, as (file, the `fn` it is in): the
/// compile of one file of a batch — a directory run, or a watch session (#389).
const COMPILE_CATCHES: &[(&str, &str)] = &[
    ("build.rs", "run_build_directory"),
    ("fmt.rs", "format_one_file"),
    ("lint.rs", "lint_dir_entry"),
    ("lint.rs", "lint_input"),
    ("main.rs", "run_check_directory"),
    ("watch.rs", "compile"),
    ("watch.rs", "compile_source"),
];

/// What a catch's closure may call, as the call is written there, and the file of mds-cli
/// that defines it — `None` for a function of mds-core. Each compiles, checks, formats or
/// lints one file, and writes nothing: [`compile_reach_findings`] follows the ones mds-cli
/// defines.
const COMPILE_CALLS: &[(&str, Option<&str>)] = &[
    ("compile_to_content", Some("build.rs")),
    ("mds::check_collecting_warnings", None),
    ("format_source_named", Some("fmt.rs")),
    ("mds::lint", None),
    ("run_fix_pipeline", Some("lint.rs")),
];

/// What a compile a catch wraps must never reach, through the functions of mds-cli it
/// calls: a write, rename or delete of a file, a write to stdout, or an end of the process.
/// A call matches when its path ends with the entry, so `fs::write` is `std::fs::write(`.
/// Stderr's status lines — a compile's warnings — are not in it: the writer formats a line
/// before it writes it, so a panic leaves no part of one.
const WRITES: &[&str] = &[
    "atomic_write_file",
    "write_output",
    "write_session_output",
    "write_stdout",
    "fs::write",
    "fs::copy",
    "File::create",
    "OpenOptions::new",
    "persist",
    "persist_noclobber",
    "rename",
    "remove_file",
    "remove_dir",
    "remove_dir_all",
    "create_dir",
    "create_dir_all",
    "hard_link",
    "set_permissions",
    "probe_and_remove_stale",
    "verify_then_delete_map",
    "exit",
    "exit_after_panic",
];

/// The first path segments of a call into another crate — mds-cli's dependencies and the
/// standard library — which [`compile_reach_findings`] does not follow.
const OTHER_CRATES: &[&str] = &[
    "mds",
    "std",
    "core",
    "alloc",
    "clap",
    "serde",
    "serde_json",
    "miette",
    "notify",
    "ctrlc",
    "similar",
    "tempfile",
];

/// What the compile call's arguments may not hold: a write, a delete, a change to state, a
/// mutable borrow, an assignment, a macro, a closure, a statement.
const STATE_CHANGES: &[&str] = &[
    "write", "remove", "delete", "rename", "create", "insert", "push", "clear", "take", "exit",
    "print", "&mut", "=", "!", "|", ";", "{",
];

/// Where mds-cli calls `catch_panic` outside its test modules, as (file, the `fn` it is
/// in): `main`, around the whole command, and `catch_compile`.
const PANIC_CATCHES: &[(&str, &str)] = &[("main.rs", "main"), ("output.rs", "catch_compile")];

/// What is wrong with the per-file catches in `sources` (see
/// [`each_catch_wraps_one_compile_call_and_nothing_else`]); empty when nothing is.
fn catch_findings(sources: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    let mut placed: Vec<(String, String)> = Vec::new();
    for (name, src) in sources {
        let code = blank(src, true);
        let tests = mod_body(&code, "tests");
        // A call of `function` outside the test module: a `(` follows its name.
        let calls = |function: &str| -> Vec<(usize, String)> {
            ident_positions(&code, function)
                .into_iter()
                .filter(|at| {
                    code[at + function.len()..].trim_start().starts_with('(')
                        && !tests.as_ref().is_some_and(|t| t.contains(at))
                })
                .map(|at| {
                    let inside = innermost_fn(&code, at).map_or_else(|| "-".to_string(), |f| f.0);
                    (at, inside)
                })
                .collect()
        };
        for (at, inside) in calls("catch_compile") {
            if let Err(reason) = catch_closure(&code, at) {
                found.push(format!(
                    "{name}:{} (`{inside}`): {reason}",
                    line_of(&code, at)
                ));
            }
            placed.push((name.clone(), inside));
        }
        for (at, inside) in calls("catch_panic") {
            if !PANIC_CATCHES.contains(&(name.as_str(), inside.as_str())) {
                found.push(format!(
                    "{name}:{}: `catch_panic` in `{inside}`, outside its places",
                    line_of(&code, at)
                ));
            }
        }
    }
    for (file, in_fn) in COMPILE_CATCHES {
        let count = placed
            .iter()
            .filter(|(name, inside)| name == file && inside == in_fn)
            .count();
        if count != 1 {
            found.push(format!(
                "{file}: `{in_fn}` must hold one `catch_compile`; it holds {count}"
            ));
        }
    }
    for (file, inside) in &placed {
        if !COMPILE_CATCHES.contains(&(file.as_str(), inside.as_str())) {
            found.push(format!(
                "{file}: a `catch_compile` in `{inside}`, which is not a listed place"
            ));
        }
    }
    found.extend(compile_reach_findings(sources));
    found
}

/// Check the closure of the `catch_compile` call at byte `at` of blanked `code`: its
/// second argument is `AssertUnwindSafe(|| <one call>)`, the call one of
/// [`COMPILE_CALLS`], nothing chained onto it, and none of [`STATE_CHANGES`] in its
/// arguments.
fn catch_closure(code: &str, at: usize) -> Result<(), String> {
    let open = at + code[at..].find('(').ok_or("no argument list")?;
    let close = closing_paren(code, open).ok_or("an argument list that never closes")?;
    let args = top_level_parts(&code[open + 1..close]);
    let [_label, closure] = args.as_slice() else {
        return Err(format!(
            "takes {} arguments, not a label and a closure",
            args.len()
        ));
    };
    let closure: String = closure.split_whitespace().collect();
    let body = closure
        .strip_prefix("AssertUnwindSafe(")
        .or_else(|| closure.strip_prefix("std::panic::AssertUnwindSafe("))
        .and_then(|inner| inner.strip_suffix(')'))
        .and_then(|inner| {
            inner
                .strip_prefix("||")
                .or_else(|| inner.strip_prefix("move||"))
        })
        .ok_or_else(|| format!("its closure is not `AssertUnwindSafe(|| …)`: `{closure}`"))?;
    let body = body
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(body);
    let paren = body
        .find('(')
        .ok_or_else(|| format!("its closure calls nothing: `{body}`"))?;
    let callee = &body[..paren];
    if !callee
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    {
        return Err(format!("its closure wraps more than one call: `{body}`"));
    }
    if !COMPILE_CALLS.iter().any(|(call, _)| *call == callee) {
        return Err(format!(
            "its closure calls `{callee}`, which is not a compile"
        ));
    }
    let end = closing_paren(body, paren).ok_or("a call that never closes")?;
    if end + 1 != body.len() {
        return Err(format!("its closure does more than its one call: `{body}`"));
    }
    let call_args = &body[paren + 1..end];
    match STATE_CHANGES
        .iter()
        .find(|change| call_args.contains(**change))
    {
        Some(change) => Err(format!(
            "its call changes state: `{change}` in `{call_args}`"
        )),
        None => Ok(()),
    }
}

/// What is wrong with the compiles the catches call, in `sources`: each of mds-cli's
/// [`COMPILE_CALLS`] is defined once, in its file, and neither it nor any function of
/// mds-cli it reaches calls one of [`WRITES`].
///
/// A call is a name followed by `(`, a method call's included, unless its path starts
/// with one of [`OTHER_CRATES`]. It leads to every function of mds-cli outside the test
/// modules with that name — each one, when several types define it — so the walk sees
/// more than a compile runs, never less. It does not see into a macro, a function of
/// another crate (mds-core's among them), or a call through a function pointer or a
/// trait object.
fn compile_reach_findings(sources: &[(String, String)]) -> Vec<String> {
    let codes: Vec<(&str, String)> = sources
        .iter()
        .map(|(name, src)| (name.as_str(), blank(src, true)))
        .collect();
    // Every `fn` of mds-cli outside its test modules: (index into `codes`, name, body).
    let mut defs: Vec<(usize, String, RangeInclusive<usize>)> = Vec::new();
    for (index, (_, code)) in codes.iter().enumerate() {
        let tests = mod_body(code, "tests");
        for (name, body) in fn_bodies(code) {
            if !tests.as_ref().is_some_and(|t| t.contains(body.start())) {
                defs.push((index, name, body));
            }
        }
    }
    let mut found = Vec::new();
    for (callee, file) in COMPILE_CALLS {
        let Some(file) = file else { continue };
        let mut pending: Vec<usize> = (0..defs.len()).filter(|&i| defs[i].1 == *callee).collect();
        let places: Vec<&str> = pending.iter().map(|&i| codes[defs[i].0].0).collect();
        if places != [*file] {
            found.push(format!(
                "`{callee}` must be defined once, in {file}; it is defined in {places:?}"
            ));
        }
        // Each function is looked at once, so the walk ends after `defs.len()` steps.
        let mut seen = vec![false; defs.len()];
        while let Some(i) = pending.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            let (index, name, body) = &defs[i];
            let (file_name, code) = &codes[*index];
            let text = &code[body.clone()];
            for write in WRITES {
                if let Some(at) = calls_ending_with(text, write).first() {
                    found.push(format!(
                        "{file_name}:{}: `{callee}` reaches `{name}`, which calls `{write}`",
                        line_of(code, body.start() + at)
                    ));
                }
            }
            let called = called_names(text);
            pending.extend((0..defs.len()).filter(|&j| !seen[j] && called.contains(&defs[j].1)));
        }
    }
    found
}

/// Byte offsets in blanked `text` of the calls whose path ends with `path`: its last name,
/// then `(`, with the rest of `path` written just before that name.
fn calls_ending_with(text: &str, path: &str) -> Vec<usize> {
    let name = path.rsplit("::").next().unwrap_or(path);
    ident_positions(text, name)
        .into_iter()
        .filter(|&at| {
            text[at + name.len()..].trim_start().starts_with('(')
                && text[..at + name.len()].ends_with(path)
        })
        .collect()
}

/// The names blanked `text` calls — each a name followed by `(` — except a call whose path
/// starts with one of [`OTHER_CRATES`].
fn called_names(text: &str) -> std::collections::HashSet<String> {
    let b = text.as_bytes();
    let mut names = std::collections::HashSet::new();
    let mut i = 0usize;
    while i < b.len() {
        if !is_ident_byte(b[i]) || prev_is_ident(b, i) {
            i += 1;
            continue;
        }
        let end = i + b[i..].iter().take_while(|c| is_ident_byte(**c)).count();
        if text[end..].trim_start().starts_with('(') && !OTHER_CRATES.contains(&path_head(text, i))
        {
            names.insert(text[i..end].to_string());
        }
        i = end;
    }
    names
}

/// The first name of the path that ends with the name at byte `at` of `text`:
/// `std` for `std::fs::write`, the name itself when nothing precedes it.
fn path_head(text: &str, at: usize) -> &str {
    let mut start = at;
    while start >= 2 && &text[start - 2..start] == "::" {
        let segment_end = start - 2;
        let segment_start = text[..segment_end]
            .bytes()
            .rposition(|c| !is_ident_byte(c))
            .map_or(0, |p| p + 1);
        if segment_start == segment_end {
            break;
        }
        start = segment_start;
    }
    let end = start
        + text[start..]
            .bytes()
            .take_while(|c| is_ident_byte(*c))
            .count();
    &text[start..end]
}

/// Index of the `)` closing the `(` at `open` in `text`, blanked of literals.
fn closing_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.bytes().enumerate().skip(open) {
        match c {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` split at its top-level commas, each part trimmed, empty parts left out (a
/// trailing comma).
fn top_level_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in text.bytes().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

/// Where `src` runs a process without setting or removing `RUST_BACKTRACE` (see
/// [`every_run_sets_or_removes_rust_backtrace`]).
struct UnsetRuns {
    /// Every `.spawn(` / `.output(` / `.status(` call in code, and every call of a
    /// [`SPAWN_HELPERS`] helper.
    runs: usize,
    /// 1-based lines of the calls whose function names `RUST_BACKTRACE` in neither an
    /// `env(RUST_BACKTRACE, …)` nor an `env_remove(RUST_BACKTRACE)` call.
    stray: Vec<usize>,
}

fn runs_without_rust_backtrace(src: &str) -> UnsetRuns {
    let code = blank(src, true);
    let mut found = UnsetRuns {
        runs: 0,
        stray: Vec::new(),
    };
    let calls = [".spawn(", ".output(", ".status("]
        .into_iter()
        .chain(SPAWN_HELPERS.iter().copied());
    for call in calls {
        for (at, _) in code.match_indices(call) {
            found.runs += 1;
            let sets = innermost_fn(&code, at).is_some_and(|(_, body)| {
                let text: String = code[body].split_whitespace().collect();
                text.contains("env(RUST_BACKTRACE,") || text.contains("env_remove(RUST_BACKTRACE)")
            });
            if !sets {
                found.stray.push(line_of(&code, at));
            }
        }
    }
    found.stray.sort_unstable();
    found
}

/// What is wrong with the panic hook in `output` (the text of `src/output.rs`); empty
/// when nothing is (see [`the_hook_writes_one_constant_and_nothing_about_the_panic`]).
fn hook_findings(output: &str) -> Vec<String> {
    let code = blank(output, true);
    let with_literals = blank(output, false);
    let Some(body) = fn_body(&code, "on_panic") else {
        return vec!["output.rs defines no `fn on_panic`".to_string()];
    };
    let text = &code[body.clone()];
    let compact: String = text.split_whitespace().collect();
    let mut found = Vec::new();

    // The panic is recorded before anything that can wait: a hook blocked in its write
    // must not leave the run to exit without it.
    if !code[body.start() + 1..]
        .trim_start()
        .starts_with("OUTPUT_STATE.note_panicked();")
    {
        found.push(
            "the hook must record the panic first: its body starts \
             `OUTPUT_STATE.note_panicked();`"
                .to_string(),
        );
    }
    let writes = compact.matches("write_all(").count();
    if writes != 1 || !compact.contains("write_all(ICE_TEXT.as_bytes())") {
        found.push(format!(
            "the hook must make exactly one `write_all`, of `ICE_TEXT`; it makes {writes}"
        ));
    }
    for required in ["note_panicked()", "exit_after_panic()"] {
        if !compact.contains(required) {
            found.push(format!("the hook must call `{required}`"));
        }
    }
    for forbidden in [
        "print",
        "format",
        "write!",
        "writeln!",
        "ewrite",
        "miette",
        "payload",
        "location",
        "downcast",
        "to_string",
        "Display",
        "Debug",
        "unwrap",
        "expect",
        "panic!",
        "unreachable!",
        "assert",
        "dbg!",
    ] {
        if text.contains(forbidden) {
            found.push(format!("the hook names `{forbidden}`"));
        }
    }
    // `info`, the hook's `PanicHookInfo`, only on a line directly under one of the two
    // feature gates.
    let gates = [
        "#[cfg(feature = \"debug-panics\")]",
        "#[cfg(not(feature = \"debug-panics\"))]",
    ];
    for at in ident_positions(text, "info") {
        let at = body.start() + at;
        let gated = attributes_above(&with_literals, at)
            .first()
            .is_some_and(|above| gates.contains(above));
        if !gated {
            found.push(format!(
                "the hook names its `PanicHookInfo` outside a `debug-panics` gate, at line {}",
                line_of(&code, at)
            ));
        }
    }
    found
}

/// What is wrong with the trigger's gating across `sources`; empty when nothing is (see
/// [`the_trigger_is_compiled_only_into_debug_builds`]).
fn trigger_findings(sources: &[(String, String)]) -> Vec<String> {
    gate_findings(sources, &PANIC_TRIGGER)
}

/// A hook that only a debug build may hold: the module it lives in, the words that may
/// appear only inside that module — each looked for with string literals kept (`true`) or
/// blanked (`false`) — and its functions, each with a release build's stub that does
/// nothing.
struct DebugGate {
    module: &'static str,
    needles: &'static [(&'static str, bool)],
    fns: &'static [&'static str],
}

/// The panic trigger (#389): `MDS_TEST_PANIC` and `panic_any`, in `mod panic_trigger`.
const PANIC_TRIGGER: DebugGate = DebugGate {
    module: "panic_trigger",
    needles: &[("MDS_TEST_PANIC", true), ("panic_any", false)],
    fns: TRIGGER_FNS,
};

/// The pause between a rewrite's read and its replace (#160):
/// `MDS_TEST_PAUSE_BEFORE_REPLACE`, in `mod pause_trigger`.
const PAUSE_TRIGGER: DebugGate = DebugGate {
    module: "pause_trigger",
    needles: &[("MDS_TEST_PAUSE_BEFORE_REPLACE", true)],
    fns: &["pause_before_replace"],
};

/// What is wrong with `gate`'s gating across `sources`; empty when nothing is: its module
/// is one, under `#[cfg(debug_assertions)]`; its words appear in it alone; and each of its
/// functions is defined there once and once outside it as an empty stub under
/// `#[cfg(not(debug_assertions))]`.
fn gate_findings(sources: &[(String, String)], gate: &DebugGate) -> Vec<String> {
    let module_item = format!("mod {}", gate.module);
    let mut found = Vec::new();
    let mut modules = 0usize;
    let mut defined = vec![0usize; gate.fns.len()];
    let mut stubs = vec![0usize; gate.fns.len()];
    for (name, src) in sources {
        let code = blank(src, true);
        let with_literals = blank(src, false);
        let module = mod_body(&code, gate.module);
        if let Some(module) = &module {
            modules += 1;
            let item = with_literals[..*module.start()]
                .rfind(&module_item)
                .unwrap_or(0);
            if !attributes_above(&with_literals, item).contains(&"#[cfg(debug_assertions)]") {
                found.push(format!(
                    "{name}: `{module_item}` is not under `#[cfg(debug_assertions)]`"
                ));
            }
        }
        let inside = |at: usize| module.as_ref().is_some_and(|m| m.contains(&at));
        for &(needle, literals) in gate.needles {
            let view = if literals { &with_literals } else { &code };
            for (at, _) in view.match_indices(needle) {
                if !inside(at) {
                    found.push(format!(
                        "{name}:{}: `{needle}` outside `{module_item}`",
                        line_of(&code, at)
                    ));
                }
            }
        }
        // Each function: defined in the module, and a release build's stub outside it,
        // under `#[cfg(not(debug_assertions))]`, with an empty body.
        for (fn_name, body) in fn_bodies(&code) {
            let Some(index) = gate.fns.iter().position(|f| *f == fn_name) else {
                continue;
            };
            if inside(*body.start()) {
                defined[index] += 1;
                continue;
            }
            stubs[index] += 1;
            let item = with_literals[..*body.start()]
                .rfind(&format!("fn {fn_name}"))
                .unwrap_or(0);
            if !attributes_above(&with_literals, item).contains(&"#[cfg(not(debug_assertions))]") {
                found.push(format!(
                    "{name}: a `{fn_name}` outside the module is not under \
                     `#[cfg(not(debug_assertions))]`"
                ));
            }
            let inner = code[body.clone()]
                .trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
                .to_string();
            if !inner.is_empty() {
                found.push(format!(
                    "{name}: the release build's `{fn_name}` must do nothing; it does \
                     `{inner}`"
                ));
            }
        }
    }
    if modules != 1 {
        found.push(format!(
            "the crate must hold exactly one `{module_item}`; it holds {modules}"
        ));
    }
    for ((fn_name, defined), stubs) in gate.fns.iter().zip(defined).zip(stubs) {
        if defined != 1 || stubs != 1 {
            found.push(format!(
                "`{fn_name}` must be defined once in `{module_item}` and once as a \
                 release build's stub; it is defined {defined} and {stubs} times"
            ));
        }
    }
    found
}

/// The trigger's functions: `panic_on_request` in the dispatch, `panic_on_compile` in
/// `catch_compile`, `panic_in_handler` in `mds watch`'s file-event callback and Ctrl-C
/// handler.
const TRIGGER_FNS: &[&str] = &["panic_on_request", "panic_on_compile", "panic_in_handler"];

/// What is wrong with `debug-panics` in `manifest`; empty when nothing is (see
/// [`debug_panics_is_never_on_by_default`]): every line, comments left out, that names it
/// in quotes — a value, which is how a feature turns another on, whether the list sits on
/// one line or several and whatever key holds it (`default`, `features.default`) — and a
/// `[features]` table that does not declare it as `debug-panics = []`. The declaration's
/// key is bare, so it is no quoted mention; a key written in quotes is reported too.
fn feature_findings(manifest: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut in_features = false;
    let mut declared = false;
    for (index, line) in manifest.lines().enumerate() {
        let line = toml_code(line).trim();
        if ["\"debug-panics\"", "'debug-panics'"]
            .iter()
            .any(|quoted| line.contains(quoted))
        {
            found.push(format!(
                "Cargo.toml:{}: `{line}` names `debug-panics` as a value, which turns it on",
                index + 1
            ));
        }
        if line.starts_with('[') {
            in_features = line == "[features]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_features) else {
            continue;
        };
        if key.trim() == "debug-panics" {
            declared = true;
            let value = value.trim();
            if value != "[]" {
                found.push(format!("`debug-panics` must enable nothing; it is {value}"));
            }
        }
    }
    if !declared {
        found.push("Cargo.toml's [features] does not declare `debug-panics`".to_string());
    }
    found
}

/// `line` of a TOML file without its comment: the text before the first `#` that is not
/// inside a string — `"…"`, with `\` escapes, or `'…'`, as TOML writes one on a line.
fn toml_code(line: &str) -> &str {
    let mut open: Option<char> = None;
    let mut escaped = false;
    for (at, c) in line.char_indices() {
        match open {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(quote) if c == quote => open = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => open = Some(c),
            None if c == '#' => return &line[..at],
            None => {}
        }
    }
    line
}

/// `src` with comments — and, when `literals` is set, string and char literals — blanked
/// to spaces, byte for byte and newlines kept, so a byte offset names the same place in
/// every view of a file and a search of the blanked text finds code only.
fn blank(src: &str, literals: bool) -> String {
    fn blank_range(out: &mut [u8], from: usize, to: usize) {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(end) = literal_end(src, i) {
            if literals {
                blank_range(&mut out, i, end);
            }
            i = end;
        } else if b[i..].starts_with(b"//") {
            let end = src[i..].find('\n').map_or(b.len(), |rel| i + rel);
            blank_range(&mut out, i, end);
            i = end;
        } else if b[i..].starts_with(b"/*") {
            let start = i;
            let mut depth = 0usize;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            blank_range(&mut out, start, i.min(b.len()));
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking writes ASCII spaces over whole characters")
}

/// If a string, raw string or char literal starts at `i`, the index just past it.
fn literal_end(src: &str, i: usize) -> Option<usize> {
    let b = src.as_bytes();
    let starts_word = !prev_is_ident(b, i);
    let raw = match b[i] {
        b'r' if starts_word => Some(i + 1),
        b'b' if starts_word && b.get(i + 1) == Some(&b'r') => Some(i + 2),
        _ => None,
    };
    if let Some(open) = raw {
        let hashes = b[open..].iter().take_while(|c| **c == b'#').count();
        let quote = open + hashes;
        if b.get(quote) != Some(&b'"') {
            return None;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        return Some(
            src[quote + 1..]
                .find(&close)
                .map_or(b.len(), |rel| quote + 1 + rel + close.len()),
        );
    }
    match b[i] {
        b'"' => {
            let mut j = i + 1;
            while j < b.len() {
                match b[j] {
                    b'\\' => j += 2,
                    b'"' => return Some(j + 1),
                    _ => j += 1,
                }
            }
            Some(b.len())
        }
        b'\'' => {
            if b.get(i + 1) == Some(&b'\\') {
                // An escape: a newline, a quote, a hex or a braced codepoint.
                let close = src.get(i + 3..)?.find('\'')?;
                return Some(i + 3 + close + 1);
            }
            // One character and a closing quote; otherwise a lifetime or a label.
            let ch = src[i + 1..].chars().next()?;
            let after = i + 1 + ch.len_utf8();
            (b.get(after) == Some(&b'\'')).then_some(after + 1)
        }
        _ => None,
    }
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && is_ident_byte(b[i - 1])
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Byte offsets in `text` where the identifier `name` stands on its own.
fn ident_positions(text: &str, name: &str) -> Vec<usize> {
    let b = text.as_bytes();
    text.match_indices(name)
        .map(|(at, _)| at)
        .filter(|at| {
            !prev_is_ident(b, *at) && !b.get(at + name.len()).copied().is_some_and(is_ident_byte)
        })
        .collect()
}

/// The 1-based line of byte offset `at`.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Index of the `}` closing the `{` at `open` in blanked `code`.
fn closing_brace(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in code.bytes().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every `fn` with a body in blanked `code`: its name and its body's byte range, braces
/// included. The body opens at the first `{` outside the signature's parentheses and
/// brackets; a `;` there first means the `fn` has no body.
fn fn_bodies(code: &str) -> Vec<(String, RangeInclusive<usize>)> {
    let b = code.as_bytes();
    let mut found = Vec::new();
    for at in ident_positions(code, "fn") {
        let after = at + 2;
        let name_start = after + (code[after..].len() - code[after..].trim_start().len());
        let name_len = b[name_start..]
            .iter()
            .take_while(|c| is_ident_byte(**c))
            .count();
        if name_len == 0 {
            continue;
        }
        let name_end = name_start + name_len;
        let mut depth = 0usize;
        let mut open = None;
        for (i, c) in b.iter().enumerate().skip(name_end) {
            match c {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth = depth.saturating_sub(1),
                b'{' if depth == 0 => {
                    open = Some(i);
                    break;
                }
                b';' if depth == 0 => break,
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        if let Some(close) = closing_brace(code, open) {
            found.push((code[name_start..name_end].to_string(), open..=close));
        }
    }
    found
}

/// The body of the first `fn name` in blanked `code`.
fn fn_body(code: &str, name: &str) -> Option<RangeInclusive<usize>> {
    fn_bodies(code)
        .into_iter()
        .find(|(fn_name, _)| fn_name == name)
        .map(|(_, body)| body)
}

/// The innermost `fn` of blanked `code` whose body holds byte offset `at`.
fn innermost_fn(code: &str, at: usize) -> Option<(String, RangeInclusive<usize>)> {
    fn_bodies(code)
        .into_iter()
        .filter(|(_, body)| body.contains(&at))
        .min_by_key(|(_, body)| body.end() - body.start())
}

/// The body of `mod name { … }` in blanked `code`, braces included.
fn mod_body(code: &str, name: &str) -> Option<RangeInclusive<usize>> {
    let item = format!("mod {name}");
    let b = code.as_bytes();
    let at = code.match_indices(&item).map(|(at, _)| at).find(|at| {
        !prev_is_ident(b, *at) && !b.get(at + item.len()).copied().is_some_and(is_ident_byte)
    })?;
    let open = at + code[at..].find('{')?;
    closing_brace(code, open).map(|close| open..=close)
}

/// The attribute lines directly above the line holding byte offset `at` of
/// `with_literals` (a file blanked of comments only, so a doc comment reads as a blank
/// line and is skipped), trimmed, nearest first.
fn attributes_above(with_literals: &str, at: usize) -> Vec<&str> {
    let line_start = with_literals[..at].rfind('\n').map_or(0, |i| i + 1);
    with_literals[..line_start]
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take_while(|line| line.starts_with("#["))
        .collect()
}
