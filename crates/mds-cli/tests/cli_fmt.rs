//! Integration tests for `mds fmt` (issue #60).
//!
//! Coverage maps to AC-CF-1..8 from the execution plan:
//! - AC-CF-1: file.mds rewrites in place only when content changes, unchanged
//!   keeps mtime, prints Formatted/Unchanged to stderr
//! - AC-CF-2: `-` reads stdin -> stdout, creates no file
//! - AC-CF-3: `.` formats every .mds recursively INCLUDING _partials,
//!   continue-on-error, summary `N formatted, M unchanged, K failed`, exit 1
//!   iff any failed
//! - AC-CF-4: --check exits 0 when clean / 1 when any would change or fails,
//!   NEVER writes
//! - AC-CF-5: --diff prints unified diff to stdout (colorized only on TTY),
//!   writes nothing, exit 0 unless read/parse error -> 1
//! - AC-CF-6: --check --diff prints diffs AND exits non-zero when changes exist
//! - AC-CF-7: no-arg auto-detects single .mds in cwd, errors on zero/many
//! - AC-CF-8: -q/--quiet suppresses status but never errors

mod common;
use common::{fixture, mds_bin};

use std::fs;
use std::path::Path;
use std::time::Duration;

fn read_fixture(name: &str) -> String {
    fs::read_to_string(fixture(name)).expect("read fixture")
}

/// Run `mds fmt <path> <extra_args>`, capturing stdout/stderr separately.
///
/// `mds fmt` dispatches on the filesystem type of `path` (file, dir, or `-`
/// for stdin — see the dedicated `fmt_stdin` helper below), so one helper
/// covers both single-file and directory-mode invocations.
fn fmt_path(path: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("fmt")
        .arg(path)
        .args(extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap()
}

// ── AC-CF-1: single-file in-place rewrite, mtime, status lines ──────────────

#[test]
fn inplace_rewrites_only_when_changed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("unformatted.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();
    let original = fs::read_to_string(&target).unwrap();

    let output = fmt_path(&target, &[]);
    assert!(
        output.status.success(),
        "fmt should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = fs::read_to_string(&target).unwrap();
    assert_ne!(after, original, "content should have changed");
    assert!(!after.contains('\r'), "CRLF should be gone: {after:?}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Formatted"),
        "expected a Formatted status line, got: {stderr}"
    );
}

#[test]
fn inplace_unchanged_file_preserves_mtime_and_reports_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clean.mds");
    fs::write(&target, read_fixture("fmt_formatted.mds")).unwrap();

    let before_bytes = fs::read(&target).unwrap();
    let before_mtime = fs::metadata(&target).unwrap().modified().unwrap();
    // Ensure the filesystem clock would visibly move if the file were rewritten.
    std::thread::sleep(Duration::from_millis(1100));

    let output = fmt_path(&target, &[]);
    assert!(
        output.status.success(),
        "fmt should succeed on an already-clean file"
    );

    // Assert both that the mtime is unchanged AND that the bytes are identical:
    // the mtime check alone is not enough to distinguish "same content written"
    // from "write skipped", because some filesystems may round mtime.
    let after_bytes = fs::read(&target).unwrap();
    assert_eq!(
        before_bytes, after_bytes,
        "file bytes must be identical for an already-clean file (no rewrite should occur)"
    );

    let after_mtime = fs::metadata(&target).unwrap().modified().unwrap();
    assert_eq!(
        before_mtime, after_mtime,
        "mtime must not change for an unchanged file"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unchanged"),
        "expected an Unchanged status line, got: {stderr}"
    );
}

#[test]
fn format_then_check_is_idempotent() {
    // T2-idem: format a file, then --check it -- must report clean (exit 0).
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("idem.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();

    let first = fmt_path(&target, &[]);
    assert!(first.status.success());

    let check = fmt_path(&target, &["--check"]);
    assert!(
        check.status.success(),
        "second pass under --check should report clean; stderr: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

// ── T1: safety-gate path coverage (Syntax propagation + structural fallback) ──
//
// FormatterInvariant end-to-end is deliberately untested here: triggering it
// requires a formatter bug (the gate fires only when the formatter itself
// diverges from the original output). That cannot be induced without a test
// hook inside formatter.rs, which falls outside the scope of test-only changes.
// The white-box unit tests for `structural_equivalent` inside formatter.rs's
// own `#[cfg(test)]` module cover both branches of that function directly.
//
// What we DO test here:
//   - Syntax propagation: `unclosed_directive_block_exits_one_with_syntax_not_formatter_invariant`
//     (already present above) proves the gate surfaces real syntax errors and
//     leaves the file untouched.
//   - structural_equivalent fallback: the test below proves that a source file
//     referencing an undefined runtime variable (which doesn't compile standalone)
//     is still formatted via the token-comparison fallback, not rejected.

#[test]
fn structural_fallback_formats_file_with_undefined_var() {
    // A source with an undefined runtime variable does not compile standalone,
    // so the safety gate cannot do a real recompile-and-diff. It must fall
    // back to the structural_equivalent token comparison and succeed, because
    // real template files are often written for runtime vars supplied at render
    // time — refusing to format them would make `mds fmt` useless on templates.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("template.mds");
    let src = "Hello {undefined_runtime_var}!\n\n\n\nBye.\n";
    fs::write(&target, src).unwrap();

    let output = fmt_path(&target, &[]);
    assert!(
        output.status.success(),
        "fmt must succeed on a file with undefined runtime var (structural fallback); \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = fs::read_to_string(&target).unwrap();
    // The blank-line run (4 newlines = 3 blank lines) must still be collapsed
    // to a single blank line (2 newlines) even via the structural fallback.
    assert_eq!(
        after, "Hello {undefined_runtime_var}!\n\nBye.\n",
        "blank-line collapse must happen even via the structural_equivalent fallback path"
    );
}

// ── AC-CF-2: stdin -> stdout, no file created ────────────────────────────────

fn fmt_stdin(input: &str, extra_args: &[&str]) -> std::process::Output {
    use std::io::Write;
    let mut child = mds_bin()
        .arg("fmt")
        .arg("-")
        .args(extra_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn stdin_reads_and_writes_stdout_creates_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let before: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    assert!(before.is_empty());

    let input = read_fixture("fmt_unformatted.mds");
    let output = mds_bin()
        .current_dir(dir.path())
        .arg("fmt")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains('\r'),
        "stdout should be reformatted: {stdout:?}"
    );

    let after: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    assert!(after.is_empty(), "fmt - must not create any file in cwd");
}

#[test]
fn stdin_filter_mode_is_idempotent() {
    // T2-filter-idem: pipe formatted output back through `fmt -` -> identical.
    let input = read_fixture("fmt_unformatted.mds");
    let first = fmt_stdin(&input, &[]);
    assert!(first.status.success());
    let once = String::from_utf8(first.stdout).unwrap();

    let second = fmt_stdin(&once, &[]);
    assert!(second.status.success());
    let twice = String::from_utf8(second.stdout).unwrap();

    assert_eq!(once, twice, "stdin filter mode must be idempotent");
}

#[test]
fn stdin_into_closed_pipe_does_not_panic() {
    // Piping into `true` closes the read end almost immediately; writing the
    // formatted result to stdout must handle a broken pipe gracefully.
    use std::io::Write;
    let mut child = mds_bin()
        .arg("fmt")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Drop stdout immediately to force a broken pipe on write.
    drop(child.stdout.take());

    let big_input = read_fixture("fmt_unformatted.mds").repeat(1000);
    // Ignore write errors -- the point is the CHILD process must not panic.
    let _ = child.stdin.take().unwrap().write_all(big_input.as_bytes());

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "must not panic on broken pipe, got stderr: {stderr}"
    );
}

// ── AC-CF-3: directory mode, partials included, continue-on-error, summary ──

#[test]
fn dir_recurse_formats_every_mds_including_partials() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("main.mds"),
        read_fixture("fmt_unformatted.mds"),
    )
    .unwrap();
    fs::write(
        dir.path().join("_partial.mds"),
        read_fixture("_fmt_partial.mds"),
    )
    .unwrap();

    let output = fmt_path(dir.path(), &[]);
    assert!(
        output.status.success(),
        "dir fmt should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let main_after = fs::read_to_string(dir.path().join("main.mds")).unwrap();
    assert!(!main_after.contains('\r'), "main.mds should be reformatted");

    // Partials ARE formatted (deliberate divergence from build/check). The
    // partial's directive lines (@define/@end/@export) and the blank-line run
    // between @end and @export are OUTSIDE the @define body and must be
    // reformatted; the "Hello {x}!\r\n" line INSIDE the @define body
    // deliberately keeps its \r (raw-content exemption -- see formatter.rs's
    // module docs and the `message_body_carriage_return_is_preserved`-style
    // tests in mds-core/tests/fmt.rs), so this asserts the directive-level
    // change rather than a blanket "no \\r anywhere".
    let partial_after = fs::read_to_string(dir.path().join("_partial.mds")).unwrap();
    assert_ne!(
        partial_after,
        read_fixture("_fmt_partial.mds"),
        "_partial.mds should ALSO be reformatted"
    );
    assert!(
        partial_after.contains("@define greet(x):\n"),
        "directive trailing ws/\\r should be stripped even in a partial, got: {partial_after:?}"
    );
    assert!(
        partial_after.contains("@end\n\n@export"),
        "blank-line run between @end and @export should collapse, got: {partial_after:?}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("formatted") && stderr.contains("unchanged") && stderr.contains("failed"),
        "expected a summary with formatted/unchanged/failed counts, got: {stderr}"
    );
}

#[test]
fn dir_continue_on_error_summary_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("good.mds"), "Hello, world!\n").unwrap();
    fs::write(dir.path().join("bad.mds"), "```\nunclosed fence\n").unwrap();

    let output = fmt_path(dir.path(), &[]);
    assert!(
        !output.status.success(),
        "dir fmt with a failing file must exit non-zero"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 failed") || stderr.contains("failed"),
        "summary must mention the failure, got: {stderr}"
    );
}

#[test]
fn dir_already_formatted_tree_reports_all_unchanged_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("clean.mds"),
        read_fixture("fmt_formatted.mds"),
    )
    .unwrap();

    let output = fmt_path(dir.path(), &[]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 unchanged"),
        "expected '1 unchanged' in summary, got: {stderr}"
    );
}

// ── AC-CF-9 (EC-9): partial + parent that @includes it stay compile-equivalent ─

#[test]
fn parent_that_includes_partial_compiles_unchanged_after_formatting_both() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("_fmt_partial.mds"),
        read_fixture("_fmt_partial.mds"),
    )
    .unwrap();
    let parent = dir.path().join("parent.mds");
    fs::write(&parent, read_fixture("fmt_parent_includes_partial.mds")).unwrap();

    let before = mds_bin()
        .arg("build")
        .arg(&parent)
        .arg("-o")
        .arg("-")
        .output()
        .unwrap();
    assert!(before.status.success(), "pre-format build should succeed");

    let output = fmt_path(dir.path(), &[]);
    assert!(
        output.status.success(),
        "formatting the dir should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = mds_bin()
        .arg("build")
        .arg(&parent)
        .arg("-o")
        .arg("-")
        .output()
        .unwrap();
    assert!(after.status.success(), "post-format build should succeed");
    assert_eq!(
        before.stdout, after.stdout,
        "compiled output must be unchanged after formatting the partial + parent"
    );
}

// ── AC-CF-4: --check exits 0 clean / 1 dirty, never writes ──────────────────

#[test]
fn check_clean_file_exits_zero_no_write() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clean.mds");
    fs::write(&target, read_fixture("fmt_formatted.mds")).unwrap();
    let before = fs::read_to_string(&target).unwrap();
    let before_mtime = fs::metadata(&target).unwrap().modified().unwrap();

    let output = fmt_path(&target, &["--check"]);
    assert!(output.status.success(), "check on clean file should exit 0");

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        before,
        "must not write"
    );
    assert_eq!(
        fs::metadata(&target).unwrap().modified().unwrap(),
        before_mtime
    );
}

#[test]
fn check_dirty_file_exits_nonzero_no_write() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();
    let before = fs::read_to_string(&target).unwrap();

    let output = fmt_path(&target, &["--check"]);
    assert!(
        !output.status.success(),
        "check on a file that would change must exit non-zero"
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        before,
        "--check must never write"
    );
}

#[test]
fn dir_check_reports_would_reformat_and_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dirty.mds"),
        read_fixture("fmt_unformatted.mds"),
    )
    .unwrap();

    let output = fmt_path(dir.path(), &["--check"]);
    assert!(
        !output.status.success(),
        "dir --check with dirty files must exit non-zero"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("would reformat"),
        "expected 'would reformat' summary wording under --check, got: {stderr}"
    );

    // No file should have been touched.
    let content = fs::read_to_string(dir.path().join("dirty.mds")).unwrap();
    assert!(
        content.contains('\r'),
        "--check must never write, CRLF should survive"
    );
}

// ── AC-CF-5: --diff prints unified diff, writes nothing ─────────────────────

#[test]
fn diff_prints_unified_diff_to_stdout_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();
    let before = fs::read_to_string(&target).unwrap();

    let output = fmt_path(&target, &["--diff"]);
    assert!(
        output.status.success(),
        "--diff alone should exit 0 even when changes exist"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    // `---`/`+++` are the unified diff file headers; `@@` is the hunk header.
    // Checking `@@` is more discriminating than `---` alone, which could be
    // satisfied by frontmatter content that starts with `---`.
    assert!(
        stdout.contains("---"),
        "expected unified diff --- header, got: {stdout:?}"
    );
    assert!(
        stdout.contains("+++"),
        "expected unified diff +++ header, got: {stdout:?}"
    );
    assert!(
        stdout.contains("@@"),
        "expected unified diff @@ hunk header, got: {stdout:?}"
    );
    assert!(
        stdout.lines().any(|l| l.starts_with('-')) || stdout.lines().any(|l| l.starts_with('+')),
        "expected +/- diff lines, got: {stdout:?}"
    );

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        before,
        "--diff must never write"
    );
}

#[test]
fn diff_on_clean_file_prints_nothing_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clean.mds");
    fs::write(&target, read_fixture("fmt_formatted.mds")).unwrap();

    let output = fmt_path(&target, &["--diff"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "no diff expected for a clean file, got: {stdout:?}"
    );
}

#[test]
fn diff_on_malformed_file_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("bad.mds");
    fs::write(&target, read_fixture("fmt_malformed.mds")).unwrap();

    let output = fmt_path(&target, &["--diff"]);
    assert!(
        !output.status.success(),
        "--diff on unparseable input must exit non-zero"
    );
}

// ── AC-CF-6: --check --diff combined ─────────────────────────────────────────

#[test]
fn check_and_diff_combined_prints_diff_and_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();

    let output = fmt_path(&target, &["--check", "--diff"]);
    assert!(
        !output.status.success(),
        "--check --diff must exit non-zero when changes exist"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("---") && stdout.contains("+++"),
        "expected a diff, got: {stdout:?}"
    );
}

// ── AC-CF-7: no-arg auto-detect ──────────────────────────────────────────────

#[test]
fn autodetect_single_mds_file_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("only.mds"),
        read_fixture("fmt_unformatted.mds"),
    )
    .unwrap();

    let output = mds_bin()
        .current_dir(dir.path())
        .arg("fmt")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "auto-detect should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let content = fs::read_to_string(dir.path().join("only.mds")).unwrap();
    assert!(
        !content.contains('\r'),
        "auto-detected file should be formatted"
    );
}

#[test]
fn autodetect_errors_on_zero_files() {
    let dir = tempfile::tempdir().unwrap();
    let output = mds_bin()
        .current_dir(dir.path())
        .arg("fmt")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "auto-detect with zero .mds files must fail"
    );
}

#[test]
fn autodetect_errors_on_multiple_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.mds"), "Hello!\n").unwrap();
    fs::write(dir.path().join("b.mds"), "Hello!\n").unwrap();
    let output = mds_bin()
        .current_dir(dir.path())
        .arg("fmt")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "auto-detect with multiple .mds files must fail"
    );
}

// ── AC-CF-8: --quiet suppresses status but never errors ─────────────────────

#[test]
fn quiet_suppresses_status_line() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();

    let output = mds_bin()
        .arg("--quiet")
        .arg("fmt")
        .arg(&target)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.trim().is_empty(),
        "quiet should suppress status, got: {stderr}"
    );
}

#[test]
fn quiet_does_not_suppress_errors() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("bad.mds");
    fs::write(&target, read_fixture("fmt_malformed.mds")).unwrap();

    let output = mds_bin()
        .arg("--quiet")
        .arg("fmt")
        .arg(&target)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "malformed input must still fail under --quiet"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.trim().is_empty(),
        "errors must still be printed under --quiet"
    );
}

// ── Malformed input / missing / not-.mds / symlink / oversized / bad-utf8 ───

#[test]
fn malformed_single_file_exits_one_with_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("bad.mds");
    fs::write(&target, read_fixture("fmt_malformed.mds")).unwrap();

    let output = fmt_path(&target, &[]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "syntax error should map to exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("syntax") || stderr.contains("fence"),
        "expected a diagnostic mentioning the syntax problem, got: {stderr}"
    );
    // Must not have written a garbled file.
    let content = fs::read_to_string(&target).unwrap();
    assert_eq!(
        content,
        read_fixture("fmt_malformed.mds"),
        "malformed file must be untouched"
    );
}

#[test]
fn unclosed_directive_block_exits_one_with_syntax_not_formatter_invariant() {
    // An unclosed `@message` (parse-level malformation, unlike the unclosed
    // code fence in fmt_malformed.mds which fails in the lexer) must be
    // rejected with the real syntax error and leave the file untouched —
    // not silently "formatted" nor reported as a formatter bug.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("unclosed.mds");
    let src = "@message user:\nHi there\n";
    fs::write(&target, src).unwrap();

    let output = fmt_path(&target, &[]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "unclosed block should map to exit 1"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("syntax") && stderr.contains("@end"),
        "expected a syntax diagnostic about the missing @end, got: {stderr}"
    );
    assert!(
        !stderr.contains("formatter_invariant") && !stderr.contains("file an issue"),
        "author error must not be reported as a formatter bug, got: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        src,
        "malformed file must be left untouched"
    );
}

#[test]
fn dir_malformed_file_continues_and_reports_one_failed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("bad.mds"),
        read_fixture("fmt_malformed.mds"),
    )
    .unwrap();
    fs::write(
        dir.path().join("good.mds"),
        read_fixture("fmt_unformatted.mds"),
    )
    .unwrap();

    let output = fmt_path(dir.path(), &[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 failed"),
        "expected '1 failed' in summary, got: {stderr}"
    );

    // good.mds must still have been formatted (continue-on-error).
    let good_after = fs::read_to_string(dir.path().join("good.mds")).unwrap();
    assert!(
        !good_after.contains('\r'),
        "good.mds should still be formatted"
    );
}

#[test]
fn missing_file_exits_two() {
    let output = fmt_path(Path::new("/tmp/no_such_fmt_target_xyz.mds"), &[]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn non_mds_extension_rejected_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("notes.txt");
    fs::write(&target, "just some text\n").unwrap();

    let output = fmt_path(&target, &[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "non-.mds explicit file must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mds") || stderr.contains("extension"),
        "expected a not-an-mds-file diagnostic, got: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn symlinked_file_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.mds");
    fs::write(&real, "Hello!\n").unwrap();
    let link = dir.path().join("link.mds");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let output = fmt_path(&link, &[]);
    assert!(!output.status.success(), "symlinked file must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink"),
        "expected symlink diagnostic, got: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn symlinked_directory_root_rejected() {
    let real_dir = tempfile::tempdir().unwrap();
    fs::write(real_dir.path().join("page.mds"), "Hello!\n").unwrap();
    let link_parent = tempfile::tempdir().unwrap();
    let link_path = link_parent.path().join("linked");
    std::os::unix::fs::symlink(real_dir.path(), &link_path).unwrap();

    let output = fmt_path(&link_path, &[]);
    assert!(
        !output.status.success(),
        "symlinked directory root must be rejected"
    );
}

#[test]
fn oversized_file_exits_three() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("huge.mds");
    let content = "x".repeat(10 * 1024 * 1024 + 1);
    fs::write(&target, &content).unwrap();

    let output = fmt_path(&target, &[]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "oversized source should map to exit 3"
    );
}

#[test]
#[cfg(unix)]
fn bad_utf8_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("bad_utf8.mds");
    std::fs::write(&target, [0xFF, 0xFE, b'\n']).unwrap();

    let output = fmt_path(&target, &[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid UTF-8 should map to exit 2"
    );
}

// ── Channel discipline: capture stdout/stderr separately ────────────────────

#[test]
fn channels_stdout_only_content_stderr_only_status() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();

    // Write mode: nothing on stdout, status on stderr.
    let output = fmt_path(&target, &[]);
    assert!(
        output.stdout.is_empty(),
        "write mode must not print to stdout"
    );
    assert!(
        !output.stderr.is_empty(),
        "write mode should print status to stderr"
    );
}

#[test]
fn channels_diff_goes_to_stdout_summary_to_stderr_in_dir_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dirty.mds"),
        read_fixture("fmt_unformatted.mds"),
    )
    .unwrap();

    let output = fmt_path(dir.path(), &["--diff"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("---") || stdout.contains("+++"),
        "diff should be on stdout: {stdout}"
    );
    assert!(
        !stderr.contains("---") && !stderr.contains("+++"),
        "diff must not leak to stderr: {stderr}"
    );
}

// ── Config: mds.json `fmt` section ───────────────────────────────────────────

#[test]
fn fmt_config_section_parses_and_pre_existing_configs_still_load() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();
    fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "dist"}, "fmt": {"sort_frontmatter_keys": false}}"#,
    )
    .unwrap();

    let output = fmt_path(&target, &[]);
    assert!(
        output.status.success(),
        "fmt should succeed with a fmt-section config present; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pre_existing_config_without_fmt_section_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("dirty.mds");
    fs::write(&target, read_fixture("fmt_unformatted.mds")).unwrap();
    fs::write(
        dir.path().join("mds.json"),
        r#"{"build": {"output_dir": "dist"}}"#,
    )
    .unwrap();

    let output = fmt_path(&target, &[]);
    assert!(
        output.status.success(),
        "fmt should succeed with a pre-existing (no fmt section) config; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
