//! A closed output stream never changes an `mds` exit code (#157).
//!
//! # The vector
//!
//! A pipe whose read end is already gone: [`closed_pipe`] drops the reader BEFORE the
//! child is spawned, so no process holds it and the child's first write to that stream
//! fails deterministically with a broken pipe (EPIPE on unix, the pipe-is-closing
//! error on Windows). The two tempting alternatives do not test this:
//!
//! - Dropping `child.stderr` / `child.stdout` AFTER spawn races the child's writes — a
//!   short run can finish before the drop.
//! - Closing the descriptor outright (`2>&-`) never reaches the writer at all: the Rust
//!   runtime reopens a closed standard descriptor as `/dev/null` at startup.
//!
//! # What each row proves
//!
//! Every row closes one stream, stdout or stderr, and runs the same command twice, in
//! fresh directories:
//!
//! 1. **Open stream (positive control).** It must exit with the row's verdict AND print
//!    the row's expected text on the stream the row closes. That proves the command
//!    really writes to the stream the closed run loses — without it, a command that
//!    printed nothing would pass the closed run vacuously.
//! 2. **Closed stream.** It must exit with the SAME verdict and leave the other stream
//!    and every output file exactly as the open run did.
//!
//! The assertion is the exit code (`status.code() == Some(verdict)`). With stderr
//! closed a panic message has nowhere to go, so an absence-of-`panicked` check would
//! pass whether or not the command panicked; a panic shows up here as `Some(101)`.
//! With stdout closed, stderr stays open and must match the open run's byte for byte,
//! so a panic or an error report about the closed pipe fails the row as well.

mod common;
use common::mds_bin;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Failure bound for one `mds` run. Every row finishes in milliseconds; this only stops
/// a hung child from hanging the suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// How often [`wait_bounded`] polls the child for exit.
const POLL: Duration = Duration::from_millis(5);

/// The write end of a pipe whose read end has already been dropped.
///
/// Handing this to a child as one of its standard streams makes every write the child
/// makes to that stream fail with a broken pipe from the first byte on.
fn closed_pipe() -> std::io::PipeWriter {
    let (reader, writer) = std::io::pipe().expect("create a pipe");
    drop(reader);
    writer
}

/// One of the child's output streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn other(self) -> Self {
        match self {
            Self::Stdout => Self::Stderr,
            Self::Stderr => Self::Stdout,
        }
    }
}

/// What one run left behind.
struct Run {
    code: Option<i32>,
    /// Empty when stdout was closed.
    stdout: String,
    /// Empty when stderr was closed.
    stderr: String,
}

impl Run {
    fn text(&self, stream: Stream) -> &str {
        match stream {
            Stream::Stdout => &self.stdout,
            Stream::Stderr => &self.stderr,
        }
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
fn wait_bounded(child: &mut Child, what: &str) -> Option<i32> {
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("poll the mds child") {
            return status.code();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`mds {what}` did not exit within {RUN_TIMEOUT:?}");
        }
        std::thread::sleep(POLL);
    }
}

/// Run `mds <args>` in `dir`, feeding `stdin`, with the stream `closed` (if any) a closed
/// pipe from the start.
fn run(dir: &Path, args: &[&str], stdin: &str, closed: Option<Stream>) -> Run {
    let stdout = match closed {
        Some(Stream::Stdout) => Stdio::from(closed_pipe()),
        _ => Stdio::piped(),
    };
    let stderr = match closed {
        Some(Stream::Stderr) => Stdio::from(closed_pipe()),
        _ => Stdio::piped(),
    };
    run_with(dir, args, stdin, stdout, stderr)
}

/// Run `mds <args>` in `dir`, feeding `stdin`, with the given stdout and stderr.
fn run_with(dir: &Path, args: &[&str], stdin: &str, stdout: Stdio, stderr: Stdio) -> Run {
    let what = args.join(" ");
    let mut cmd = mds_bin();
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr);
    let mut child = cmd.spawn().expect("spawn mds");

    // A child that exits without reading stdin closes it first; on Linux that makes
    // this write fail with a broken pipe, which says nothing about the child.
    let mut child_stdin = child.stdin.take().expect("stdin is piped");
    let input = stdin.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = child_stdin.write_all(&input);
    });
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let code = wait_bounded(&mut child, &what);
    writer.join().expect("stdin writer thread");
    Run {
        code,
        stdout: joined(out),
        stderr: joined(err),
    }
}

/// A fresh working directory holding every fixture a row may name.
///
/// - `ok.mds` compiles; `bad.mds` does not (an unterminated `@if`).
/// - `d/` holds one of each; `good/` holds two that compile, so a run that stops after
///   the first output shows up as a missing second one.
/// - `messy.mds` and `m/messy.mds` lack the final newline `mds fmt` adds.
fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let files: [(&str, &str); 7] = [
        ("ok.mds", "Hello\n"),
        ("bad.mds", "@if flag:\nunterminated\n"),
        ("d/ok.mds", "Hello\n"),
        ("d/bad.mds", "@if flag:\nunterminated\n"),
        ("good/a.mds", "A\n"),
        ("good/b.mds", "B\n"),
        ("messy.mds", "Hello"),
    ];
    for dir_name in ["d", "good", "m"] {
        std::fs::create_dir(root.join(dir_name)).expect("create a fixture dir");
    }
    for (name, text) in files {
        std::fs::write(root.join(name), text).expect("write a fixture file");
    }
    std::fs::write(root.join("m/messy.mds"), "Hello").expect("write m/messy.mds");
    dir
}

/// Every file under `dir` with its bytes, sorted — what a run left on disk.
fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    // Bounded: the fixture tree is two levels deep and holds a handful of files.
    for _ in 0..64 {
        let Some(d) = stack.pop() else { break };
        for entry in std::fs::read_dir(&d).expect("read fixture dir").flatten() {
            let path = entry.path();
            let file_type = entry.file_type().expect("file type");
            if file_type.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("read fixture file");
                let rel = path.strip_prefix(dir).expect("under dir").to_path_buf();
                out.push((rel, bytes));
            }
        }
    }
    assert!(stack.is_empty(), "fixture tree deeper than the walk bound");
    out.sort();
    out
}

/// One command, its verdict with open streams, the stream it closes, and what the open
/// run must print on that stream.
struct Row {
    args: &'static [&'static str],
    stdin: &'static str,
    verdict: i32,
    closed: Stream,
    open_contains: &'static str,
}

/// Run `row` with every stream open and then with `row.closed` closed, asserting both
/// arms (see the module doc).
///
/// Returns the file listing both runs left, for rows that also check what was written.
fn assert_closing_keeps_the_verdict(row: &Row) -> Vec<(PathBuf, Vec<u8>)> {
    let what = row.args.join(" ");
    let closed_name = format!("{:?}", row.closed).to_lowercase();
    let kept = row.closed.other();

    let open_dir = fixture_dir();
    let open = run(open_dir.path(), row.args, row.stdin, None);
    assert_eq!(
        open.code,
        Some(row.verdict),
        "control: `mds {what}` with open streams must exit with its verdict; stderr: {:?}",
        open.stderr
    );
    assert!(
        open.text(row.closed).contains(row.open_contains),
        "control: `mds {what}` must print {:?} on {closed_name}, or the closed-{closed_name} \
         run would prove nothing; {closed_name}: {:?}",
        row.open_contains,
        open.text(row.closed)
    );

    let closed_dir = fixture_dir();
    let closed = run(closed_dir.path(), row.args, row.stdin, Some(row.closed));
    assert_eq!(
        closed.code,
        Some(row.verdict),
        "`mds {what}` with {closed_name} closed must exit with the verdict it has with open \
         streams ({}), not a panic (101) or a signal (None); open-run stderr: {:?}; \
         closed-run stdout: {:?}; closed-run stderr: {:?}",
        row.verdict,
        open.stderr,
        closed.stdout,
        closed.stderr
    );
    assert_eq!(
        closed.text(kept),
        open.text(kept),
        "`mds {what}`: closing {closed_name} must not change {:?}",
        kept
    );

    let open_files = files_under(open_dir.path());
    let closed_files = files_under(closed_dir.path());
    assert_eq!(
        closed_files, open_files,
        "`mds {what}`: closing {closed_name} must not change what is written to disk"
    );
    closed_files
}

/// The bytes a run left at `path` (relative to its working directory), if any.
fn written<'a>(files: &'a [(PathBuf, Vec<u8>)], path: &str) -> Option<&'a [u8]> {
    files
        .iter()
        .find(|(p, _)| p == Path::new(path))
        .map(|(_, bytes)| bytes.as_slice())
}

// ── check, init, lint ────────────────────────────────────────────────────────

#[test]
fn check_a_file_with_stderr_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["check", "ok.mds"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "OK: ok.mds\n",
    });
}

#[test]
fn check_a_broken_file_with_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["check", "bad.mds"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "mds::syntax",
    });
}

#[test]
fn check_stdin_with_stderr_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["check", "-"],
        stdin: "Hello\n",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "OK: <stdin>\n",
    });
}

#[test]
fn check_a_directory_with_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["check", "d"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "1 passed, 1 failed\n",
    });
}

#[test]
fn init_with_stderr_closed_exits_0_and_writes_the_file() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["init"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "Created hello.mds",
    });
    let starter = written(&files, "hello.mds").map(String::from_utf8_lossy);
    assert!(
        starter
            .as_deref()
            .is_some_and(|s| s.contains("Hello {{name}}!")),
        "`mds init` with stderr closed must still write the starter file; got {starter:?}"
    );
}

#[test]
fn lint_with_an_unknown_format_and_stderr_closed_exits_2() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--format", "yaml", "ok.mds"],
        stdin: "",
        verdict: 2,
        closed: Stream::Stderr,
        open_contains: "unknown --format value",
    });
}

// ── build ────────────────────────────────────────────────────────────────────

#[test]
fn build_to_stdout_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["build", "ok.mds", "-o", "-"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "Hello\n",
    });
}

#[test]
fn build_stdin_to_stdout_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["build", "-"],
        stdin: "Hello\n",
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "Hello\n",
    });
}

#[test]
fn build_a_file_with_stderr_closed_exits_0_and_writes_the_output() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["build", "ok.mds"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "Compiled to ./ok.md\n",
    });
    assert_eq!(
        written(&files, "ok.md"),
        Some(&b"Hello\n"[..]),
        "`mds build ok.mds` with stderr closed must still write ok.md"
    );
}

#[test]
fn build_with_a_source_map_and_stderr_closed_exits_0_and_writes_both_files() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["build", "ok.mds", "--source-map"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "Source map written to ./ok.md.map\n",
    });
    assert!(
        written(&files, "ok.md").is_some() && written(&files, "ok.md.map").is_some(),
        "`mds build ok.mds --source-map` with stderr closed must still write the output \
         and its map; files: {:?}",
        files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
}

#[test]
fn build_a_directory_with_stderr_closed_exits_0_and_writes_every_output() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["build", "good"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "2 built, 0 failed\n",
    });
    assert_eq!(
        (written(&files, "good/a.md"), written(&files, "good/b.md")),
        (Some(&b"A\n"[..]), Some(&b"B\n"[..])),
        "`mds build good` with stderr closed must still write both outputs"
    );
}

#[test]
fn build_a_directory_with_a_broken_file_and_stderr_closed_exits_1() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["build", "d"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "1 built, 1 failed\n",
    });
    assert_eq!(
        written(&files, "d/ok.md"),
        Some(&b"Hello\n"[..]),
        "`mds build d` with stderr closed must still write the file that compiles"
    );
}

// ── fmt ──────────────────────────────────────────────────────────────────────

#[test]
fn fmt_a_file_with_stderr_closed_exits_0_and_rewrites_it() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["fmt", "messy.mds"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "Formatted: messy.mds\n",
    });
    assert_eq!(
        written(&files, "messy.mds"),
        Some(&b"Hello\n"[..]),
        "`mds fmt messy.mds` with stderr closed must still rewrite the file"
    );
}

#[test]
fn fmt_check_with_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["fmt", "--check", "messy.mds"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "Would reformat: messy.mds\n",
    });
}

#[test]
fn fmt_stdin_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["fmt", "-"],
        stdin: "Hello",
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "Hello\n",
    });
}

#[test]
fn fmt_diff_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["fmt", "--diff", "messy.mds"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "+++ messy.mds\n",
    });
}

#[test]
fn fmt_check_diff_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["fmt", "--check", "--diff", "messy.mds"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "+++ messy.mds\n",
    });
}

#[test]
fn fmt_check_diff_on_a_directory_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["fmt", "--check", "--diff", "m"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "messy.mds\n",
    });
}

// ── A stdout that fails for another reason (Linux: /dev/full) ────────────────

/// Run `mds <args>` with stdout on `/dev/full`, where every write fails with "no space
/// left on device" — a failure that is not a closed pipe, so it is reported as
/// `mds::io` and the run exits at least 2 (#157). The directory handle, the other
/// vector a unix host has, is no use for stdout: the Rust runtime swallows the EBADF a
/// write to a read-only standard stream gets.
fn run_into_dev_full(args: &[&str], stdin: &str) -> Run {
    let dir = fixture_dir();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    run_with(dir.path(), args, stdin, Stdio::from(full), Stdio::piped())
}

fn assert_stdout_failure_exits_2(args: &[&str], stdin: &str) {
    let what = args.join(" ");
    let run = run_into_dev_full(args, stdin);
    assert_eq!(
        run.code,
        Some(2),
        "`mds {what}` into /dev/full must exit 2; stderr: {:?}",
        run.stderr
    );
    assert!(
        run.stderr.contains("mds::io") && run.stderr.contains("cannot write to stdout"),
        "`mds {what}` into /dev/full must report one mds::io error naming stdout; \
         stderr: {:?}",
        run.stderr
    );
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "/dev/full exists only on Linux")]
fn build_to_stdout_into_a_full_device_exits_2() {
    assert_stdout_failure_exits_2(&["build", "ok.mds", "-o", "-"], "");
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "/dev/full exists only on Linux")]
fn fmt_stdin_into_a_full_device_exits_2() {
    assert_stdout_failure_exits_2(&["fmt", "-"], "Hello");
}
