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
//! Every row runs the same command twice, in fresh directories:
//!
//! 1. **Open stream (positive control).** It must exit with the row's verdict AND print
//!    the row's expected stderr text. That proves the command really writes to the
//!    stream the closed run loses — without it, a command that printed nothing would
//!    pass the closed run vacuously.
//! 2. **Closed stream.** It must exit with the SAME verdict and leave stdout and every
//!    output file exactly as the open run did.
//!
//! The assertion is the exit code (`status.code() == Some(verdict)`). With stderr
//! closed a panic message has nowhere to go, so an absence-of-`panicked` check would
//! pass whether or not the command panicked; a panic shows up here as `Some(101)`.

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

/// Which state the child's stderr starts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stderr {
    Open,
    Closed,
}

/// What one run left behind.
struct Run {
    code: Option<i32>,
    stdout: String,
    /// Empty when stderr was closed.
    stderr: String,
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

/// Run `mds <args>` in `dir`, feeding `stdin` and starting stderr in state `stderr`.
fn run(dir: &Path, args: &[&str], stdin: &str, stderr: Stderr) -> Run {
    let what = args.join(" ");
    let mut cmd = mds_bin();
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    match stderr {
        Stderr::Open => cmd.stderr(Stdio::piped()),
        Stderr::Closed => cmd.stderr(Stdio::from(closed_pipe())),
    };
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
fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("ok.mds"), "Hello\n").expect("write ok.mds");
    std::fs::write(root.join("bad.mds"), "@if flag:\nunterminated\n").expect("write bad.mds");
    std::fs::create_dir(root.join("d")).expect("create d");
    std::fs::write(root.join("d/ok.mds"), "Hello\n").expect("write d/ok.mds");
    std::fs::write(root.join("d/bad.mds"), "@if flag:\nunterminated\n").expect("write d/bad.mds");
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

/// One command, its verdict with open streams, and what its open stderr must contain.
struct Row {
    args: &'static [&'static str],
    stdin: &'static str,
    verdict: i32,
    open_stderr_contains: &'static str,
}

/// Run `row` with stderr open and then closed, asserting both arms (see the module doc).
///
/// Returns the file listing both runs left, for rows that also check what was written.
fn assert_closed_stderr_keeps_the_verdict(row: &Row) -> Vec<(PathBuf, Vec<u8>)> {
    let what = row.args.join(" ");

    let open_dir = fixture_dir();
    let open = run(open_dir.path(), row.args, row.stdin, Stderr::Open);
    assert_eq!(
        open.code,
        Some(row.verdict),
        "control: `mds {what}` with stderr open must exit with its verdict; stderr: {:?}",
        open.stderr
    );
    assert!(
        open.stderr.contains(row.open_stderr_contains),
        "control: `mds {what}` with stderr open must print {:?}, or the closed-stderr run \
         would prove nothing; stderr: {:?}",
        row.open_stderr_contains,
        open.stderr
    );

    let closed_dir = fixture_dir();
    let closed = run(closed_dir.path(), row.args, row.stdin, Stderr::Closed);
    assert_eq!(
        closed.code,
        Some(row.verdict),
        "`mds {what}` with stderr closed must exit with the verdict it has with stderr \
         open ({}), not a panic (101) or a signal (None); open-run stderr: {:?}; \
         closed-run stdout: {:?}",
        row.verdict,
        open.stderr,
        closed.stdout
    );
    assert_eq!(
        closed.stdout, open.stdout,
        "`mds {what}`: closing stderr must not change stdout"
    );

    let open_files = files_under(open_dir.path());
    let closed_files = files_under(closed_dir.path());
    assert_eq!(
        closed_files, open_files,
        "`mds {what}`: closing stderr must not change what is written to disk"
    );
    closed_files
}

#[test]
fn check_a_file_with_stderr_closed_exits_0() {
    assert_closed_stderr_keeps_the_verdict(&Row {
        args: &["check", "ok.mds"],
        stdin: "",
        verdict: 0,
        open_stderr_contains: "OK: ok.mds\n",
    });
}

#[test]
fn check_a_broken_file_with_stderr_closed_exits_1() {
    assert_closed_stderr_keeps_the_verdict(&Row {
        args: &["check", "bad.mds"],
        stdin: "",
        verdict: 1,
        open_stderr_contains: "mds::syntax",
    });
}

#[test]
fn check_stdin_with_stderr_closed_exits_0() {
    assert_closed_stderr_keeps_the_verdict(&Row {
        args: &["check", "-"],
        stdin: "Hello\n",
        verdict: 0,
        open_stderr_contains: "OK: <stdin>\n",
    });
}

#[test]
fn check_a_directory_with_stderr_closed_exits_1() {
    assert_closed_stderr_keeps_the_verdict(&Row {
        args: &["check", "d"],
        stdin: "",
        verdict: 1,
        open_stderr_contains: "1 passed, 1 failed\n",
    });
}

#[test]
fn init_with_stderr_closed_exits_0_and_writes_the_file() {
    let files = assert_closed_stderr_keeps_the_verdict(&Row {
        args: &["init"],
        stdin: "",
        verdict: 0,
        open_stderr_contains: "Created hello.mds",
    });
    let starter = files
        .iter()
        .find(|(path, _)| path == Path::new("hello.mds"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned());
    assert!(
        starter
            .as_deref()
            .is_some_and(|s| s.contains("Hello {{name}}!")),
        "`mds init` with stderr closed must still write the starter file; got {starter:?}"
    );
}

#[test]
fn lint_with_an_unknown_format_and_stderr_closed_exits_2() {
    assert_closed_stderr_keeps_the_verdict(&Row {
        args: &["lint", "--format", "yaml", "ok.mds"],
        stdin: "",
        verdict: 2,
        open_stderr_contains: "unknown --format value",
    });
}
