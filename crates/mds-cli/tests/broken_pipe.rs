//! How a stream or I/O failure sets an `mds` exit code (#157):
//!
//! - A closed stdout or stderr never changes the exit code.
//! - Any other I/O failure — a stdout that fails for another reason, an output file or
//!   directory that cannot be written, stdin that cannot be read or is not UTF-8 — is
//!   one `mds::io` error, and the run exits at least 2.
//! - Stdin over the 10 MiB cap is `mds::resource_limit`, exit 3; exactly the cap passes.
//!
//! # The closed-stream vector
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
//!
//! # The other failures
//!
//! - An output that cannot be written: a directory at mode `0o555` (unix). Root ignores
//!   the mode, so each such test first checks that it really cannot create a file there
//!   and skips with a printed reason when it can. A source that cannot be read, in
//!   directory mode: a file at mode `0o000` (unix), skipped the same way.
//! - Stdin that cannot be read: a directory handle on unix (reading it fails with
//!   "is a directory"), a write-only file handle on Windows. A write-only handle cannot
//!   stand in on unix: the Rust runtime reads EBADF on a standard stream as end of input.
//! - A stdout that fails for another reason: `/dev/full` on Linux ("no space left on
//!   device"), and on every unix a regular file the child may not grow — its file-size
//!   limit is 0 and it ignores SIGXFSZ, so each write fails with "file too large".

mod common;
use common::{closed_pipe, mds_bin};

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Failure bound for one `mds` run. Every row finishes in milliseconds; this only stops
/// a hung child from hanging the suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// How often [`wait_bounded`] polls the child for exit.
const POLL: Duration = Duration::from_millis(5);

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

/// What the child reads on stdin.
enum Input {
    /// These bytes, written into a pipe.
    Bytes(Vec<u8>),
    /// This handle, as it is.
    Handle(Stdio),
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
    run_with(
        dir,
        args,
        Input::Bytes(stdin.as_bytes().to_vec()),
        stdout,
        stderr,
    )
}

/// Run `mds <args>` in `dir` with the given stdin, stdout and stderr.
fn run_with(dir: &Path, args: &[&str], stdin: Input, stdout: Stdio, stderr: Stdio) -> Run {
    run_command(mds_bin(), dir, args, stdin, stdout, stderr)
}

/// [`run_with`] on an `mds` command the caller has already prepared.
fn run_command(
    mut cmd: Command,
    dir: &Path,
    args: &[&str],
    stdin: Input,
    stdout: Stdio,
    stderr: Stdio,
) -> Run {
    let what = args.join(" ");
    cmd.args(args)
        .current_dir(dir)
        .stdout(stdout)
        .stderr(stderr);
    let bytes = match stdin {
        Input::Bytes(bytes) => {
            cmd.stdin(Stdio::piped());
            Some(bytes)
        }
        Input::Handle(handle) => {
            cmd.stdin(handle);
            None
        }
    };
    let mut child = cmd.spawn().expect("spawn mds");

    // A child that exits without reading stdin closes it first; on Linux that makes
    // this write fail with a broken pipe, which says nothing about the child.
    let writer = bytes.map(|bytes| {
        let mut child_stdin = child.stdin.take().expect("stdin is piped");
        std::thread::spawn(move || {
            let _ = child_stdin.write_all(&bytes);
        })
    });
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let code = wait_bounded(&mut child, &what);
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    Run {
        code,
        stdout: joined(out),
        stderr: joined(err),
    }
}

/// A template `mds lint` warns about (an unused frontmatter key), exit 1.
const LINT_WARN_SOURCE: &str =
    "---\ngreeting: Hello\nunused_key: never referenced in the body\n---\n\n{{greeting}}, world!\n";

/// A template `mds lint --fix` fixes to `Hello\n` (a branch that can never be taken).
const LINT_FIXABLE_SOURCE: &str = "@if \"x\" == \"y\":\nhidden\n@end\nHello\n";

/// A fresh working directory holding every fixture a row may name.
///
/// - `ok.mds` compiles, and `mds lint` finds nothing in it; `bad.mds` does not compile
///   (an unterminated `@if`).
/// - `d/` holds one of each; `good/` holds two that compile, so a run that stops after
///   the first output shows up as a missing second one.
/// - `messy.mds` and `m/messy.mds` lack the final newline `mds fmt` adds; so do both
///   files in `two/`, so `mds fmt --diff two` writes two diffs.
/// - `warn.mds` and `lints/` (a clean file and a warning one) lint with warnings, exit 1;
///   `fix.mds` and both files in `fixes/` hold a fix, so `mds lint --fix --diff fixes`
///   writes two diffs.
fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let files: [(&str, &str); 15] = [
        ("ok.mds", "Hello\n"),
        ("bad.mds", "@if flag:\nunterminated\n"),
        ("d/ok.mds", "Hello\n"),
        ("d/bad.mds", "@if flag:\nunterminated\n"),
        ("good/a.mds", "A\n"),
        ("good/b.mds", "B\n"),
        ("messy.mds", "Hello"),
        ("two/a.mds", "A"),
        ("two/b.mds", "B"),
        ("warn.mds", LINT_WARN_SOURCE),
        ("fix.mds", LINT_FIXABLE_SOURCE),
        ("lints/ok.mds", "Hello\n"),
        ("lints/warn.mds", LINT_WARN_SOURCE),
        ("fixes/a.mds", LINT_FIXABLE_SOURCE),
        ("fixes/b.mds", LINT_FIXABLE_SOURCE),
    ];
    for dir_name in ["d", "good", "m", "two", "lints", "fixes"] {
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

// ── lint ─────────────────────────────────────────────────────────────────────

#[test]
fn lint_a_file_with_warnings_and_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "warn.mds"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "[unused-variable]",
    });
}

#[test]
fn lint_a_clean_file_with_stderr_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "ok.mds"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "Clean: ok.mds\n",
    });
}

#[test]
fn lint_a_directory_with_warnings_and_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "lints"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "1 clean, 1 with warnings, 0 with errors, 0 resource-limited\n",
    });
}

#[test]
fn lint_stdin_with_warnings_and_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "-"],
        stdin: LINT_WARN_SOURCE,
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "[unused-variable]",
    });
}

#[test]
fn lint_fix_check_of_stdin_with_stderr_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--fix", "--check", "-"],
        stdin: LINT_FIXABLE_SOURCE,
        verdict: 1,
        closed: Stream::Stderr,
        open_contains: "Would fix: <stdin>\n",
    });
}

#[test]
fn lint_fix_of_a_file_with_stderr_closed_exits_0_and_rewrites_it() {
    let files = assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--fix", "fix.mds"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stderr,
        open_contains: "Fixed: fix.mds\n",
    });
    assert_eq!(
        written(&files, "fix.mds"),
        Some(&b"Hello\n"[..]),
        "`mds lint --fix fix.mds` with stderr closed must still rewrite the file"
    );
}

#[test]
fn lint_json_on_a_file_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--format", "json", "warn.mds"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "\"file\":\"warn.mds\"",
    });
}

#[test]
fn lint_json_on_a_directory_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--format", "json", "lints"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "\"file\":\"warn.mds\"",
    });
}

#[test]
fn lint_json_on_stdin_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--format", "json", "-"],
        stdin: LINT_WARN_SOURCE,
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "\"file\":\"<stdin>\"",
    });
}

#[test]
fn lint_fix_of_stdin_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--fix", "-"],
        stdin: LINT_FIXABLE_SOURCE,
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "Hello\n",
    });
}

#[test]
fn lint_fix_diff_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--fix", "--diff", "fix.mds"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "+++ fix.mds\n",
    });
}

/// Two diffs into a closed stdout: neither file is counted as failed — stderr, the
/// summary line included, is the open run's byte for byte (#157).
#[test]
fn lint_fix_diff_of_a_directory_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--fix", "--diff", "fixes"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "+++ fixes/b.mds\n",
    });
}

/// The `--format json` directory emitter is a separate one; the same holds for it.
#[test]
fn lint_json_fix_diff_of_a_directory_with_stdout_closed_exits_1() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["lint", "--format", "json", "--fix", "--diff", "fixes"],
        stdin: "",
        verdict: 1,
        closed: Stream::Stdout,
        open_contains: "+++ fixes/b.mds\n",
    });
}

// ── A stdout that fails for another reason than a closed pipe ────────────────

/// Run `mds <args>` in a fresh fixture dir with stdout on `/dev/full`, where every write
/// fails with "no space left on device" — a failure that is not a closed pipe, so it is
/// reported as `mds::io` and the run exits at least 2 (#157).
fn run_into_dev_full(args: &[&str], stdin: &str) -> Run {
    let dir = fixture_dir();
    run_into_dev_full_in(dir.path(), args, stdin)
}

/// [`run_into_dev_full`] in `dir`.
fn run_into_dev_full_in(dir: &Path, args: &[&str], stdin: &str) -> Run {
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    run_with(
        dir,
        args,
        Input::Bytes(stdin.as_bytes().to_vec()),
        Stdio::from(full),
        Stdio::piped(),
    )
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

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "/dev/full exists only on Linux")]
fn help_into_a_full_device_exits_2() {
    assert_stdout_failure_exits_2(&["--help"], "");
}

/// Run `mds <args>` in `dir` with stdout on a regular file the child may not grow: its
/// file-size limit is 0 and it ignores SIGXFSZ, so every write to stdout fails with
/// "file too large" (EFBIG) — a failure that is not a closed pipe, on every unix, where
/// `/dev/full` is Linux only. Stderr stays a pipe, which the limit does not cover (#157).
#[cfg(unix)]
fn run_into_a_file_it_may_not_grow_in(dir: &Path, args: &[&str], stdin: &str) -> Run {
    use std::os::unix::process::CommandExt as _;

    let out = tempfile::tempfile().expect("create the stdout file");
    let mut cmd = mds_bin();
    // SAFETY: the closure runs in the forked child just before `exec`, where only
    // async-signal-safe work is sound: `signal` is on POSIX's async-signal-safe list, and
    // `setrlimit` is a thin wrapper around its system call that takes no lock and
    // allocates nothing. The closure touches none of the parent's state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            let no_growth = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &no_growth) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    run_command(
        cmd,
        dir,
        args,
        Input::Bytes(stdin.as_bytes().to_vec()),
        Stdio::from(out),
        Stdio::piped(),
    )
}

/// Run `mds <args>` in `dir`, feeding `stdin`, with stdout failing for another reason
/// than a closed pipe.
type FailingStdout = fn(&Path, &[&str], &str) -> Run;

/// Every way this platform has to fail stdout for another reason than a closed pipe, by
/// name: `/dev/full` on Linux and a file the child may not grow on every unix; none
/// elsewhere, where the rows that use them are ignored.
fn failing_stdouts() -> Vec<(&'static str, FailingStdout)> {
    [
        #[cfg(target_os = "linux")]
        ("/dev/full", run_into_dev_full_in as FailingStdout),
        #[cfg(unix)]
        (
            "a file it may not grow",
            run_into_a_file_it_may_not_grow_in as FailingStdout,
        ),
    ]
    .into_iter()
    .collect()
}

/// An `mds` run whose stdout fails for another reason than a closed pipe.
struct FullRow {
    args: &'static [&'static str],
    stdin: &'static str,
    /// The exit code with stdout on an open pipe.
    verdict: i32,
    /// What the open run prints on stdout: the writes a failing stdout loses.
    open_contains: &'static [&'static str],
    /// Run on each fresh fixture dir before `mds` is.
    prepare: fn(&Path),
}

/// Run `row` into an open pipe, then with stdout failing in each of the platform's
/// [`failing_stdouts`] (#157).
///
/// The open run is the control: it must exit with the row's verdict, report nothing
/// about stdout, and print every `open_contains` text there — so a failing stdout really
/// loses those writes. Each failing run must exit `max(verdict, 2)` and report the
/// failure exactly once, as one `mds::io` error naming stdout, however many writes it
/// lost. Returns the open run and the failing runs.
fn assert_a_failing_stdout_lifts_the_verdict(row: &FullRow) -> (Run, Vec<Run>) {
    let what = row.args.join(" ");
    let ways = failing_stdouts();
    assert!(
        !ways.is_empty(),
        "no way to fail stdout on this platform: ignore the row here"
    );

    let open_dir = fixture_dir();
    (row.prepare)(open_dir.path());
    let open = run(open_dir.path(), row.args, row.stdin, None);
    assert_eq!(
        open.code,
        Some(row.verdict),
        "control: `mds {what}` into an open pipe must exit with its verdict; stderr: {:?}",
        open.stderr
    );
    assert!(
        !open.stderr.contains("cannot write to stdout"),
        "control: `mds {what}` into an open pipe must not report stdout; stderr: {:?}",
        open.stderr
    );
    for text in row.open_contains {
        assert!(
            open.stdout.contains(text),
            "control: `mds {what}` must print {text:?} on stdout, or a failing stdout would \
             lose nothing; stdout: {:?}",
            open.stdout
        );
    }

    let want = row.verdict.max(2);
    let mut failing = Vec::with_capacity(ways.len());
    for (how, run_failing) in ways {
        let dir = fixture_dir();
        (row.prepare)(dir.path());
        let run = run_failing(dir.path(), row.args, row.stdin);
        assert_eq!(
            run.code,
            Some(want),
            "`mds {what}` into {how} must exit {want}, its verdict {} lifted to at least 2; \
             stderr: {:?}",
            row.verdict,
            run.stderr
        );
        assert_eq!(
            (
                run.stderr.matches("mds::io").count(),
                run.stderr.matches("cannot write to stdout").count()
            ),
            (1, 1),
            "`mds {what}` into {how} must report the failure exactly once, as one mds::io \
             error naming stdout; stderr: {:?}",
            run.stderr
        );
        failing.push(run);
    }
    (open, failing)
}

/// Write `big.mds`, one byte over the 10 MiB source cap.
fn write_an_oversized_source(dir: &Path) {
    std::fs::write(dir.join("big.mds"), vec![b'x'; STDIN_CAP + 1]).expect("write big.mds");
}

/// A clean JSON report into a failing stdout exits 2, not 0: the lost report is the
/// run's only output (#157).
#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn lint_json_on_a_clean_file_into_a_failing_stdout_exits_2() {
    assert_a_failing_stdout_lifts_the_verdict(&FullRow {
        args: &["lint", "--format", "json", "ok.mds"],
        stdin: "",
        verdict: 0,
        open_contains: &["{\"files\":[],\"truncated\":false,\"version\":1}\n"],
        prepare: |_| {},
    });
}

#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn lint_json_on_a_clean_directory_into_a_failing_stdout_exits_2() {
    assert_a_failing_stdout_lifts_the_verdict(&FullRow {
        args: &["lint", "--format", "json", "good"],
        stdin: "",
        verdict: 0,
        open_contains: &["{\"files\":[],\"truncated\":false,\"version\":1}\n"],
        prepare: |_| {},
    });
}

/// The fixed source has no final newline, so it waits in stdout's buffer and only the
/// flush fails.
#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn lint_fix_of_stdin_without_a_final_newline_into_a_failing_stdout_exits_2() {
    let (open, _) = assert_a_failing_stdout_lifts_the_verdict(&FullRow {
        args: &["lint", "--fix", "-"],
        stdin: "Hello",
        verdict: 0,
        open_contains: &["Hello"],
        prepare: |_| {},
    });
    assert_eq!(
        open.stdout, "Hello",
        "control: the source must be written back without a final newline"
    );
}

/// A resource limit keeps its 3 when the report of it is lost as well.
#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn lint_json_on_a_file_over_the_size_limit_into_a_failing_stdout_exits_3() {
    assert_a_failing_stdout_lifts_the_verdict(&FullRow {
        args: &["lint", "--format", "json", "big.mds"],
        stdin: "",
        verdict: 3,
        open_contains: &["\"code\":\"mds::resource_limit\""],
        prepare: write_an_oversized_source,
    });
}

/// Run `row`, a directory run whose files each write a diff, as
/// [`assert_a_failing_stdout_lifts_the_verdict`] does, and check its summary line on
/// stderr: `open_summary` with every diff written, `lost_summary` once a failing stdout
/// has lost them all.
///
/// One rule for `mds fmt <dir>` and `mds lint <dir>` (#157): a file whose own diff a
/// failing stdout lost counts as failed — `N failed` for `mds fmt`, `with errors` for
/// `mds lint` — as a file whose rewrite fails does, while the failure is reported once
/// for the run. A closed stdout loses nothing: its rows above keep the open run's
/// summary.
fn assert_each_lost_diff_counts_as_failed(row: &FullRow, open_summary: &str, lost_summary: &str) {
    let what = row.args.join(" ");
    let (open, failing) = assert_a_failing_stdout_lifts_the_verdict(row);
    assert!(
        open.stderr.contains(open_summary),
        "control: `mds {what}` into an open pipe must print {open_summary:?}; stderr: {:?}",
        open.stderr
    );
    for run in &failing {
        assert!(
            run.stderr.contains(lost_summary),
            "`mds {what}` must count each file whose diff a failing stdout lost as failed, \
             {lost_summary:?}; stderr: {:?}",
            run.stderr
        );
    }
}

#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn fmt_diff_of_a_directory_into_a_failing_stdout_counts_each_lost_diff_as_failed() {
    assert_each_lost_diff_counts_as_failed(
        &FullRow {
            args: &["fmt", "--diff", "two"],
            stdin: "",
            verdict: 0,
            open_contains: &["+++ two/a.mds\n", "+++ two/b.mds\n"],
            prepare: |_| {},
        },
        "2 would reformat, 0 unchanged, 0 failed\n",
        "0 would reformat, 0 unchanged, 2 failed\n",
    );
}

#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn lint_fix_diff_of_a_directory_into_a_failing_stdout_counts_each_lost_diff_under_errors() {
    assert_each_lost_diff_counts_as_failed(
        &FullRow {
            args: &["lint", "--fix", "--diff", "fixes"],
            stdin: "",
            verdict: 1,
            open_contains: &["+++ fixes/a.mds\n", "+++ fixes/b.mds\n"],
            prepare: |_| {},
        },
        "2 clean, 0 with warnings, 0 with errors, 0 resource-limited\n",
        "0 clean, 0 with warnings, 2 with errors, 0 resource-limited\n",
    );
}

/// The `--format json` directory emitter is a separate one; the same rule holds for it.
#[test]
#[cfg_attr(not(unix), ignore = "needs /dev/full or a file-size limit (unix)")]
fn lint_json_fix_diff_of_a_directory_into_a_failing_stdout_counts_each_lost_diff_under_errors() {
    assert_each_lost_diff_counts_as_failed(
        &FullRow {
            args: &["lint", "--format", "json", "--fix", "--diff", "fixes"],
            stdin: "",
            verdict: 1,
            open_contains: &["+++ fixes/a.mds\n", "+++ fixes/b.mds\n", "\"version\":1}\n"],
            prepare: |_| {},
        },
        "2 clean, 0 with warnings, 0 with errors, 0 resource-limited\n",
        "0 clean, 0 with warnings, 2 with errors, 0 resource-limited\n",
    );
}

// ── clap's own output: help, version and usage errors ────────────────────────

#[test]
fn help_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["--help"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "Usage: mds",
    });
}

#[test]
fn version_with_stdout_closed_exits_0() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["--version"],
        stdin: "",
        verdict: 0,
        closed: Stream::Stdout,
        open_contains: "mds ",
    });
}

#[test]
fn a_usage_error_with_stderr_closed_exits_2() {
    assert_closing_keeps_the_verdict(&Row {
        args: &["build", "--no-such-flag"],
        stdin: "",
        verdict: 2,
        closed: Stream::Stderr,
        open_contains: "unexpected argument",
    });
}

// ── An output that cannot be written: mds::io, exit 2 ────────────────────────

/// Assert that `run` exited `code` and reported the error code `mds_code` on stderr.
fn assert_exit_and_code(run: &Run, what: &str, code: i32, mds_code: &str) {
    assert_eq!(
        run.code,
        Some(code),
        "`mds {what}` must exit {code}; stderr: {:?}",
        run.stderr
    );
    assert!(
        run.stderr.contains(mds_code),
        "`mds {what}` must report {mds_code}; stderr: {:?}",
        run.stderr
    );
}

/// A directory the test's user cannot create files in (mode `0o555`), made writable
/// again on drop so the tempdir can be removed.
#[cfg(unix)]
struct ReadOnlyDir(PathBuf);

#[cfg(unix)]
impl ReadOnlyDir {
    /// `None` when a file can still be created in `path` at mode `0o555` — root ignores
    /// the mode — after printing why the caller skips.
    fn new(path: &Path) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))
            .expect("chmod 0o555");
        let guard = Self(path.to_path_buf());
        let probe = path.join(".write-probe");
        if std::fs::write(&probe, b"").is_ok() {
            let _ = std::fs::remove_file(&probe);
            eprintln!(
                "skipped: a file can be created in {} at mode 0o555 (running as root?)",
                path.display()
            );
            return None;
        }
        Some(guard)
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// A fixture dir with `ro/` holding `files`, made read-only. `None` when the mode does
/// not stop writes (see [`ReadOnlyDir::new`]).
#[cfg(unix)]
fn fixture_with_read_only_dir(files: &[(&str, &str)]) -> Option<(tempfile::TempDir, ReadOnlyDir)> {
    let dir = fixture_dir();
    let ro = dir.path().join("ro");
    std::fs::create_dir(&ro).expect("create ro");
    for (name, text) in files {
        std::fs::write(ro.join(name), text).expect("write a file in ro");
    }
    let guard = ReadOnlyDir::new(&ro)?;
    Some((dir, guard))
}

/// Run `mds <args>` in `dir` with open pipes and no stdin.
#[cfg(unix)]
fn run_in(dir: &Path, args: &[&str]) -> Run {
    run(dir, args, "", None)
}

#[cfg(unix)]
#[test]
fn a_build_whose_output_cannot_be_written_exits_2() {
    let Some((dir, _ro)) = fixture_with_read_only_dir(&[]) else {
        return;
    };
    let run = run_in(dir.path(), &["build", "ok.mds", "-o", "ro/out.md"]);
    assert_exit_and_code(&run, "build ok.mds -o ro/out.md", 2, "mds::io");
    assert!(
        !dir.path().join("ro/out.md").exists(),
        "nothing may be written"
    );
}

#[cfg(unix)]
#[test]
fn a_directory_build_with_one_unwritable_output_exits_2_and_writes_the_others() {
    let dir = fixture_dir();
    let w = dir.path().join("w");
    std::fs::create_dir_all(w.join("ro")).expect("create w/ro");
    std::fs::write(w.join("a.mds"), "A\n").expect("write w/a.mds");
    std::fs::write(w.join("ro/b.mds"), "B\n").expect("write w/ro/b.mds");
    let Some(_ro) = ReadOnlyDir::new(&w.join("ro")) else {
        return;
    };
    let run = run_in(dir.path(), &["build", "w"]);
    assert_exit_and_code(&run, "build w", 2, "mds::io");
    assert!(
        run.stderr.contains("1 built, 1 failed"),
        "the summary must count the write failure; stderr: {:?}",
        run.stderr
    );
    assert_eq!(
        std::fs::read_to_string(w.join("a.md")).ok().as_deref(),
        Some("A\n"),
        "the output that can be written must still be written"
    );
}

#[cfg(unix)]
#[test]
fn fmt_of_a_file_it_cannot_rewrite_exits_2() {
    let Some((dir, _ro)) = fixture_with_read_only_dir(&[("messy.mds", "Hello")]) else {
        return;
    };
    let run = run_in(dir.path(), &["fmt", "ro/messy.mds"]);
    assert_exit_and_code(&run, "fmt ro/messy.mds", 2, "mds::io");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ro/messy.mds"))
            .ok()
            .as_deref(),
        Some("Hello"),
        "the file must be left as it was"
    );
}

#[cfg(unix)]
#[test]
fn fmt_of_a_directory_with_a_file_it_cannot_rewrite_exits_2() {
    let Some((dir, _ro)) = fixture_with_read_only_dir(&[("messy.mds", "Hello")]) else {
        return;
    };
    let run = run_in(dir.path(), &["fmt", "ro"]);
    assert_exit_and_code(&run, "fmt ro", 2, "mds::io");
    assert!(
        run.stderr.contains("0 formatted, 0 unchanged, 1 failed"),
        "the summary must count the write failure; stderr: {:?}",
        run.stderr
    );
}

#[cfg(unix)]
#[test]
fn lint_fix_of_a_file_it_cannot_rewrite_exits_2_with_the_io_code() {
    let fixable = "@if \"x\" == \"y\":\nhidden\n@end\nHello\n";
    let Some((dir, _ro)) = fixture_with_read_only_dir(&[("fix.mds", fixable)]) else {
        return;
    };
    let run = run_in(dir.path(), &["lint", "--fix", "ro/fix.mds"]);
    assert_exit_and_code(&run, "lint --fix ro/fix.mds", 2, "mds::io");
}

#[cfg(unix)]
#[test]
fn init_in_a_directory_it_cannot_write_exits_2() {
    let Some((dir, _ro)) = fixture_with_read_only_dir(&[]) else {
        return;
    };
    let run = run_in(dir.path(), &["init", "ro/hello.mds"]);
    assert_exit_and_code(&run, "init ro/hello.mds", 2, "mds::io");
    assert!(
        !dir.path().join("ro/hello.mds").exists(),
        "nothing may be written"
    );
}

// ── A directory run whose file fails with an I/O or file-system error: exit 2 ─

/// A fixture dir with `u/a.mds`, which compiles but lacks the final newline `mds fmt`
/// adds, and `u/b.mds`, which the test's user cannot read (mode `0o000`). `None`, after
/// printing why, when the mode does not stop the read — root ignores it.
#[cfg(unix)]
fn fixture_with_unreadable_source() -> Option<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = fixture_dir();
    let u = dir.path().join("u");
    std::fs::create_dir(&u).expect("create u");
    std::fs::write(u.join("a.mds"), "A").expect("write u/a.mds");
    let b = u.join("b.mds");
    std::fs::write(&b, "B\n").expect("write u/b.mds");
    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o000)).expect("chmod 0o000");
    if std::fs::read(&b).is_ok() {
        eprintln!(
            "skipped: {} can be read at mode 0o000 (running as root?)",
            b.display()
        );
        return None;
    }
    Some(dir)
}

/// Assert a directory run that could not read one of its sources: exit 2, the failure
/// reported as `mds::io`, and `summary` on stderr — the other file was processed.
#[cfg(unix)]
fn assert_unreadable_source_exits_2(run: &Run, what: &str, summary: &str) {
    assert_exit_and_code(run, what, 2, "mds::io");
    assert!(
        run.stderr.contains("b.mds"),
        "`mds {what}` must name the file it could not read; stderr: {:?}",
        run.stderr
    );
    assert!(
        run.stderr.contains(summary),
        "`mds {what}` must process the other file and count the failed one: {summary:?}; \
         stderr: {:?}",
        run.stderr
    );
}

#[cfg(unix)]
#[test]
fn a_directory_build_with_an_unreadable_source_exits_2() {
    let Some(dir) = fixture_with_unreadable_source() else {
        return;
    };
    let run = run_in(dir.path(), &["build", "u"]);
    assert_unreadable_source_exits_2(&run, "build u", "1 built, 1 failed");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("u/a.md"))
            .ok()
            .as_deref(),
        Some("A\n"),
        "the source that can be read must still be built"
    );
}

#[cfg(unix)]
#[test]
fn a_directory_check_with_an_unreadable_source_exits_2() {
    let Some(dir) = fixture_with_unreadable_source() else {
        return;
    };
    let run = run_in(dir.path(), &["check", "u"]);
    assert_unreadable_source_exits_2(&run, "check u", "1 passed, 1 failed");
}

#[cfg(unix)]
#[test]
fn a_directory_fmt_with_an_unreadable_source_exits_2() {
    let Some(dir) = fixture_with_unreadable_source() else {
        return;
    };
    let run = run_in(dir.path(), &["fmt", "u"]);
    assert_unreadable_source_exits_2(&run, "fmt u", "1 formatted, 0 unchanged, 1 failed");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("u/a.mds"))
            .ok()
            .as_deref(),
        Some("A\n"),
        "the source that can be read must still be formatted"
    );
}

/// A directory build or check with a file that imports a file that does not exist
/// exits 2, as that file given alone does: `mds::file_not_found` is in the I/O and
/// file-system class (#157). Unlike the unreadable-source tests, this runs on every OS.
#[test]
fn a_directory_build_or_check_with_a_missing_import_exits_2() {
    for (args, summary) in [
        (&["build", "i"][..], "1 built, 1 failed"),
        (&["check", "i"][..], "1 passed, 1 failed"),
    ] {
        let what = args.join(" ");
        let dir = fixture_dir();
        let i = dir.path().join("i");
        std::fs::create_dir(&i).expect("create i");
        std::fs::write(i.join("a.mds"), "@import \"./nope.mds\" as n\n\nHi\n")
            .expect("write i/a.mds");
        std::fs::write(i.join("b.mds"), "Ok\n").expect("write i/b.mds");
        let run = run(dir.path(), args, "", None);
        assert_exit_and_code(&run, &what, 2, "mds::file_not_found");
        assert!(
            run.stderr.contains(summary),
            "`mds {what}` must process the other file and count the failed one: \
             {summary:?}; stderr: {:?}",
            run.stderr
        );
    }
}

/// Control for the tests above: a directory run whose only failures are a
/// template error and a file over the size cap still exits 1 — only an I/O or
/// file-system failure lifts it to 2 (#157).
#[test]
fn a_directory_run_whose_failures_are_a_template_error_and_a_resource_limit_exits_1() {
    let over_cap = "x".repeat(STDIN_CAP + 1);
    for (args, summary) in [
        (&["build", "t"][..], "1 built, 2 failed"),
        (&["check", "t"][..], "1 passed, 2 failed"),
        (&["fmt", "t"][..], "0 formatted, 1 unchanged, 2 failed"),
    ] {
        let what = args.join(" ");
        let dir = fixture_dir();
        let t = dir.path().join("t");
        std::fs::create_dir(&t).expect("create t");
        std::fs::write(t.join("ok.mds"), "Hello\n").expect("write t/ok.mds");
        std::fs::write(t.join("bad.mds"), "@if flag:\nunterminated\n").expect("write t/bad.mds");
        std::fs::write(t.join("big.mds"), &over_cap).expect("write t/big.mds");
        let run = run(dir.path(), args, "", None);
        assert_exit_and_code(&run, &what, 1, "mds::resource_limit");
        assert!(
            run.stderr.contains("mds::syntax") && !run.stderr.contains("mds::io"),
            "`mds {what}` must report the template error and no I/O failure; stderr: {:?}",
            run.stderr
        );
        assert!(
            run.stderr.contains(summary),
            "`mds {what}` must count both failures: {summary:?}; stderr: {:?}",
            run.stderr
        );
    }
}

// ── Stdin that cannot be read, is not UTF-8, or is over the cap ──────────────

/// The four subcommands that read a source from stdin.
const STDIN_COMMANDS: [&[&str]; 4] = [
    &["build", "-"],
    &["check", "-"],
    &["fmt", "-"],
    &["lint", "-"],
];

/// A stdin handle every read of fails: a directory on unix, a write-only file on
/// Windows. Kept alive by the returned tempdir.
fn unreadable_stdin() -> (tempfile::TempDir, Stdio) {
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    let handle = std::fs::File::open(dir.path()).expect("open a directory handle");
    #[cfg(windows)]
    let handle = std::fs::File::create(dir.path().join("stdin")).expect("create a write-only file");
    (dir, Stdio::from(handle))
}

#[test]
fn stdin_that_cannot_be_read_exits_2() {
    for args in STDIN_COMMANDS {
        let what = args.join(" ");
        let dir = fixture_dir();
        let (_keep, stdin) = unreadable_stdin();
        let run = run_with(
            dir.path(),
            args,
            Input::Handle(stdin),
            Stdio::piped(),
            Stdio::piped(),
        );
        assert_exit_and_code(&run, &what, 2, "mds::io");
        assert!(
            run.stderr.contains("cannot read stdin"),
            "`mds {what}` must say stdin could not be read; stderr: {:?}",
            run.stderr
        );
    }
}

#[test]
fn stdin_that_is_not_utf8_exits_2() {
    for args in STDIN_COMMANDS {
        let what = args.join(" ");
        let dir = fixture_dir();
        let run = run_with(
            dir.path(),
            args,
            Input::Bytes(b"Hello \xff\n".to_vec()),
            Stdio::piped(),
            Stdio::piped(),
        );
        assert_exit_and_code(&run, &what, 2, "mds::io");
        assert!(
            run.stderr.contains("valid UTF-8"),
            "`mds {what}` must say stdin was not UTF-8; stderr: {:?}",
            run.stderr
        );
    }
}

/// The per-source cap stdin is read against: 10 MiB.
const STDIN_CAP: usize = 10 * 1024 * 1024;

#[test]
fn stdin_over_the_cap_exits_3() {
    let lint_json: &[&str] = &["lint", "--format", "json", "-"];
    for args in STDIN_COMMANDS.into_iter().chain([lint_json]) {
        let what = args.join(" ");
        let dir = fixture_dir();
        let run = run_with(
            dir.path(),
            args,
            Input::Bytes(vec![b'x'; STDIN_CAP + 1]),
            Stdio::piped(),
            Stdio::piped(),
        );
        assert_exit_and_code(&run, &what, 3, "mds::resource_limit");
    }
}

/// The source ends in a newline, so `mds fmt` has nothing to change and the run tests
/// the size gate alone.
#[test]
fn stdin_of_exactly_the_cap_passes_the_size_gate() {
    let mut at_cap = vec![b'x'; STDIN_CAP - 1];
    at_cap.push(b'\n');
    for args in STDIN_COMMANDS {
        let what = args.join(" ");
        let dir = fixture_dir();
        let run = run_with(
            dir.path(),
            args,
            Input::Bytes(at_cap.clone()),
            Stdio::piped(),
            Stdio::piped(),
        );
        assert_eq!(
            run.code,
            Some(0),
            "`mds {what}` must accept exactly the cap; stderr: {:?}",
            run.stderr
        );
    }
}
