//! #265: the CLI refuses paths carrying a forbidden path character at input.
//!
//! - The directory walker keeps collecting: a hostile-named file fails on its own
//!   (`mds::io`, the name shown escaped) while its siblings are processed, under every
//!   directory-mode subcommand — `build`, `check`, `fmt`, `lint` and `watch` — each with
//!   its existing per-file-failure exit code, and `watch` keeps running.
//! - `-o`/`--output`, `--out-dir` and `mds.json` `build.output_dir` are refused UP FRONT
//!   (`mds::io`, exit 2), before any input is read.
//! - A `..` component in `build.output_dir` is `mds::io`, exit 2.
//!
//! PF-018: every hostile character is built at runtime and every six-character escape
//! text with `format!`, so this file holds no live control byte.

mod common;
use common::{assert_no_control_chars, mds_bin};

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

const ESC: char = '\x1b';

/// The six-character escape text for `ch` (uppercase hex).
fn escaped(ch: char) -> String {
    format!("\\u{:04X}", u32::from(ch))
}

/// Run `mds` with `args` in `dir` and return `(exit code, stdout + stderr)`.
///
/// Bounded: a command still running after 20 s — a `watch` expected to refuse at
/// startup that started watching instead — is killed and fails the test rather than
/// hanging the suite.
fn run(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let mut child = mds_bin()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    // Bounded: at most 20 s / 10 ms iterations.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("mds {args:?} was still running after 20 s");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout.join().unwrap()),
        String::from_utf8_lossy(&stderr.join().unwrap())
    );
    (status.code(), text)
}

/// Assert `text` carries a `mds::io` refusal naming `ch` and showing `shown_escaped`.
fn assert_refusal(text: &str, ch: char, shown_escaped: &str, label: &str) {
    assert!(text.contains("mds::io"), "{label}: mds::io; got: {text}");
    assert!(
        text.contains(&format!("U+{:04X}", u32::from(ch))) && text.contains("forbidden"),
        "{label}: must name U+{:04X}; got: {text}",
        u32::from(ch)
    );
    assert!(
        text.contains(shown_escaped),
        "{label}: must show {shown_escaped:?}; got: {text}"
    );
    assert_no_control_chars(text, label);
}

/// `s` without whitespace or miette's `│` frame marker, so a message miette wrapped
/// (at a space or after a `/`) compares equal to the unwrapped one.
#[cfg(unix)]
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{2502}')
        .collect()
}

// ── Walker matrix ───────────────────────────────────────────────────────────
//
// Unix-only: a Windows file name cannot hold a C0 control, so the hostile file the
// matrix needs cannot be created there.

#[cfg(unix)]
mod walker {
    use super::*;
    use common::{make_symlink, spawn_watch_ready, write_atomic, ChildGuard};

    fn is_line(text: &str, line: &str) -> bool {
        text.lines().any(|l| l.trim() == line)
    }

    /// A tree holding `ok.mds` and `evil<ESC>[31m.mds`. Returns the guard and the
    /// escaped form of the hostile name, as every refusal shows it.
    fn tree() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.mds"), "Hello!\n").unwrap();
        std::fs::write(dir.path().join(format!("evil{ESC}[31m.mds")), "Evil!\n").unwrap();
        (dir, format!("evil{}[31m.mds", escaped(ESC)))
    }

    #[test]
    fn build_refuses_the_hostile_file_and_builds_its_sibling() {
        let (dir, shown) = tree();
        let (code, text) = run(dir.path(), &["build", ".", "--out-dir", "out"]);
        assert_eq!(code, Some(1), "per-file failure exits 1; got: {text}");
        assert_refusal(&text, ESC, &shown, "build");
        assert!(is_line(&text, "1 built, 1 failed"), "got: {text}");
        let out = dir.path().join("out");
        assert_eq!(
            std::fs::read_to_string(out.join("ok.md")).unwrap(),
            "Hello!\n"
        );
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "only ok.md");
    }

    /// A file under a hostile-named DIRECTORY fails the same way: its entry path
    /// carries the character.
    #[test]
    fn build_refuses_a_file_under_a_hostile_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.mds"), "Hello!\n").unwrap();
        let hostile = dir.path().join(format!("sub{ESC}dir"));
        std::fs::create_dir(&hostile).unwrap();
        std::fs::write(hostile.join("a.mds"), "A\n").unwrap();
        let (code, text) = run(dir.path(), &["build", ".", "--out-dir", "out"]);
        assert_eq!(code, Some(1), "got: {text}");
        assert_refusal(
            &text,
            ESC,
            &format!("./sub{}dir/a.mds", escaped(ESC)),
            "build dir",
        );
        assert!(is_line(&text, "1 built, 1 failed"), "got: {text}");
        assert!(dir.path().join("out").join("ok.md").is_file());
    }

    #[test]
    fn check_refuses_the_hostile_file_and_checks_its_sibling() {
        let (dir, shown) = tree();
        let (code, text) = run(dir.path(), &["check", "."]);
        assert_eq!(code, Some(1), "got: {text}");
        assert_refusal(&text, ESC, &shown, "check");
        assert!(is_line(&text, "1 passed, 1 failed"), "got: {text}");
    }

    #[test]
    fn fmt_refuses_the_hostile_file_and_formats_its_sibling() {
        let (dir, shown) = tree();
        // The sibling needs a rewrite, so "formatted" proves it was processed.
        std::fs::write(dir.path().join("ok.mds"), "Hello!").unwrap();
        let (code, text) = run(dir.path(), &["fmt", "."]);
        assert_eq!(code, Some(1), "got: {text}");
        assert_refusal(&text, ESC, &shown, "fmt");
        assert!(
            is_line(&text, "1 formatted, 0 unchanged, 1 failed"),
            "got: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ok.mds")).unwrap(),
            "Hello!\n"
        );
    }

    #[test]
    fn lint_refuses_the_hostile_file_and_lints_its_sibling() {
        let (dir, shown) = tree();
        let (code, text) = run(dir.path(), &["lint", "."]);
        assert_eq!(
            code,
            Some(2),
            "a refused file is an error-severity file; got: {text}"
        );
        assert_refusal(&text, ESC, &shown, "lint");
        assert!(
            is_line(
                &text,
                "1 clean, 0 with warnings, 1 with errors, 0 resource-limited"
            ),
            "got: {text}"
        );
    }

    /// Watch starts, refuses the hostile file, builds the sibling, and keeps running:
    /// an edit to the sibling after the refusal is still rebuilt.
    #[test]
    fn watch_refuses_the_hostile_file_and_keeps_running() {
        let (dir, shown) = tree();
        let out = dir.path().join("out");
        let (child, tap, _) = spawn_watch_ready(
            mds_bin()
                .arg("watch")
                .arg(dir.path())
                .arg("--out-dir")
                .arg(&out)
                .args(["--debounce", "0"])
                .stdout(Stdio::null()),
        );
        let mut child = ChildGuard(child);

        let wait_for = |path: &Path, needle: &str| {
            // Bounded: 2 s at a 20 ms poll.
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if std::fs::read_to_string(path).is_ok_and(|c| c.contains(needle)) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(
            wait_for(&out.join("ok.md"), "Hello!"),
            "sibling built at startup"
        );

        write_atomic(&dir.path().join("ok.mds"), "Edited!\n");
        assert!(
            wait_for(&out.join("ok.md"), "Edited!"),
            "watch keeps running after the refusal; stderr: {}",
            tap.text()
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "watch must still be running"
        );

        let stderr = tap.finish_text(&mut child);
        assert_refusal(&stderr, ESC, &shown, "watch");
        assert!(
            !out.join(format!("evil{ESC}[31m.md")).exists(),
            "nothing is written for the refused file"
        );
    }

    /// `--vars` naming a hostile file reports the refusal, not the symlink message it
    /// used to be rewritten to; a symlinked `--vars` still gets the symlink message.
    #[test]
    fn watch_vars_errors_keep_their_real_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.mds"), "Hello!\n").unwrap();
        let vars = format!("v{ESC}.json");
        std::fs::write(dir.path().join(&vars), "{}").unwrap();
        let (code, text) = run(dir.path(), &["watch", "in.mds", "--vars", vars.as_str()]);
        assert_eq!(code, Some(2), "got: {text}");
        assert_refusal(
            &text,
            ESC,
            &format!("v{}.json", escaped(ESC)),
            "watch --vars",
        );
        assert!(!text.contains("must not be a symlink"), "got: {text}");

        std::fs::write(dir.path().join("real.json"), "{}").unwrap();
        assert!(make_symlink(
            &dir.path().join("real.json"),
            &dir.path().join("link.json")
        ));
        let (code, text) = run(dir.path(), &["watch", "in.mds", "--vars", "link.json"]);
        assert_eq!(code, Some(2), "got: {text}");
        assert!(
            text.contains("--vars file must not be a symlink: link.json"),
            "control: a symlink keeps the symlink message; got: {text}"
        );
    }
}

// ── Stdin: the working directory is shown as "." ────────────────────────────
//
// Unix-only: a Windows directory name cannot hold a C0 control such as TAB or ESC,
// so the hostile working directory cannot be created there.

#[cfg(unix)]
mod stdin_cwd {
    use super::*;
    use std::io::Write;

    /// Run `mds` with `args` in `dir`, feeding `input` on stdin; `(exit code, stdout +
    /// stderr)`. Bounded like [`run`]: killed after 20 s.
    fn run_stdin(dir: &Path, args: &[&str], input: &str) -> (Option<i32>, String) {
        let mut child = mds_bin()
            .current_dir(dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        // Bounded: at most 20 s / 10 ms iterations.
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("mds {args:?} was still running after 20 s");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code(), text)
    }

    /// `-` compiles against the working directory, a path the caller never typed. A
    /// working directory whose name carries a forbidden character is refused under
    /// `build`, `check`, `lint` and `fmt` alike (`mds::io`, exit 2) as the resolved
    /// form of `"."`, and the message names it `"."` — never its absolute path, never a
    /// raw character.
    #[test]
    fn stdin_refuses_a_hostile_working_directory_shown_as_dot() {
        let tmp = tempfile::tempdir().unwrap();
        // The temp directory's own name appears in its absolute path whatever the
        // symlinks above it resolve to (macOS `/var` → `/private/var`).
        let tmp_name = tmp.path().file_name().unwrap().to_str().unwrap().to_owned();
        for ch in ['\t', ESC] {
            let cwd = tmp.path().join(format!("d{ch}e"));
            std::fs::create_dir(&cwd).unwrap();
            for sub in ["build", "check", "lint", "fmt"] {
                let (code, text) = run_stdin(&cwd, &[sub, "-"], "Hello\n");
                let label = format!("{sub} - in d<U+{:04X}>e", u32::from(ch));
                assert_eq!(code, Some(2), "{label}: got: {text}");
                assert!(
                    text.contains(&format!(
                        "resolved path contains forbidden character U+{:04X}: \".\"",
                        u32::from(ch)
                    )),
                    "{label}: got: {text:?}"
                );
                assert_refusal(&text, ch, "\".\"", &label);
                assert!(!text.contains(ch), "{label}: raw char; got: {text:?}");
                assert!(
                    !text.contains(&tmp_name),
                    "{label}: absolute path; got: {text:?}"
                );
            }
        }

        // Control: the same input in a clean working directory succeeds.
        let clean = tmp.path().join("clean");
        std::fs::create_dir(&clean).unwrap();
        for sub in ["build", "check", "lint", "fmt"] {
            let (code, text) = run_stdin(&clean, &[sub, "-"], "Hello\n");
            assert_eq!(code, Some(0), "{sub} - control: got: {text}");
        }
    }
}

// ── Single-file inputs: refused before the existence check ──────────────────

/// A file argument carrying a forbidden character is refused (`mds::io`, exit 2, the
/// name escaped) whether or not the file exists and whatever its extension. `lint`
/// and `fmt` check that the argument names an existing `.mds` file before they read
/// it; the refusal must come first, or the `file not found` / `not an MDS file` error
/// shows the name with the raw character in it.
///
/// Portable: no hostile file is created — every path here is missing.
#[test]
fn single_file_argument_is_refused_before_the_existence_check() {
    let dir = tempfile::tempdir().unwrap();
    for ch in [ESC, '\t', '\n', '\u{202E}'] {
        for ext in ["mds", "txt"] {
            let value = format!("in{ch}x.{ext}");
            let shown = format!("in{}x.{ext}", escaped(ch));
            for sub in ["build", "check", "lint", "fmt", "watch"] {
                let (code, text) = run(dir.path(), &[sub, value.as_str()]);
                let label = format!("{sub} in<U+{:04X}>x.{ext}", u32::from(ch));
                assert_eq!(code, Some(2), "{label}: got: {text}");
                assert_refusal(&text, ch, &shown, &label);
                assert!(!text.contains(&value), "{label}: raw name; got: {text}");
                assert!(
                    !text.contains("not found") && !text.contains("not an MDS file"),
                    "{label}: the refusal comes first; got: {text}"
                );
            }

            // The JSON envelope carries the same refusal.
            let (code, text) = run(dir.path(), &["lint", "--format", "json", value.as_str()]);
            let label = format!("lint --format json in<U+{:04X}>x.{ext}", u32::from(ch));
            assert_eq!(code, Some(2), "{label}: got: {text}");
            let json: serde_json::Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{label}: stdout must be JSON ({e}); got: {text}"));
            assert_eq!(json["error"]["code"], "mds::io", "{label}: got: {text}");
            let message = json["error"]["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(&format!(
                    "contains forbidden character U+{:04X}",
                    u32::from(ch)
                )) && message.contains(&shown),
                "{label}: got: {message:?}"
            );
            assert!(!message.contains(ch), "{label}: raw char; got: {message:?}");
        }
    }

    // Control: a clean missing file still reports `file not found`, so the refusal
    // above is not every missing path's error.
    for sub in ["lint", "fmt"] {
        let (code, text) = run(dir.path(), &[sub, "missing.mds"]);
        assert_eq!(code, Some(2), "{sub} control: got: {text}");
        assert!(
            text.contains("mds::file_not_found") && text.contains("missing.mds"),
            "{sub} control: got: {text}"
        );
    }
}

// ── Output locations: refused up front ──────────────────────────────────────

/// `-o` and `--out-dir` values carrying a forbidden character are refused before the
/// input is even looked at: the input here does not exist, and the flag error — not
/// "file not found" — is what reports.
#[test]
fn output_flags_are_refused_up_front() {
    let dir = tempfile::tempdir().unwrap();
    for ch in [ESC, '\t', '\n', '\u{202E}'] {
        for (sub, flag, what) in [
            ("build", "-o", "-o/--output"),
            ("build", "--out-dir", "--out-dir"),
            ("watch", "-o", "-o/--output"),
            ("watch", "--out-dir", "--out-dir"),
        ] {
            let value = format!("out{ch}x");
            let (code, text) = run(dir.path(), &[sub, "missing.mds", flag, value.as_str()]);
            let label = format!("{sub} {flag} U+{:04X}", u32::from(ch));
            assert_eq!(code, Some(2), "{label}: got: {text}");
            assert!(
                text.contains(&format!(
                    "{what} contains forbidden character U+{:04X}",
                    u32::from(ch)
                )),
                "{label}: got: {text}"
            );
            assert_refusal(&text, ch, &format!("out{}x", escaped(ch)), &label);
            assert!(
                !text.contains("not found"),
                "{label}: flag first; got: {text}"
            );
        }
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing is created"
    );
}

/// Control: clean output locations work.
#[test]
fn clean_output_locations_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mds"), "Hi\n").unwrap();
    let (code, text) = run(dir.path(), &["build", "in.mds", "-o", "o.md"]);
    assert_eq!(code, Some(0), "got: {text}");
    let (code, text) = run(dir.path(), &["build", "in.mds", "--out-dir", "out"]);
    assert_eq!(code, Some(0), "got: {text}");
    assert!(dir.path().join("o.md").is_file());
    assert!(dir.path().join("out").join("in.md").is_file());
}

fn write_mds_json(dir: &Path, output_dir: &str) {
    let json = serde_json::json!({ "build": { "output_dir": output_dir } });
    std::fs::write(dir.join("mds.json"), json.to_string()).unwrap();
}

// ── Output locations: refused by their resolved form ────────────────────────
//
// Unix-only: a Windows directory name cannot hold TAB, so the hostile directory a
// symlinked output location leads into cannot be created there.

#[cfg(unix)]
mod resolved_output {
    use super::*;
    use common::make_symlink;

    /// `in.mds`, `src/a.mds`, a hostile-named directory `x<TAB>y` with `link` → it,
    /// and a clean directory `clean` with `clean_link` → it. Returns the guard and the
    /// temp directory's own name, which any absolute path in a message would carry.
    fn tree() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.mds"), "Hi\n").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("a.mds"), "A\n").unwrap();
        let hostile = dir.path().join("x\ty");
        std::fs::create_dir(&hostile).unwrap();
        assert!(make_symlink(&hostile, &dir.path().join("link")));
        std::fs::create_dir(dir.path().join("clean")).unwrap();
        assert!(make_symlink(
            &dir.path().join("clean"),
            &dir.path().join("clean_link")
        ));
        let name = dir.path().file_name().unwrap().to_str().unwrap().to_owned();
        (dir, name)
    }

    /// Assert `text` is the resolved-path refusal of `what` = `shown`: `mds::io`, the
    /// codepoint, the value as typed, no raw TAB (nor any other hostile character) and
    /// no absolute path in the refusal.
    fn assert_resolved_refusal(text: &str, what: &str, shown: &str, tmp_name: &str, label: &str) {
        let expected =
            format!("{what} resolved path contains forbidden character U+0009: \"{shown}\"");
        assert!(
            squash(text).contains(&squash(&expected)),
            "{label}: expected {expected:?}; got: {text:?}"
        );
        assert_no_control_chars(text, label);
        assert!(!text.contains('\t'), "{label}: raw TAB; got: {text:?}");
        // The refusal itself: `watch` announces its (canonical) entry before it.
        let refusal = &text[text
            .find("mds::io")
            .unwrap_or_else(|| panic!("{label}: mds::io; got: {text:?}"))..];
        assert!(
            !refusal.contains(tmp_name),
            "{label}: absolute path; got: {text:?}"
        );
    }

    /// `-o` and `--out-dir` pass the typed check but lead, through a symlink, into a
    /// hostile-named directory: refused by their resolved form (`mds::io`, exit 2)
    /// before anything is written — including a location below the link that does not
    /// exist yet — under `build` and `watch`, single-file and directory mode.
    #[test]
    fn output_flags_resolving_into_a_hostile_directory_are_refused() {
        let (dir, tmp_name) = tree();
        for (args, what, shown) in [
            (
                &["build", "in.mds", "--out-dir", "link"][..],
                "--out-dir",
                "link",
            ),
            (&["build", "src", "--out-dir", "link"], "--out-dir", "link"),
            (
                &["build", "in.mds", "--out-dir", "link/new/sub"],
                "--out-dir",
                "link/new/sub",
            ),
            (
                &["build", "in.mds", "-o", "link/x.md"],
                "-o/--output",
                "link/x.md",
            ),
            (
                &["build", "-", "-o", "link/x.md"],
                "-o/--output",
                "link/x.md",
            ),
            (
                &["watch", "in.mds", "--out-dir", "link"],
                "--out-dir",
                "link",
            ),
            (&["watch", "src", "--out-dir", "link"], "--out-dir", "link"),
            (
                &["watch", "in.mds", "-o", "link/x.md"],
                "-o/--output",
                "link/x.md",
            ),
        ] {
            let label = args.join(" ");
            let (code, text) = run(dir.path(), args);
            assert_eq!(code, Some(2), "{label}: got: {text}");
            assert_resolved_refusal(&text, what, shown, &tmp_name, &label);
        }
        assert_eq!(
            std::fs::read_dir(dir.path().join("x\ty")).unwrap().count(),
            0,
            "nothing is written into the hostile directory"
        );

        // Control: the same shapes through a symlink to a clean directory build.
        for (args, written) in [
            (&["build", "in.mds", "--out-dir", "clean_link"][..], "in.md"),
            (&["build", "src", "--out-dir", "clean_link"], "a.md"),
            (&["build", "in.mds", "-o", "clean_link/x.md"], "x.md"),
        ] {
            let (code, text) = run(dir.path(), args);
            assert_eq!(code, Some(0), "{args:?} control: got: {text}");
            assert!(dir.path().join("clean").join(written).is_file(), "{args:?}");
        }
    }

    /// `mds.json` `build.output_dir` naming a symlink into a hostile-named directory is
    /// refused where the config is loaded, like the typed check.
    #[test]
    fn output_dir_in_mds_json_resolving_into_a_hostile_directory_is_refused() {
        let (dir, tmp_name) = tree();
        write_mds_json(dir.path(), "link");
        for args in [
            &["build", "in.mds"][..],
            &["build", "src"],
            &["lint", "in.mds"],
            &["watch", "in.mds"],
        ] {
            let label = args.join(" ");
            let (code, text) = run(dir.path(), args);
            assert_eq!(code, Some(2), "{label}: got: {text}");
            assert_resolved_refusal(
                &text,
                "mds.json build.output_dir",
                "link",
                &tmp_name,
                &label,
            );
        }
        assert_eq!(
            std::fs::read_dir(dir.path().join("x\ty")).unwrap().count(),
            0,
            "nothing is written into the hostile directory"
        );

        // Control: a symlink to a clean directory works.
        write_mds_json(dir.path(), "clean_link");
        let (code, text) = run(dir.path(), &["build", "in.mds"]);
        assert_eq!(code, Some(0), "got: {text}");
        assert!(dir.path().join("clean").join("in.md").is_file());
    }
}

/// `mds.json` `build.output_dir` carrying a forbidden character is refused where the
/// config is loaded. `load_config` is shared, so every subcommand that loads `mds.json`
/// fails — including the ones that write no output (`lint`, `fmt` directory mode) —
/// while `check`, which does not load `mds.json`, is unaffected.
#[test]
fn output_dir_in_mds_json_is_refused_at_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mds"), "Hi\n").unwrap();
    let value = format!("out{ESC}[31m");
    write_mds_json(dir.path(), &value);
    let shown = format!("out{}[31m", escaped(ESC));

    for args in [
        &["build", "in.mds"][..],
        &["build", "."],
        &["lint", "in.mds"],
        &["lint", "."],
        &["fmt", "."],
        &["watch", "in.mds"],
    ] {
        let (code, text) = run(dir.path(), args);
        let label = args.join(" ");
        assert_eq!(code, Some(2), "{label}: got: {text}");
        assert!(
            text.contains("mds.json build.output_dir contains forbidden character U+001B"),
            "{label}: got: {text}"
        );
        assert_refusal(&text, ESC, &shown, &label);
    }
    let (code, text) = run(dir.path(), &["check", "in.mds"]);
    assert_eq!(code, Some(0), "check does not load mds.json; got: {text}");

    // Control: a clean output_dir works.
    write_mds_json(dir.path(), "out");
    let (code, text) = run(dir.path(), &["build", "in.mds"]);
    assert_eq!(code, Some(0), "got: {text}");
    assert!(dir.path().join("out").join("in.md").is_file());
}

/// A `..` component in `build.output_dir` is `mds::io`, exit 2 — in single-file and
/// directory mode (two resolvers, one shared check) — and the value is shown escaped.
#[test]
fn output_dir_traversal_is_io_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.mds"), "Hi\n").unwrap();
    write_mds_json(dir.path(), "../escaped");
    for args in [&["build", "in.mds"][..], &["build", "."]] {
        let (code, text) = run(dir.path(), args);
        let label = args.join(" ");
        assert_eq!(code, Some(2), "{label}: got: {text}");
        assert!(text.contains("mds::io"), "{label}: got: {text}");
        assert!(
            text.contains("mds.json output_dir '../escaped' must not contain '..' components"),
            "{label}: got: {text}"
        );
    }
    assert!(!dir.path().parent().unwrap().join("escaped").exists());
}

// ── mds.json load errors: the file as the input reaches it ──────────────────
//
// Unix-only: a Windows directory name cannot hold TAB, so the hostile working
// directory cannot be created there.

#[cfg(unix)]
mod config_errors {
    use super::*;

    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Every `mds.json` load error names the file by the path the input leads to it —
    /// `./mds.json`, `sub/../mds.json` — escaped, never by the canonical absolute path
    /// the upward walk uses. That path is what a hostile-named directory above the
    /// project would put a raw TAB into, before the input itself is validated.
    #[test]
    fn config_load_errors_name_the_file_as_reached_from_the_input() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_name = tmp.path().file_name().unwrap().to_str().unwrap().to_owned();
        let cwd = tmp.path().join("d\te");
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        std::fs::write(cwd.join("in.mds"), "Hi\n").unwrap();
        std::fs::write(cwd.join("sub").join("in.mds"), "Hi\n").unwrap();
        let config = cwd.join("mds.json");

        // (content, mode, expected message around the shown path)
        let mut cases: Vec<(Vec<u8>, u32, &str, &str)> = vec![
            (b"{".to_vec(), 0o644, "invalid mds.json at ", ":"),
            (
                vec![b' '; 1024 * 1024 + 1],
                0o644,
                "mds.json at ",
                " is too large",
            ),
            (vec![0xFF, 0xFE], 0o644, "invalid UTF-8 in ", ":"),
        ];
        // An unreadable mds.json — unless this process can read it anyway (root).
        std::fs::write(&config, "{}").unwrap();
        set_mode(&config, 0o000);
        if std::fs::read(&config).is_err() {
            cases.push((b"{}".to_vec(), 0o000, "cannot read ", ":"));
        }

        for (content, mode, before, after) in &cases {
            set_mode(&config, 0o644);
            std::fs::write(&config, content).unwrap();
            set_mode(&config, *mode);
            for (args, shown, exit) in [
                (&["build", "in.mds"][..], "./mds.json", 1),
                (&["build", "sub/in.mds"], "sub/../mds.json", 1),
                (&["lint", "in.mds"], "./mds.json", 2),
                (&["fmt", "."], "./mds.json", 1),
            ] {
                let label = format!("{} [{before}]", args.join(" "));
                let (code, text) = run(&cwd, args);
                assert_eq!(code, Some(exit), "{label}: got: {text:?}");
                let expected = format!("{before}{shown}{after}");
                assert!(
                    squash(&text).contains(&squash(&expected)),
                    "{label}: expected {expected:?}; got: {text:?}"
                );
                assert_no_control_chars(&text, &label);
                assert!(!text.contains('\t'), "{label}: raw TAB; got: {text:?}");
                assert!(
                    !text.contains(&tmp_name),
                    "{label}: absolute path; got: {text:?}"
                );
            }
        }
        set_mode(&config, 0o644);
    }
}

// ── mds init: refused up front ────────────────────────────────────────────

/// `mds init <filename>` refuses a forbidden character the same way `-o`/
/// `--out-dir`/`build.output_dir` do: `mds::io`, exit 2, before the starter
/// file is written.
#[test]
fn init_filename_is_refused_up_front() {
    let dir = tempfile::tempdir().unwrap();
    for ch in [ESC, '\t', '\n', '\u{202E}'] {
        let value = format!("out{ch}x");
        let (code, text) = run(dir.path(), &["init", value.as_str()]);
        let label = format!("init U+{:04X}", u32::from(ch));
        assert_eq!(code, Some(2), "{label}: got: {text}");
        assert!(
            text.contains(&format!(
                "init filename contains forbidden character U+{:04X}",
                u32::from(ch)
            )),
            "{label}: got: {text}"
        );
        assert_refusal(&text, ch, &format!("out{}x", escaped(ch)), &label);
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "nothing is created"
    );
}

/// Control: a clean filename still creates the starter file.
#[test]
fn init_clean_filename_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let (code, text) = run(dir.path(), &["init", "hello.mds"]);
    assert_eq!(code, Some(0), "got: {text}");
    assert!(dir.path().join("hello.mds").is_file());
}
