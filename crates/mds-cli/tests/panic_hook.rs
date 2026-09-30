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
//! for. The payload carries a sentinel word, a raw ESC and the absolute path of the
//! working directory — a fresh temporary directory in every test here — so a test can
//! tell that none of it reached stderr. A release build compiles the trigger out
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
    let mut cmd = mds_bin();
    cmd.args(["check", "ok.mds"])
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
    /// Controls: the line check finds a raw ESC; the backtrace is a real one, naming a
    /// frame of the CLI's own code.
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
                backtrace.contains("mds::") && backtrace.lines().count() > 2,
                "RUST_BACKTRACE={value}: a real backtrace names a frame of the CLI; it \
                 was:\n{backtrace}"
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
/// `.output(` / `.status(` call also calls `env(RUST_BACKTRACE, …)` or
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
}

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
/// `#[cfg(debug_assertions)]`; a release build's `panic_on_request` does nothing. The
/// payload the module builds carries the sentinel these tests look for.
///
/// Controls: the module without its `cfg`, a mention outside it, and a release stub that
/// does something are each reported.
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

/// Where `src` runs a process without setting or removing `RUST_BACKTRACE` (see
/// [`every_run_sets_or_removes_rust_backtrace`]).
struct UnsetRuns {
    /// Every `.spawn(` / `.output(` / `.status(` call in code.
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
    for call in [".spawn(", ".output(", ".status("] {
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
    let mut found = Vec::new();
    let mut modules = 0usize;
    for (name, src) in sources {
        let code = blank(src, true);
        let with_literals = blank(src, false);
        let module = mod_body(&code, "panic_trigger");
        if let Some(module) = &module {
            modules += 1;
            let item = with_literals[..*module.start()]
                .rfind("mod panic_trigger")
                .unwrap_or(0);
            if !attributes_above(&with_literals, item).contains(&"#[cfg(debug_assertions)]") {
                found.push(format!(
                    "{name}: `mod panic_trigger` is not under `#[cfg(debug_assertions)]`"
                ));
            }
        }
        let inside = |at: usize| module.as_ref().is_some_and(|m| m.contains(&at));
        for (needle, view) in [("MDS_TEST_PANIC", &with_literals), ("panic_any", &code)] {
            for (at, _) in view.match_indices(needle) {
                if !inside(at) {
                    found.push(format!(
                        "{name}:{}: `{needle}` outside `mod panic_trigger`",
                        line_of(&code, at)
                    ));
                }
            }
        }
        // The release build's stub: under `#[cfg(not(debug_assertions))]`, with an empty
        // body.
        for (fn_name, body) in fn_bodies(&code) {
            if fn_name != "panic_on_request" || inside(*body.start()) {
                continue;
            }
            let item = with_literals[..*body.start()]
                .rfind("fn panic_on_request")
                .unwrap_or(0);
            if !attributes_above(&with_literals, item).contains(&"#[cfg(not(debug_assertions))]") {
                found.push(format!(
                    "{name}: a `panic_on_request` outside the module is not under \
                     `#[cfg(not(debug_assertions))]`"
                ));
            }
            let inner = code[body.clone()]
                .trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
                .to_string();
            if !inner.is_empty() {
                found.push(format!(
                    "{name}: the release build's `panic_on_request` must do nothing; it does \
                     `{inner}`"
                ));
            }
        }
    }
    if modules != 1 {
        found.push(format!(
            "the crate must hold exactly one `mod panic_trigger`; it holds {modules}"
        ));
    }
    found
}

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
