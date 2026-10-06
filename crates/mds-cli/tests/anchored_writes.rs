//! Every write stays below its anchor and never follows a symbolic link (#160).
//!
//! A write resolves its anchor by path — `--out-dir`, a directory argument's root, the
//! parent of a file argument or of `-o`, or, for `mds.json`'s `build.output_dir`, the
//! directory `mds.json` is in — so a symlinked anchor the user typed is still followed.
//! Every directory BELOW the anchor — `build.output_dir`'s own included — is opened
//! without following a symlink, so a link planted there, or swapped in while the run is
//! going, is refused (`mds::io`, exit 2) by the path the user would know it by, and
//! nothing is written through it.
//!
//! Each absence check here — nothing written through the link — sits beside the refusal
//! that proves the write was attempted after the link was in place, and beside a write
//! that did land, so no test passes on a run that wrote nothing.
//!
//! An expected path is written with `/` and printed through [`native`], so it names the
//! path in the platform's separator, exactly as `mds` prints it.

mod common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::mds_bin;

/// How long a run, or one step of a watch session, may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(20);

/// A scratch directory whose name no output could carry by chance.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("mds-anchored-")
        .tempdir()
        .expect("create a scratch directory")
}

/// `path`, written with `/`, in the platform's separator.
fn native(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// Write `contents` to `root/rel` (`rel` written with `/`), creating its directories.
fn put(root: &Path, rel: &str, contents: &str) -> PathBuf {
    let path = root.join(native(rel));
    let parent = path.parent().expect("a file below the root");
    std::fs::create_dir_all(parent).expect("create the fixture's directories");
    std::fs::write(&path, contents).expect("write a fixture file");
    path
}

/// `mds <args>` in `cwd`, stdin empty.
fn run(cwd: &Path, args: &[impl AsRef<OsStr>]) -> Output {
    mds_bin()
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run mds")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `s` without whitespace and without miette's frame gutter (U+2502): an error frame
/// wraps a long line, even inside a path, and continues it after the gutter.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{2502}')
        .collect()
}

/// The names in `dir`, sorted: what a directory a link pointed at holds.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("list a directory")
        .map(|entry| {
            entry
                .expect("read a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The refusal of a symlink at `component`, a directory below the anchor, as typed.
fn refused(component: &str) -> String {
    format!(
        "cannot write {}: refusing to follow a symlink",
        native(component)
    )
}

// ── A link planted below the anchor ─────────────────────────────────────────

/// A symlink planted below `--out-dir` is refused by the path the user knows it by, and
/// the directory it points at is left as it was. Controls: the output beside it, directly
/// in the out-dir, is written; the link itself is left in place; and a file written
/// through the link by hand does show up in the directory it points at, so the check
/// that nothing did can see one.
#[cfg(unix)]
#[test]
fn a_symlink_below_the_out_dir_is_refused_and_its_target_left_alone() {
    let dir = scratch();
    let root = dir.path();
    put(root, "src/sub/a.mds", "Hello {{name}}!\n");
    put(root, "src/top.mds", "Top\n");
    std::fs::create_dir(root.join("victim")).unwrap();
    std::fs::create_dir(root.join("out")).unwrap();
    std::os::unix::fs::symlink("../victim", root.join("out/sub")).unwrap();

    let out = run(
        root,
        &["build", "src", "--out-dir", "out", "--set", "name=World"],
    );
    let stderr = text(&out.stderr);
    assert_eq!(
        entries(&root.join("victim")),
        Vec::<String>::new(),
        "nothing is written through the symlink; stderr: {stderr}"
    );
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("mds::io") && squash(&stderr).contains(&squash(&refused("out/sub"))),
        "the refusal names the link as typed; stderr: {stderr}"
    );
    assert_eq!(read(&root.join("out/top.md")), "Top\n", "stderr: {stderr}");
    assert!(
        std::fs::symlink_metadata(root.join("out/sub"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is left in place"
    );

    // Control: the check above sees a file that does arrive through the link.
    std::fs::write(root.join("out/sub/by-hand"), "x").unwrap();
    assert_eq!(entries(&root.join("victim")), vec!["by-hand".to_owned()]);
}

/// The anchor itself is resolved by path, as before (#160): a symlink the user typed as
/// `--out-dir`, as the directory of `-o`, or as a file argument's `--out-dir`, is followed
/// and written through. (`mds.json`'s `build.output_dir` is no path the user typed: a
/// symlink there is refused, below.)
#[cfg(unix)]
#[test]
fn a_symlinked_anchor_the_user_named_is_still_followed() {
    let dir = scratch();
    let root = dir.path();
    put(root, "src/sub/a.mds", "A\n");
    std::fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink("real", root.join("link")).unwrap();

    // (arguments, the line naming the output, where the bytes land)
    for (args, line, landed) in [
        (
            &["build", "src", "--out-dir", "link"][..],
            "Compiled to link/sub/a.md",
            "real/sub/a.md",
        ),
        (
            &["build", "src/sub/a.mds", "-o", "link/one.md"][..],
            "Compiled to link/one.md",
            "real/one.md",
        ),
        (
            &["build", "src/sub/a.mds", "--out-dir", "link"][..],
            "Compiled to link/a.md",
            "real/a.md",
        ),
    ] {
        let out = run(root, args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains(&native(line)),
            "{args:?}: {line:?}; stderr: {stderr}"
        );
        let landed = root.join(native(landed));
        assert!(
            landed.is_file(),
            "{args:?}: {} was written",
            landed.display()
        );
    }
}

/// An `-o` that ends in a separator, or in a separator and a `.`, names a directory: it is
/// refused as one (`mds::io`, exit 2), and no file of the name before that ending is
/// created, nor an existing one replaced. Control: the same directory with a file name
/// below it is written.
#[cfg(unix)]
#[test]
fn an_output_path_ending_in_a_separator_is_refused_as_a_directory() {
    let dir = scratch();
    let root = dir.path();
    put(root, "page.mds", "Hello\n");
    put(root, "other.md", "Other\n");

    for typed in ["newdir/", "newdir/.", "other.md/"] {
        let out = run(root, &["build", "page.mds", "-o", typed]);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{typed}: stderr: {stderr}");
        let refusal = format!("cannot write {}: is a directory", native(typed));
        assert!(
            stderr.contains("mds::io") && squash(&stderr).contains(&squash(&refusal)),
            "{typed}: refused as a directory; stderr: {stderr}"
        );
    }
    assert!(
        !root.join("newdir").exists(),
        "no file `newdir` was created"
    );
    assert_eq!(
        read(&root.join("other.md")),
        "Other\n",
        "other.md was left alone"
    );
    // The entry itself, so spelled, is refused before any write is attempted.
    let out = run(root, &["build", "page.mds", "-o", "page.mds/"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", text(&out.stderr));
    assert_eq!(
        read(&root.join("page.mds")),
        "Hello\n",
        "the entry was left alone"
    );

    // Control: the same route writes a file below that directory.
    let out = run(root, &["build", "page.mds", "-o", "newdir/page.md"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(read(&root.join(native("newdir/page.md"))), "Hello\n");
}

/// A FIFO at the output path is no file a write replaces: it is refused (`mds::io`, exit
/// 2) without being opened — the run does not block on it — and left in place. Control:
/// a regular file at the same path is replaced.
#[cfg(unix)]
#[test]
fn a_fifo_at_the_output_path_is_refused_and_left_in_place() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::FileTypeExt as _;

    let dir = scratch();
    let root = dir.path();
    put(root, "page.mds", "Hello\n");
    let target = root.join("page.md");
    let name = std::ffi::CString::new(target.as_os_str().as_bytes()).expect("no NUL in it");
    // SAFETY: `name` is a NUL-terminated path in this test's own scratch directory.
    assert_eq!(
        unsafe { libc::mkfifo(name.as_ptr(), 0o644) },
        0,
        "create a FIFO"
    );

    let mut child = common::ChildGuard(
        mds_bin()
            .current_dir(root)
            .args(["build", "page.mds", "-o", "page.md"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mds"),
    );
    let tap = common::tap_reader(child.0.stderr.take().expect("a piped stderr"));
    // Bounded by TIMEOUT: a run that opened the FIFO would block until it is read.
    let deadline = Instant::now() + TIMEOUT;
    let code = loop {
        if let Some(status) = child.0.try_wait().expect("poll the run") {
            break status.code();
        }
        assert!(Instant::now() < deadline, "the run blocked on the FIFO");
        std::thread::sleep(Duration::from_millis(5));
    };
    let stderr = tap.finish_text(&mut child);
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("mds::io")
            && squash(&stderr).contains(&squash("cannot write page.md: not a regular file")),
        "stderr: {stderr}"
    );
    assert!(
        std::fs::symlink_metadata(&target)
            .expect("the FIFO is still there")
            .file_type()
            .is_fifo(),
        "the FIFO is left in place"
    );

    // Control: a regular file at the same path is replaced.
    std::fs::remove_file(&target).unwrap();
    std::fs::write(&target, "Old\n").unwrap();
    let out = run(root, &["build", "page.mds", "-o", "page.md"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(read(&target), "Hello\n");
}

// ── `build.output_dir`: a directory the repository names ─────────────────────

/// `mds.json`'s `build.output_dir` is the repository's, not a path the user typed: it is
/// written below the directory `mds.json` is in, its own directories included, so a
/// symlink committed at `dist` is refused — `mds build` of a file and of a directory, and
/// `mds watch` of a directory — and the directory it points at is left alone. The
/// refusal names the link below the directory `mds.json` was reached by, as an output is.
#[cfg(unix)]
#[test]
fn a_symlink_committed_as_build_output_dir_is_refused() {
    let dir = scratch();
    let root = dir.path();
    put(root, "mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(root, "src/a.mds", "A\n");
    std::fs::create_dir(root.join("victim")).unwrap();
    std::os::unix::fs::symlink("victim", root.join("dist")).unwrap();

    for args in [&["build", "src/a.mds"][..], &["build", "src"]] {
        let out = run(root, args);
        let stderr = text(&out.stderr);
        assert_eq!(
            entries(&root.join("victim")),
            Vec::<String>::new(),
            "{args:?}: nothing is written through the symlink; stderr: {stderr}"
        );
        assert_eq!(out.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains("mds::io")
                && squash(&stderr).contains(&squash(&refused("src/../dist"))),
            "{args:?}: the refusal names the link; stderr: {stderr}"
        );
    }

    let (child, tap, _) = common::spawn_watch_ready(
        mds_bin()
            .current_dir(root)
            .args(["watch", "src", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let mut child = common::ChildGuard(child);
    let stderr = tap.finish_text(&mut child);
    assert_eq!(
        entries(&root.join("victim")),
        Vec::<String>::new(),
        "watch: nothing is written through the symlink; stderr: {stderr}"
    );
    assert!(
        squash(&stderr).contains(&squash(&refused("src/../dist"))),
        "watch: the refusal names the link; stderr: {stderr}"
    );
    assert!(
        std::fs::symlink_metadata(root.join("dist"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is left in place"
    );
}

/// An absolute `build.output_dir` is refused before anything is written (`mds::io`, exit
/// 2), naming `mds.json` and the value as written, in file and directory mode: the
/// repository does not choose where outside it a build writes. Control: a relative one is
/// written below the directory `mds.json` is in, its directories created.
#[test]
fn a_build_output_dir_must_be_relative() {
    let dir = scratch();
    let root = dir.path();
    let project = root.join("proj");
    let victim = root.join("victim");
    std::fs::create_dir(&victim).unwrap();
    let absolute = victim.to_str().expect("a UTF-8 scratch path").to_owned();
    let config = serde_json::json!({ "build": { "output_dir": absolute } }).to_string();
    put(&project, "mds.json", &config);
    put(&project, "src/a.mds", "A\n");

    let refusal = format!("mds.json output_dir '{absolute}' must be a relative path");
    for args in [&["build", "src/a.mds"][..], &["build", "src"]] {
        let out = run(&project, args);
        let stderr = text(&out.stderr);
        assert_eq!(
            entries(&victim),
            Vec::<String>::new(),
            "{args:?}: nothing is written there; stderr: {stderr}"
        );
        assert_eq!(out.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains("mds::io") && squash(&stderr).contains(&squash(&refusal)),
            "{args:?}: stderr: {stderr}"
        );
    }

    // Control: a relative `build.output_dir`.
    put(
        &project,
        "mds.json",
        r#"{"build":{"output_dir":"out/nested"}}"#,
    );
    for args in [&["build", "src/a.mds"][..], &["build", "src"]] {
        let written = project.join(native("out/nested/a.md"));
        let _ = std::fs::remove_file(&written);
        let out = run(&project, args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: stderr: {}",
            text(&out.stderr)
        );
        assert_eq!(read(&written), "A\n", "{args:?}");
    }
}

/// `mds watch` refuses a symlink that replaced a directory below `--out-dir` when it
/// rebuilds into it. Control: the startup write landed in that directory before the swap.
#[cfg(unix)]
#[test]
fn watch_refuses_a_symlink_below_the_out_dir_on_a_rebuild() {
    let dir = scratch();
    let root = dir.path();
    put(root, "src/sub/a.mds", "First\n");
    std::fs::create_dir(root.join("victim")).unwrap();

    let (child, tap, _) = common::spawn_watch_ready(
        mds_bin()
            .current_dir(root)
            .args(["watch", "src", "--out-dir", "out", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let mut child = common::ChildGuard(child);
    assert_eq!(read(&root.join("out/sub/a.md")), "First\n");

    std::fs::remove_dir_all(root.join("out/sub")).unwrap();
    std::os::unix::fs::symlink("../victim", root.join("out/sub")).unwrap();
    common::write_atomic(&root.join("src/sub/a.mds"), "Second\n");
    let reported = common::poll_tap_until(&tap, TIMEOUT, |seen| {
        seen.contains("Recompiled") || seen.contains("symlink")
    });
    let stderr = tap.finish_text(&mut child);

    assert!(
        reported.is_ok(),
        "the rebuild reported nothing; stderr: {stderr}"
    );
    assert_eq!(
        entries(&root.join("victim")),
        Vec::<String>::new(),
        "nothing is written through the symlink; stderr: {stderr}"
    );
    assert!(
        squash(&stderr).contains(&squash(&refused("out/sub"))),
        "the refusal names the link as typed; stderr: {stderr}"
    );
}

// ── A removal below the anchor ──────────────────────────────────────────────

/// `mds watch` never removes a deleted source's output through a symlink that replaced a
/// directory below `--out-dir`: the removal is refused by the path the user knows the
/// output by, naming the link, and the file of that name in the directory the link points
/// at is left as it was — one holding exactly the bytes the session wrote, which is all a
/// removal otherwise asks of a file (#160). Controls: earlier in the same session, a
/// deleted source's output below a real directory is removed; and the link is left in
/// place.
#[cfg(unix)]
#[test]
fn watch_never_removes_an_output_through_a_symlink_below_the_out_dir() {
    const HAND: &str = "X\n";
    let dir = scratch();
    let root = dir.path();
    put(root, "src/top.mds", "Top\n");
    put(root, "src/sub/x.mds", "X\n");
    let victim = put(root, "victim/x.md", HAND);

    let (child, tap, _) = common::spawn_watch_ready(
        mds_bin()
            .current_dir(root)
            .args(["watch", "src", "--out-dir", "out", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let mut child = common::ChildGuard(child);
    assert_eq!(read(&root.join("out/sub/x.md")), "X\n");

    // Control: the output of a source deleted below a real directory is removed.
    std::fs::remove_file(root.join("src/top.mds")).unwrap();
    common::wait_for_tap(&tap, "top.md (source deleted)", TIMEOUT);
    assert!(
        !root.join("out/top.md").exists(),
        "control: the deleted source's output is removed"
    );

    std::fs::remove_dir_all(root.join("out/sub")).unwrap();
    std::os::unix::fs::symlink("../victim", root.join("out/sub")).unwrap();
    std::fs::remove_file(root.join("src/sub/x.mds")).unwrap();
    common::wait_for_tap(&tap, "could not remove", TIMEOUT);
    let stderr = tap.finish_text(&mut child);

    assert_eq!(
        read(&victim),
        HAND,
        "nothing is removed through the symlink; stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "warning: could not remove {}: refusing to follow a symlink at {}",
            native("out/sub/x.md"),
            native("out/sub")
        )),
        "the refusal names the output and the link as typed; stderr: {stderr}"
    );
    assert!(
        std::fs::symlink_metadata(root.join("out/sub"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link is left in place"
    );
}

/// `mds watch` never removes a deleted source's output through an out-dir replaced by a
/// symlink mid-session: the out-dir is checked first, as before a write, the removal is
/// refused as such a write is, naming the output as typed, and the file of that name
/// where the link leads is left as it was. Control: earlier in the same session, a
/// deleted source's output in the out-dir as it started is removed.
#[cfg(unix)]
#[test]
fn watch_never_removes_an_output_through_an_out_dir_replaced_by_a_symlink() {
    const HAND: &str = "Hand-written, not an mds output\n";
    let dir = scratch();
    let root = dir.path();
    put(root, "src/top.mds", "Top\n");
    put(root, "src/x.mds", "X\n");
    let victim = put(root, "victim/x.md", HAND);

    let (child, tap, _) = common::spawn_watch_ready(
        mds_bin()
            .current_dir(root)
            .args(["watch", "src", "--out-dir", "out", "--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let mut child = common::ChildGuard(child);
    assert_eq!(read(&root.join("out/x.md")), "X\n");

    // Control: the output of a source deleted while the out-dir is as it started is removed.
    std::fs::remove_file(root.join("src/top.mds")).unwrap();
    common::wait_for_tap(&tap, "top.md (source deleted)", TIMEOUT);
    assert!(
        !root.join("out/top.md").exists(),
        "control: the deleted source's output is removed"
    );

    std::fs::remove_dir_all(root.join("out")).unwrap();
    std::os::unix::fs::symlink("victim", root.join("out")).unwrap();
    std::fs::remove_file(root.join("src/x.mds")).unwrap();
    let reported = common::poll_tap_until(&tap, TIMEOUT, |seen| {
        seen.contains("x.md (source deleted)") || seen.contains("could not remove")
    });
    let stderr = tap.finish_text(&mut child);

    assert!(
        reported.is_ok(),
        "the deletion reported nothing; stderr: {stderr}"
    );
    assert_eq!(
        read(&victim),
        HAND,
        "nothing is removed through the symlink; stderr: {stderr}"
    );
    let refusal = format!(
        "warning: could not remove {}: the output directory now resolves to a different \
         directory; restart mds watch to follow it",
        native("out/x.md")
    );
    assert!(
        squash(&stderr).contains(&squash(&refusal)),
        "the refusal names the output as typed; stderr: {stderr}"
    );
}

// ── A directory swapped for a link while a rewrite runs ─────────────────────

/// A pipe filled to the brim, and its ends: a child given the write end as a stream
/// blocks on its first write to it, until the read end is drained.
#[cfg(unix)]
fn a_full_pipe() -> (std::io::PipeReader, std::io::PipeWriter) {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    let (reader, mut writer) = std::io::pipe().expect("create a pipe");
    let fd = writer.as_raw_fd();
    // SAFETY: `fd` is the open write end this function owns; F_GETFL/F_SETFL only read
    // and set its status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "read the pipe's flags");
    // SAFETY: as above.
    let set = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(set, 0, "make the pipe non-blocking");
    // Bounded: a pipe buffer holds at most a few hundred KiB, and each loop has a cap.
    let mut full = false;
    for chunk in [&[b'.'; 4096][..], b"."] {
        for _ in 0..(1 << 16) {
            match writer.write(chunk) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    full = true;
                    break;
                }
                Err(e) => panic!("fill the pipe: {e}"),
            }
        }
    }
    assert!(full, "the pipe filled");
    // SAFETY: as above; restores the flags read first, so the child blocks.
    let restored = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    assert_eq!(restored, 0, "make the pipe blocking again");
    (reader, writer)
}

/// Sources for a directory rewrite: the first file, the second one (in `sub/`), and the
/// file of the same name in the directory the link will point at — each as written, and
/// the first as rewritten.
#[cfg(unix)]
struct Rewrite {
    first: &'static str,
    first_rewritten: &'static str,
    second: &'static str,
    victim: &'static str,
}

/// Run `mds <args>` (a directory rewrite of `src`) with `src/sub` swapped for a symlink to
/// `victim` after the run has rewritten `src/a.mds` and before it reads `src/sub/b.mds`:
/// its stderr is a full pipe, so the status line it prints after the first rewrite holds
/// it there until the swap is done. Returns the scratch directory, the exit code and
/// stderr.
#[cfg(unix)]
fn rewrite_with_sub_swapped(
    args: &[&str],
    files: &Rewrite,
) -> (tempfile::TempDir, Option<i32>, String) {
    let dir = scratch();
    let root = dir.path();
    let first = put(root, "src/a.mds", files.first);
    let second = put(root, "src/sub/b.mds", files.second);
    put(root, "victim/b.mds", files.victim);

    let (reader, writer) = a_full_pipe();
    let child = mds_bin()
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(writer)
        .spawn()
        .expect("spawn mds");
    // The `Command` holding the parent's copy of the write end is gone, so the drain below
    // ends when the child exits.
    let mut child = common::ChildGuard(child);

    // Bounded by TIMEOUT.
    let deadline = Instant::now() + TIMEOUT;
    while std::fs::read_to_string(&first).ok().as_deref() != Some(files.first_rewritten) {
        if let Some(status) = child.0.try_wait().expect("poll the run") {
            panic!("setup: the run ended ({status}) before it rewrote src/a.mds");
        }
        assert!(
            Instant::now() < deadline,
            "setup: src/a.mds was not rewritten"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        child.0.try_wait().expect("poll the run").is_none(),
        "setup: the run is held on its status line"
    );
    assert_eq!(
        read(&second),
        files.second,
        "setup: src/sub/b.mds is not rewritten yet"
    );

    std::fs::rename(root.join("src/sub"), root.join("src/sub.moved")).unwrap();
    std::os::unix::fs::symlink("../victim", root.join("src/sub")).unwrap();
    let tap = common::tap_reader(reader);
    // Bounded by TIMEOUT.
    let deadline = Instant::now() + TIMEOUT;
    let code = loop {
        if let Some(status) = child.0.try_wait().expect("poll the run") {
            break status.code();
        }
        assert!(Instant::now() < deadline, "the run did not end");
        std::thread::sleep(Duration::from_millis(5));
    };
    let stderr = tap.finish_text(&mut child);
    let stderr = stderr.trim_start_matches('.').to_owned();
    (dir, code, stderr)
}

/// What a rewrite with `src/sub` swapped for a link must come to: refused by the path the
/// user knows the link by, nothing written through it, and nothing written into the
/// directory it replaced either.
#[cfg(unix)]
fn assert_refused_after_the_swap(dir: &Path, code: Option<i32>, stderr: &str, files: &Rewrite) {
    assert_eq!(
        read(&dir.join("victim/b.mds")),
        files.victim,
        "nothing is written through the symlink; stderr: {stderr}"
    );
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("mds::io") && squash(stderr).contains(&squash(&refused("src/sub"))),
        "the refusal names the link as typed; stderr: {stderr}"
    );
    assert_eq!(
        read(&dir.join("src/sub.moved/b.mds")),
        files.second,
        "stderr: {stderr}"
    );
}

/// `mds fmt <dir>` refuses to rewrite a file below a directory that was swapped for a
/// symlink after the walk found it. Control: the file before it was rewritten.
#[cfg(unix)]
#[test]
fn fmt_refuses_a_directory_swapped_for_a_symlink_before_its_rewrite() {
    let files = Rewrite {
        first: "Alpha\r\n",
        first_rewritten: "Alpha\n",
        second: "Beta\r\n",
        victim: "Victim\r\n",
    };
    let (dir, code, stderr) = rewrite_with_sub_swapped(&["fmt", "src"], &files);
    assert_refused_after_the_swap(dir.path(), code, &stderr, &files);
}

/// `mds lint --fix <dir>` refuses to rewrite a file below a directory that was swapped for
/// a symlink after the walk found it. Control: the file before it was fixed.
#[cfg(unix)]
#[test]
fn lint_fix_refuses_a_directory_swapped_for_a_symlink_before_its_rewrite() {
    let files = Rewrite {
        first: "@if \"x\" == \"y\":\nhidden\n@end\nAlpha\n",
        first_rewritten: "Alpha\n",
        second: "@if \"x\" == \"y\":\nhidden\n@end\nBeta\n",
        victim: "@if \"x\" == \"y\":\nhidden\n@end\nVictim\n",
    };
    let (dir, code, stderr) = rewrite_with_sub_swapped(&["lint", "--fix", "src"], &files);
    assert_refused_after_the_swap(dir.path(), code, &stderr, &files);
}

// ── A rewrite writes only over the bytes it read ────────────────────────────

/// The debug build's pause between a rewrite's read and its replace, and between `mds
/// init`'s look at its target and its commit: the file it names ends the pause, and the
/// same name with `.paused` appended says the run has stopped.
const PAUSE: &str = "MDS_TEST_PAUSE_BEFORE_REPLACE";

/// A source a rewrite changes, as written and as rewritten.
struct Source {
    written: &'static str,
    rewritten: &'static str,
}

/// What `mds fmt` rewrites, and how.
const UNFORMATTED: Source = Source {
    written: "Alpha\r\n",
    rewritten: "Alpha\n",
};

/// What `mds lint --fix` rewrites, and how.
const UNFIXED: Source = Source {
    written: "@if \"x\" == \"y\":\nhidden\n@end\nAlpha\n",
    rewritten: "Alpha\n",
};

/// Each rewrite of `src/a.mds`: `mds fmt` and `mds lint --fix`, given the file and given
/// its directory. A path in them is written with `/`.
const REWRITES: [(&[&str], &Source); 4] = [
    (&["fmt", "src/a.mds"], &UNFORMATTED),
    (&["fmt", "src"], &UNFORMATTED),
    (&["lint", "--fix", "src/a.mds"], &UNFIXED),
    (&["lint", "--fix", "src"], &UNFIXED),
];

/// Run `mds <args>` in `root`, paused at the debug build's pause ([`PAUSE`]): once it has
/// stopped, `meanwhile` runs, then the run goes on. Returns the exit code and stderr.
fn rewrite_paused(root: &Path, args: &[&str], meanwhile: impl FnOnce()) -> (Option<i32>, String) {
    let go = root.join("go");
    let paused = root.join("go.paused");
    for signal in [&go, &paused] {
        if signal.exists() {
            std::fs::remove_file(signal).expect("clear the last run's pause");
        }
    }
    let child = mds_bin()
        .current_dir(root)
        .args(args)
        .env(PAUSE, &go)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mds");
    let mut child = common::ChildGuard(child);
    let tap = common::tap_reader(child.0.stderr.take().expect("the run's stderr"));

    // Bounded by TIMEOUT.
    let deadline = Instant::now() + TIMEOUT;
    while !paused.exists() {
        if let Some(status) = child.0.try_wait().expect("poll the run") {
            panic!(
                "setup: {args:?} ended ({status}) before its rewrite paused; stderr: {}",
                tap.text()
            );
        }
        assert!(Instant::now() < deadline, "setup: {args:?} did not pause");
        std::thread::sleep(Duration::from_millis(5));
    }
    meanwhile();
    std::fs::write(&go, "").expect("let the run go on");

    // Bounded by TIMEOUT.
    let deadline = Instant::now() + TIMEOUT;
    let code = loop {
        if let Some(status) = child.0.try_wait().expect("poll the run") {
            break status.code();
        }
        assert!(Instant::now() < deadline, "{args:?} did not end");
        std::thread::sleep(Duration::from_millis(5));
    };
    (code, tap.finish_text(&mut child))
}

/// The refusal of a rewrite whose file changed after it was read, the file as typed.
fn changed(file: &str) -> String {
    format!(
        "\"{}\" changed since it was read; not written",
        native(file)
    )
}

/// A file edited between a rewrite's read and its replace is not overwritten: `mds fmt`
/// and `mds lint --fix`, given the file or its directory, refuse (`mds::io`, exit 2),
/// the edit survives, and no temporary file is left (#160). Control: with no edit in
/// that window, the same run rewrites the file.
///
/// Runs on every platform: on Windows the check is the stamp a rewrite takes by path —
/// the file's size and times — and each edit here changes the size. Each path argument is
/// typed in the platform's separator, so the file argument and the directory's entry are
/// both named as [`changed`] names them.
#[test]
fn a_rewrite_refuses_a_file_edited_after_it_was_read() {
    const EDIT: &str = "Edited by hand while the rewrite ran\n";
    for (args, source) in REWRITES {
        let typed: Vec<String> = args.iter().map(|arg| native(arg)).collect();
        let typed: Vec<&str> = typed.iter().map(String::as_str).collect();
        let args = typed.as_slice();
        let dir = scratch();
        let root = dir.path();
        let file = put(root, "src/a.mds", source.written);

        let (code, stderr) = rewrite_paused(root, args, || {
            std::fs::write(&file, EDIT).expect("edit the file");
        });
        assert_eq!(
            read(&file),
            EDIT,
            "{args:?}: the edit survives; stderr: {stderr}"
        );
        assert_eq!(code, Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains("mds::io") && squash(&stderr).contains(&squash(&changed("src/a.mds"))),
            "{args:?}: the refusal names the file as typed; stderr: {stderr}"
        );
        assert_eq!(
            entries(&root.join("src")),
            ["a.mds"],
            "{args:?}: no temporary file is left"
        );

        std::fs::write(&file, source.written).unwrap();
        let (code, stderr) = rewrite_paused(root, args, || {});
        assert_eq!(code, Some(0), "{args:?}: control; stderr: {stderr}");
        assert_eq!(
            read(&file),
            source.rewritten,
            "{args:?}: control rewrites it"
        );
    }
}

/// A directory swapped between a rewrite's read and its replace never makes the rewrite
/// write the file it read over another (#160): with `src` moved away and `other` put in
/// its place — as a directory, or as a symlink to it — `other/a.mds` is left as it was,
/// and the file read, in the directory it was read in, is the one rewritten (control).
#[cfg(unix)]
#[test]
fn a_rewrite_never_writes_over_a_file_it_did_not_read() {
    const OTHER: &str = "Bravo, another file of the same name\n";
    for link in [false, true] {
        for (args, source) in REWRITES {
            let dir = scratch();
            let root = dir.path();
            put(root, "src/a.mds", source.written);
            put(root, "other/a.mds", OTHER);

            let (code, stderr) = rewrite_paused(root, args, || {
                std::fs::rename(root.join("src"), root.join("src.moved")).unwrap();
                if link {
                    std::os::unix::fs::symlink("other", root.join("src")).unwrap();
                } else {
                    std::fs::rename(root.join("other"), root.join("src")).unwrap();
                }
            });
            let other = if link { "other/a.mds" } else { "src/a.mds" };
            assert_eq!(
                read(&root.join(other)),
                OTHER,
                "{args:?}, link {link}: the other file is not written; stderr: {stderr}"
            );
            assert_eq!(
                read(&root.join("src.moved/a.mds")),
                source.rewritten,
                "{args:?}, link {link}: the file read is rewritten; stderr: {stderr}"
            );
            assert_eq!(code, Some(0), "{args:?}, link {link}: stderr: {stderr}");
        }
    }
}

// ── `mds init` never replaces a file that appears after its check ───────────

/// A file that appears between `mds init`'s look at its target and the commit of the
/// starter file is not replaced (#160), and is refused exactly as a file already there
/// when `mds init` looks is: the same message, naming the file as typed, on the same
/// stream, with the same exit code (1) and no error code. The file keeps its bytes, and no
/// temporary file is left. Controls: a run that finds the file there at its look — the
/// refusal this one must match — names the file; with nothing put there in that window,
/// the same run creates the starter file; and `--force` replaces a file that is there.
#[test]
fn init_never_replaces_a_file_that_appears_after_its_check() {
    const APPEARED: &str = "Written by another program while init ran\n";
    let dir = scratch();
    let root = dir.path();
    std::fs::create_dir(root.join("sub")).expect("create the target's directory");
    let file = root.join("sub").join("new.mds");
    let typed = native("sub/new.mds");
    let args = ["init", typed.as_str()];

    let (code, stderr) = rewrite_paused(root, &args, || {
        std::fs::write(&file, APPEARED).expect("put a file where init writes");
    });
    assert_eq!(
        read(&file),
        APPEARED,
        "the file that appeared keeps its bytes; stderr: {stderr}"
    );
    assert_eq!(
        entries(&root.join("sub")),
        ["new.mds"],
        "no temporary file is left"
    );

    // The refusal of a file already there when init looks: the outcome to match.
    let found = run(root, &args);
    let found_stderr = text(&found.stderr);
    let refusal = format!("{typed} already exists (use --force to overwrite)");
    assert_eq!(
        found.status.code(),
        Some(1),
        "found: stderr: {found_stderr}"
    );
    assert!(
        squash(&found_stderr).contains(&squash(&refusal)) && !found_stderr.contains("mds::"),
        "found: the refusal names the file as typed, with no error code; stderr: \
         {found_stderr}"
    );
    assert!(found.stdout.is_empty(), "found: nothing on stdout");
    assert_eq!(read(&file), APPEARED, "found: the file keeps its bytes");

    assert_eq!(
        (code, stderr.as_str()),
        (Some(1), found_stderr.as_str()),
        "a file that appeared during the write is refused as one already there"
    );

    // Control: nothing appears in the window, and the starter file is created.
    std::fs::remove_file(&file).expect("remove the file that appeared");
    let (code, stderr) = rewrite_paused(root, &args, || {});
    assert_eq!(code, Some(0), "control; stderr: {stderr}");
    assert!(
        read(&file).contains("Hello {{name}}!"),
        "control: the starter file is created"
    );
    assert_eq!(entries(&root.join("sub")), ["new.mds"]);

    // Control: `--force` replaces a file that is there.
    std::fs::write(&file, APPEARED).expect("put a file where init writes");
    let out = run(root, &["init", typed.as_str(), "--force"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--force; stderr: {}",
        text(&out.stderr)
    );
    assert!(
        read(&file).contains("Hello {{name}}!"),
        "--force replaces the file"
    );
}

/// The lines of `stderr` that are a kept file's notice.
fn kept_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.starts_with("Kept "))
        .collect()
}

/// A file that appears at the output path of a watched file's new kind while `mds watch`
/// writes it there is never overwritten (#160): that write gives its file the name only
/// where nothing has it at the commit, so a `chat.json` put there after the session found
/// nothing is kept — with one notice, none under `--quiet` — and the old output `chat.md`
/// is kept as it was, since nothing replaced it. Control: with `chat.json` gone, the next
/// save writes it, and removes the `chat.md` the session wrote.
#[test]
fn watch_never_writes_over_a_file_that_appears_while_a_change_of_kind_is_written() {
    const APPEARED: &str = "Written by another program while watch ran\n";
    const MESSAGES: &str = "@message user:\nWhat is 3+3?\n@end\n";
    for quiet in [false, true] {
        let dir = scratch();
        let root = dir.path();
        let src = put(root, "src/chat.mds", "Hello\n");
        let (md, json) = (
            root.join(native("src/chat.md")),
            root.join(native("src/chat.json")),
        );
        let (go, paused) = (root.join("go"), root.join("go.paused"));
        let mut cmd = mds_bin();
        cmd.current_dir(root)
            .args(["watch", native("src/chat.mds").as_str()])
            .args(["--debounce", "0", "--poll-interval", "0"])
            .env(PAUSE, &go)
            .stdout(Stdio::null());
        if quiet {
            cmd.arg("-q");
        }
        let (child, tap, _) = common::spawn_watch_ready(&mut cmd);
        let mut child = common::ChildGuard(child);
        assert_eq!(
            read(&md),
            "Hello\n",
            "quiet {quiet}: control: the startup writes chat.md"
        );

        common::write_atomic(&src, MESSAGES);
        // Bounded by TIMEOUT.
        let deadline = Instant::now() + TIMEOUT;
        while !paused.exists() {
            assert!(
                Instant::now() < deadline,
                "quiet {quiet}: the write of chat.json did not pause; chat.json holds {:?}; \
                 stderr: {}",
                std::fs::read_to_string(&json).ok(),
                tap.text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::write(&json, APPEARED).expect("put a file where the write goes");
        std::fs::write(&go, "").expect("let the write go on");
        common::write_atomic(&src, common::ORDER_MARKER_SOURCE);
        let seen = common::wait_for_tap(&tap, common::ORDER_MARKER_LINE, TIMEOUT);

        assert_eq!(
            read(&json),
            APPEARED,
            "quiet {quiet}: the file that appeared keeps its bytes; stderr: {seen}"
        );
        assert_eq!(
            read(&md),
            "Hello\n",
            "quiet {quiet}: the old output is kept as it was; stderr: {seen}"
        );
        assert_eq!(
            entries(&root.join("src")),
            ["chat.json", "chat.md", "chat.mds"],
            "quiet {quiet}: no temporary file is left"
        );
        let notice = format!(
            "Kept {}: not written by this session; not overwritten",
            native("src/chat.json")
        );
        let expected: Vec<&str> = if quiet { vec![] } else { vec![notice.as_str()] };
        assert_eq!(kept_lines(&seen), expected, "quiet {quiet}: stderr: {seen}");

        // Control: nothing at chat.json, and the next save writes it.
        std::fs::remove_file(&json).expect("remove the file that appeared");
        common::write_atomic(&src, "@message user:\nWhat is 4+4?\n@end\n");
        let deadline = Instant::now() + TIMEOUT;
        while md.exists() || !std::fs::read_to_string(&json).is_ok_and(|t| t.contains("4+4")) {
            assert!(
                Instant::now() < deadline,
                "quiet {quiet}: control: the next save writes chat.json and removes chat.md; \
                 stderr: {}",
                tap.text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let stderr = tap.finish_text(&mut child);
        assert_eq!(
            kept_lines(&stderr),
            expected,
            "quiet {quiet}: control: no other notice; stderr: {stderr}"
        );
    }
}

// ── A directory the user may write to but not list ──────────────────────────

/// Set `dir`'s mode to `mode`, and say whether it now refuses a listing: a run as root,
/// whom no mode refuses, cannot show what the mode does, and its test is skipped.
#[cfg(unix)]
fn unlistable(dir: &Path, mode: u32) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("set the mode");
    std::fs::read_dir(dir).is_err()
}

/// Make `dir` listable and writable again, so its scratch directory can be removed.
#[cfg(unix)]
fn restore(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
        .expect("restore the mode");
}

/// A directory the user may write to and search but not list — a drop box, mode `0o333`
/// — takes an output on Linux, as it did before the write walked below its anchor (#160):
/// there each directory on the way is opened for search alone. Other Unix systems open
/// each to read, which such a directory refuses: `cannot write box/x.md: Permission
/// denied (os error 13)`, `mds::io`, exit 2, and nothing written. Control: once the box
/// can be listed, the same build writes it everywhere.
#[cfg(unix)]
#[test]
fn a_directory_that_cannot_be_listed_takes_an_output_on_linux_alone() {
    let dir = scratch();
    let root = dir.path();
    put(root, "x.mds", "Hello\n");
    let drop_box = root.join("box");
    std::fs::create_dir(&drop_box).expect("create the box");
    if !unlistable(&drop_box, 0o333) {
        restore(&drop_box);
        eprintln!(
            "skipped: {} can be listed at mode 0o333 (running as root?)",
            drop_box.display()
        );
        return;
    }
    let out = run(root, &["build", "x.mds", "-o", "box/x.md"]);
    // Read by path, which the box's search permission allows.
    let landed = std::fs::read_to_string(drop_box.join("x.md")).ok();
    restore(&drop_box);
    let stderr = text(&out.stderr);

    if cfg!(target_os = "linux") {
        assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
        assert_eq!(landed.as_deref(), Some("Hello\n"), "the output is written");
        assert_eq!(
            entries(&drop_box),
            ["x.md"],
            "and no temporary file is left"
        );
    } else {
        assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
        assert!(
            stderr.contains("mds::io")
                && squash(&stderr).contains(&squash(&format!(
                    "cannot write {}: Permission denied (os error 13)",
                    native("box/x.md")
                ))),
            "refused, named as typed; stderr: {stderr}"
        );
        assert_eq!(landed, None, "nothing is written");
        assert_eq!(
            entries(&drop_box),
            Vec::<String>::new(),
            "no temporary file is left"
        );
    }

    std::fs::remove_file(drop_box.join("x.md")).ok();
    let out = run(root, &["build", "x.mds", "-o", "box/x.md"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "control; stderr: {}",
        text(&out.stderr)
    );
    assert_eq!(read(&drop_box.join("x.md")), "Hello\n", "control writes it");
}

/// A rewrite that lands in a directory it then cannot sync is reported as written (#160):
/// `src` made `-wx` while the rewrite paused before its rename, so `mds fmt`'s and `mds lint
/// --fix`'s directory sync cannot open it to read. The file holds the rewrite and no
/// temporary file is left; `mds::io`, `src/a.mds written, but its directory could not be
/// synced: Permission denied (os error 13)`, once, exit 2. Given the file, the status line
/// — `Formatted:`, `Fixed:` — comes first, as for any rewrite that lands. Given the
/// directory, which shows a file's status line only once its directory is synced, there is
/// none: the run counts the file as what it holds — formatted, clean — and exits 2 for the
/// failed sync. Control: the same run with `src` left alone syncs it, shows the status
/// line, exit 0.
#[cfg(unix)]
#[test]
fn a_rewrite_whose_directory_cannot_be_synced_is_reported_written() {
    for (args, source) in REWRITES {
        let dir = scratch();
        let root = dir.path();
        let file = put(root, "src/a.mds", source.written);
        let src = root.join("src");
        let mut listable = true;
        let (code, stderr) = rewrite_paused(root, args, || listable = !unlistable(&src, 0o333));
        // Read by path, which the directory's search permission allows.
        let landed = std::fs::read_to_string(&file).ok();
        restore(&src);
        if listable {
            eprintln!(
                "skipped: {} can be listed at mode 0o333 (running as root?)",
                src.display()
            );
            return;
        }

        assert_eq!(code, Some(2), "{args:?}: stderr: {stderr}");
        assert_eq!(
            landed.as_deref(),
            Some(source.rewritten),
            "{args:?}: the rewrite landed"
        );
        assert_eq!(
            entries(&src),
            ["a.mds"],
            "{args:?}: no temporary file is left"
        );
        let status = match args[0] {
            "fmt" => format!("Formatted: {}", native("src/a.mds")),
            _ => format!("Fixed: {}", native("src/a.mds")),
        };
        let not_synced = format!(
            "{} written, but its directory could not be synced: Permission denied (os error 13)",
            native("src/a.mds")
        );
        let squashed = squash(&stderr);
        let at = |needle: &str| squashed.find(&squash(needle));
        assert!(
            stderr.contains("mds::io") && squashed.matches(&squash(&not_synced)).count() == 1,
            "{args:?}: the failed sync is reported once, the file named as typed; stderr: \
             {stderr}"
        );
        assert!(
            !stderr.contains("cannot write"),
            "{args:?}: not a failed write; stderr: {stderr}"
        );
        if args.contains(&"src") {
            // A directory run shows a file's status line only once its directory is
            // synced: the failed sync is the file's one line.
            assert_eq!(
                at(&status),
                None,
                "{args:?}: no status line for a rewrite not synced; stderr: {stderr}"
            );
            let summary = match args[0] {
                "fmt" => "1 formatted, 0 unchanged, 0 failed",
                _ => "1 clean, 0 with warnings, 0 with errors, 0 resource-limited",
            };
            assert!(
                stderr.contains(summary),
                "{args:?}: counted as what it holds; stderr: {stderr}"
            );
        } else {
            assert!(
                matches!((at(&status), at(&not_synced)), (Some(s), Some(n)) if s < n),
                "{args:?}: a file argument's status line comes first; stderr: {stderr}"
            );
        }

        std::fs::write(&file, source.written).unwrap();
        let (code, stderr) = rewrite_paused(root, args, || {});
        assert_eq!(code, Some(0), "{args:?}: control; stderr: {stderr}");
        assert_eq!(
            read(&file),
            source.rewritten,
            "{args:?}: control rewrites it"
        );
        assert!(
            squash(&stderr).contains(&squash(&status)),
            "{args:?}: control shows the status line of a synced rewrite; stderr: {stderr}"
        );
    }
}

// ── Windows ──────────────────────────────────────────────────────────────────

/// Windows: a directory symlink, and a junction, below `--out-dir` are refused as the
/// name-surrogate reparse points they are (#160), and nothing is written through either.
/// Control: the output beside the link, directly in the out-dir, is written.
#[cfg(windows)]
#[test]
fn a_directory_symlink_or_junction_below_the_out_dir_is_refused() {
    for junction in [false, true] {
        let dir = scratch();
        let root = dir.path();
        put(root, "src/sub/a.mds", "A\n");
        put(root, "src/top.mds", "Top\n");
        std::fs::create_dir(root.join("victim")).unwrap();
        std::fs::create_dir(root.join("out")).unwrap();
        let link = root.join("out").join("sub");
        if junction {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(&link)
                .arg(root.join("victim"))
                .stdout(Stdio::null())
                .status()
                .expect("run mklink /J");
            assert!(status.success(), "create a junction");
        } else if !common::make_symlink(&root.join("victim"), &link) {
            continue;
        }

        let out = run(root, &["build", "src", "--out-dir", "out"]);
        let stderr = text(&out.stderr);
        assert_eq!(
            entries(&root.join("victim")),
            Vec::<String>::new(),
            "junction {junction}: nothing is written through the link; stderr: {stderr}"
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "junction {junction}: stderr: {stderr}"
        );
        assert!(
            stderr.contains("mds::io") && squash(&stderr).contains(&squash(&refused("out/sub"))),
            "junction {junction}: the refusal names the link as typed; stderr: {stderr}"
        );
        assert_eq!(read(&root.join("out").join("top.md")), "Top\n");
    }
}

/// Windows: an output named in NTFS stream syntax is refused before anything is written
/// (#425): `lib.mds::$DATA` is the file `lib.mds` itself and `lib.md::$DATA` the file
/// `lib.md`, each an MDS module, which a write by that name would replace while its name
/// says it is neither. Each is refused, `mds::io`, exit 2, as an invalid file name; the
/// modules are left as they are, and no temporary file is left. Control: `-o out.md`
/// beside them is written.
#[cfg(windows)]
#[test]
fn an_output_named_in_stream_syntax_is_refused() {
    const MODULE: &str = "---\ntype: mds\n---\nLib\n";
    let dir = scratch();
    let root = dir.path();
    put(root, "page.mds", "Hello\n");
    put(root, "lib.mds", "Lib\n");
    put(root, "lib.md", MODULE);
    let invalid = std::io::Error::from(std::io::ErrorKind::InvalidFilename).to_string();

    for name in ["lib.mds::$DATA", "lib.md::$DATA"] {
        let out = run(root, &["build", "page.mds", "-o", name]);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{name}: stderr: {stderr}");
        assert!(
            stderr.contains("mds::io")
                && squash(&stderr).contains(&squash(&format!("cannot write {name}: {invalid}"))),
            "{name}: refused as an invalid file name; stderr: {stderr}"
        );
    }
    assert_eq!(read(&root.join("lib.mds")), "Lib\n");
    assert_eq!(read(&root.join("lib.md")), MODULE);
    assert_eq!(
        entries(root),
        ["lib.md", "lib.mds", "page.mds"],
        "nothing is written, and no temporary file is left"
    );

    let out = run(root, &["build", "page.mds", "-o", "out.md"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(read(&root.join("out.md")), "Hello\n");
}

// ── Probes (ignored: run with `--run-ignored only`) ─────────────────────────

/// The binary a probe runs: `MDS_PROBE_BIN` when set — another build of `mds` to compare
/// against — else this crate's own.
fn probe_bin() -> (PathBuf, bool) {
    match std::env::var_os("MDS_PROBE_BIN") {
        Some(bin) => (PathBuf::from(bin), true),
        None => (PathBuf::from(env!("CARGO_BIN_EXE_mds")), false),
    }
}

/// Startup time of `mds watch` over a 500-file directory with `--out-dir`, from spawn to
/// readiness, over seven sessions: prints the median, minimum and maximum. The measure
/// behind keeping the anchored write within the watch-startup budget (#160).
#[test]
#[ignore = "benchmark: run with --run-ignored only --no-capture"]
fn probe_a_500_file_directory_watch_startup() {
    let (bin, _) = probe_bin();
    let dir = scratch();
    let root = dir.path();
    for i in 0..500 {
        put(
            root,
            &format!("src/d{}/f{i}.mds", i % 10),
            &format!("Page {i}\n"),
        );
    }
    let mut times: Vec<Duration> = Vec::new();
    for _ in 0..7 {
        let out = root.join("out");
        if out.exists() {
            std::fs::remove_dir_all(&out).unwrap();
        }
        let mut cmd = std::process::Command::new(&bin);
        cmd.env("NO_COLOR", "1")
            .current_dir(root)
            .args(["watch", "src", "--out-dir", "out", "--quiet"])
            .stdout(Stdio::null());
        let started = Instant::now();
        let (child, tap, _) = common::spawn_watch_ready(&mut cmd);
        times.push(started.elapsed());
        let mut child = common::ChildGuard(child);
        let stderr = tap.finish_text(&mut child);
        assert!(root.join("out/d9/f499.md").is_file(), "stderr: {stderr}");
    }
    times.sort();
    eprintln!(
        "500-file watch startup ({}): median {:?}, min {:?}, max {:?}, all {times:?}",
        bin.display(),
        times[times.len() / 2],
        times[0],
        times[times.len() - 1]
    );
}

/// A black-box swap loop: while a thread swaps `out/sub` between a directory and a
/// symlink to `victim`, `mds build src --out-dir out` runs repeatedly, and every run whose
/// output landed in `victim` is counted. Prints the count; for this crate's own binary
/// there must be none (#160).
#[cfg(unix)]
#[test]
#[ignore = "probe: run with --run-ignored only --no-capture"]
fn probe_a_swap_loop_below_the_out_dir() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// How long each state — the directory, the link — is held: long enough that a run's
    /// whole write can fall inside one.
    const HOLD: Duration = Duration::from_micros(500);

    let (bin, other) = probe_bin();
    let dir = scratch();
    let root = dir.path().to_path_buf();
    put(&root, "src/sub/a.mds", "A\n");
    std::fs::create_dir_all(root.join("out/sub")).unwrap();
    std::fs::create_dir(root.join("victim")).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = Arc::clone(&stop);
        let out = root.join("out");
        std::thread::spawn(move || {
            let sub = out.join("sub");
            let mut swaps = 0u32;
            // Bounded: at most 200,000 swaps, and the loop ends when the runs do.
            for i in 0..200_000u32 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = std::fs::rename(&sub, out.join(format!("away-{i}")));
                if std::os::unix::fs::symlink("../victim", &sub).is_ok() {
                    swaps += 1;
                }
                std::thread::sleep(HOLD);
                if std::fs::remove_file(&sub).is_err() {
                    let _ = std::fs::rename(&sub, out.join(format!("away-{i}-b")));
                }
                let _ = std::fs::create_dir(&sub);
                std::thread::sleep(HOLD);
            }
            swaps
        })
    };

    let (mut runs, mut landed, mut refused_runs, mut failed) = (0u32, 0u32, 0u32, 0u32);
    let deadline = Instant::now() + Duration::from_secs(30);
    // Bounded: 300 runs or 30 s, whichever comes first.
    while runs < 300 && Instant::now() < deadline {
        let out = std::process::Command::new(&bin)
            .env("NO_COLOR", "1")
            .current_dir(&root)
            .args(["build", "src", "--out-dir", "out"])
            .stdin(Stdio::null())
            .output()
            .expect("run mds");
        runs += 1;
        if !out.status.success() {
            failed += 1;
        }
        if text(&out.stderr).contains("refusing to follow a symlink") {
            refused_runs += 1;
        }
        let through = root.join("victim/a.md");
        if through.exists() {
            landed += 1;
            std::fs::remove_file(&through).unwrap();
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().expect("the swapper does not panic");
    eprintln!(
        "swap loop ({}): {landed} of {runs} runs landed in the link's target; \
         {failed} failed, {refused_runs} of them refusing the link; {swaps} swaps",
        bin.display()
    );
    assert!(swaps > 0 && runs > 0, "the probe ran");
    if !other {
        assert_eq!(landed, 0, "no run writes through the link");
    }
}
