//! The lines of `mds build`, `mds check`, `mds fmt`, `mds lint`, `mds watch` and
//! `mds init` pinned here, each in the form spec.md §7.10 gives it (#390). Most name a
//! path as typed, or as the part below a directory the user named, as typed — the
//! directory argument, `--out-dir`, the directory `mds.json` was reached by, or the
//! entry's directory — never by a canonical or absolute spelling the user did not type;
//! `mds lint`'s findings name a file argument by its file name. The carve-outs are pinned
//! as they stand: the text of an error writing an output names the path the write was
//! given, the canonical one below a directory resolved to it, and tempfile's own cause
//! text an absolute one; and the file name of `mds watch`'s output beside its entry is the
//! one the volume holds, which differs from the name as typed for an entry typed in
//! another case on a case-insensitive volume. A directory `mds watch` watches for a
//! dependency outside the entry's directory and the directory argument has only the path
//! the compile reported (one below either is named below it as typed — only a refused
//! watch prints it, so watch.rs's unit tests pin that).
//!
//! spec.md §7.10's table cites, row by row, the tests that pin each line it lists;
//! [`the_spec_s_path_label_table_cites_exactly_these_tests`] keeps the table and
//! [`LABEL_TABLE`] in step and checks that each test cited is defined.
//!
//! Each run starts in a scratch directory with relative arguments, so the scratch
//! directory's own absolute path has no business in a line pinned as typed: [`leak`] looks
//! for it in every spelling it can take. Every absence check sits beside a check that the
//! line it is about was printed, so no test passes on a run that printed nothing.
//!
//! An expected path is written with `/` and printed through [`native`], so it names the
//! path in the platform's separator, exactly as `mds` prints it. A path argument is typed
//! through [`typed_args`] the same way: `mds` prints a typed path exactly as typed, so an
//! argument typed with `/` keeps it on Windows while the components `mds` joins to it
//! take `\`.

mod common;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::{mds_bin, write_atomic};

/// A scratch directory whose name no output could carry by chance.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("mds-path-labels-")
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

/// `args` as a user of this platform types them: each `/` in the platform's separator.
/// No flag or value in these tests other than a path carries a `/`.
fn typed_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| native(arg)).collect()
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

/// `mds <args>` in `cwd`, with `input` on stdin.
fn run_with_stdin(cwd: &Path, args: &[impl AsRef<OsStr>], input: &'static str) -> Output {
    let mut child = mds_bin()
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mds");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // A child that exits before reading all of stdin closes the pipe; that is not this
    // test's concern, so the write's result is ignored.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let output = child.wait_with_output().expect("wait for mds");
    writer.join().expect("the stdin writer does not panic");
    output
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Every spelling of `dir`'s absolute path a leak could take: as created and canonical;
/// each with and without macOS's `/private`, with and without a Windows verbatim prefix;
/// each of those JSON-escaped; and `dir`'s own name, which every one of them ends with.
fn spellings(dir: &Path) -> Vec<String> {
    let created = dir.to_string_lossy().into_owned();
    let canonical = dir
        .canonicalize()
        .expect("the scratch directory resolves")
        .to_string_lossy()
        .into_owned();
    let mut forms = Vec::new();
    for form in [created, canonical] {
        let plain = form.strip_prefix(r"\\?\").unwrap_or(&form).to_owned();
        let public = plain.strip_prefix("/private").unwrap_or(&plain).to_owned();
        for spelling in [
            form.clone(),
            format!(r"\\?\{plain}"),
            format!("/private{public}"),
            plain,
            public,
        ] {
            forms.push(spelling.replace('\\', r"\\"));
            forms.push(spelling);
        }
    }
    forms.push(
        dir.file_name()
            .expect("the scratch directory has a name")
            .to_string_lossy()
            .into_owned(),
    );
    forms.sort();
    forms.dedup();
    forms
}

/// `s` without whitespace and without miette's frame gutter (U+2502): an error frame
/// wraps a long line, even inside a path, and continues it after the gutter.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{2502}')
        .collect()
}

/// The first spelling of `dir` that `text` carries, as printed or wrapped across an
/// error frame's lines, if any.
fn leak(text: &str, dir: &Path) -> Option<String> {
    let squashed = squash(text);
    spellings(dir)
        .into_iter()
        .find(|spelling| text.contains(spelling.as_str()) || squashed.contains(&squash(spelling)))
}

/// The leak check itself finds the scratch directory in every spelling a line could
/// carry, JSON-escaped too, and finds nothing in a line that names a relative path.
#[test]
fn the_leak_check_finds_the_scratch_directory_in_every_spelling() {
    let dir = scratch();
    let canonical = dir.path().canonicalize().unwrap();
    let mut planted = vec![dir.path().to_path_buf(), canonical.clone()];
    // macOS: the temporary directory is `/var/…`, canonical `/private/var/…`.
    if let Ok(public) = canonical.strip_prefix("/private") {
        planted.push(Path::new("/").join(public));
    }
    for shown in planted {
        let line = format!("Compiled to {}\n", shown.join("page.md").display());
        assert!(
            leak(&line, dir.path()).is_some(),
            "the check must find {shown:?} in {line:?}"
        );
        let json = serde_json::to_string(&line).unwrap();
        assert!(
            leak(&json, dir.path()).is_some(),
            "the check must find {shown:?} JSON-escaped in {json:?}"
        );
        // An error frame wraps a long line inside the path, at a hyphen, and goes on
        // after its gutter.
        let text = shown.to_string_lossy().into_owned();
        let at = text
            .rfind('-')
            .expect("the scratch directory's name has a hyphen")
            + 1;
        let wrapped = format!(
            "  \u{d7} cannot create output directory {}\n  \u{2502} {}/out: File exists\n",
            &text[..at],
            &text[at..]
        );
        assert!(
            leak(&wrapped, dir.path()).is_some(),
            "the check must find {shown:?} wrapped in {wrapped:?}"
        );
    }
    assert_eq!(
        leak(
            &format!("Compiled to {}\n", native("out/page.md")),
            dir.path()
        ),
        None,
        "a relative path is no leak"
    );
}

// ── `Compiled to` under `--out-dir` ──────────────────────────────────────────

/// A directory build under a relative `--out-dir` names each output below the out-dir as
/// typed, never below its canonical absolute path.
#[test]
fn compiled_to_under_a_relative_out_dir_names_the_out_dir_as_typed() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");

    let out = run(dir.path(), &["build", "src", "--out-dir", "out"]);
    let stderr = text(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\n1 built, 0 failed\n",
            native("out/sub/page.md")
        ),
        "the output is named below the out-dir as typed"
    );
    assert!(
        dir.path().join("out/sub/page.md").is_file(),
        "the output was written"
    );
    assert_eq!(leak(&stderr, dir.path()), None, "stderr: {stderr}");
    assert_eq!(leak(&text(&out.stdout), dir.path()), None);
}

/// An out-dir reached through a symlink is named by the link, as typed — relative or
/// absolute — never by the directory the link resolves to.
///
/// Unix-only: it creates a directory symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn compiled_to_under_a_symlinked_out_dir_names_the_link_not_its_target() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");
    std::fs::create_dir(dir.path().join("real-target")).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("real-target"),
        dir.path().join("alias-link"),
    )
    .unwrap();

    // Relative, as typed.
    let out = run(dir.path(), &["build", "src", "--out-dir", "alias-link"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\n1 built, 0 failed\n",
            native("alias-link/sub/page.md")
        ),
        "the output is named below the link as typed"
    );
    assert!(
        dir.path().join("real-target/sub/page.md").is_file(),
        "the output was written through the link"
    );
    assert!(
        !stderr.contains("real-target"),
        "never by the link's target; stderr: {stderr}"
    );
    assert_eq!(leak(&stderr, dir.path()), None, "stderr: {stderr}");

    // Absolute, as typed: the same spelling, never the canonical one.
    let typed = dir.path().join("alias-link");
    let out = run(
        dir.path(),
        &["build", "src", "--out-dir", typed.to_str().unwrap()],
    );
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\n1 built, 0 failed\n",
            typed.join("sub").join("page.md").display()
        ),
        "an absolute out-dir is named exactly as typed"
    );
    assert!(
        !stderr.contains("real-target"),
        "never by the link's target; stderr: {stderr}"
    );
}

/// The `Source map written` lines under a relative `--out-dir` name each map below the
/// out-dir as typed.
#[test]
fn source_map_lines_under_an_out_dir_name_the_out_dir_as_typed() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");

    let out = run(
        dir.path(),
        &["build", "src", "--out-dir", "out", "--source-map"],
    );
    let stderr = text(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\nSource map written to {}\n1 built, 0 failed\n",
            native("out/sub/page.md"),
            native("out/sub/page.md.map")
        ),
    );
    assert!(
        dir.path().join("out/sub/page.md.map").is_file(),
        "the map was written"
    );
    assert_eq!(leak(&stderr, dir.path()), None, "stderr: {stderr}");
}

/// An output directory that cannot be created is named as its output is: below
/// `--out-dir` as typed, and below the directory `mds.json` was reached by.
#[test]
fn an_output_directory_that_cannot_be_created_is_named_as_its_output_is() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");
    // A file where the output's directory would go.
    put(dir.path(), "out/sub", "not a directory\n");
    let proj = dir.path().join("proj");
    put(&proj, "mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(&proj, "page.mds", "Page\n");
    put(&proj, "dist", "not a directory\n");

    for (cwd, args, shown) in [
        (
            dir.path(),
            &["build", "src", "--out-dir", "out"][..],
            "out/sub",
        ),
        (proj.as_path(), &["build", "page.mds"][..], "./dist"),
    ] {
        let out = run(cwd, args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        let expected = format!("cannot create output directory {}: ", native(shown));
        assert!(
            stderr.contains(&expected),
            "{args:?}: stderr must hold {expected:?}; got: {stderr}"
        );
        assert_eq!(
            leak(&stderr, dir.path()),
            None,
            "{args:?}: stderr: {stderr}"
        );
    }
}

// ── `mds.json` `build.output_dir` and `build.source_map` ─────────────────────

/// An output under `mds.json` `build.output_dir` is named through the directory the
/// argument reached `mds.json` by — `./`, `src/../`, one `..` per step up — exactly as a
/// config error names `mds.json` itself, never by the canonical config directory.
#[test]
fn a_config_output_dir_is_named_through_the_directory_mds_json_was_reached_by() {
    let dir = scratch();
    let proj = dir.path().join("proj");
    put(&proj, "mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(&proj, "page.mds", "Page\n");
    put(&proj, "src/sub/deep.mds", "Deep\n");

    let cases: [(&[&str], String, &str); 3] = [
        (
            &["build", "page.mds"],
            format!("Compiled to {}\n", native("./dist/page.md")),
            "dist/page.md",
        ),
        (
            &["build", "src"],
            format!(
                "Compiled to {}\n1 built, 0 failed\n",
                native("src/../dist/sub/deep.md")
            ),
            "dist/sub/deep.md",
        ),
        (
            &["build", "src/sub/deep.mds"],
            format!("Compiled to {}\n", native("src/sub/../../dist/deep.md")),
            "dist/deep.md",
        ),
    ];
    for (args, expected, written) in cases {
        let out = run(&proj, &typed_args(args));
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: stderr: {stderr}");
        assert_eq!(stderr, expected, "{args:?}");
        assert!(
            proj.join(written).is_file(),
            "{args:?}: {written} was written"
        );
        assert_eq!(
            leak(&stderr, dir.path()),
            None,
            "{args:?}: stderr: {stderr}"
        );
    }
}

/// Under `mds.json` `build.output_dir`, the source map a build writes and the stale map
/// a later build without one removes are both named as the output is.
#[test]
fn a_config_output_dir_names_the_map_written_and_the_stale_map_removed() {
    let dir = scratch();
    let proj = dir.path().join("proj");
    put(&proj, "mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(&proj, "page.mds", "Page\n");

    let out = run(&proj, &["build", "page.mds", "--source-map"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\nSource map written to {}\n",
            native("./dist/page.md"),
            native("./dist/page.md.map")
        ),
    );
    assert!(
        proj.join("dist/page.md.map").is_file(),
        "the map was written"
    );
    assert_eq!(leak(&stderr, dir.path()), None, "stderr: {stderr}");

    let out = run(&proj, &["build", "page.mds"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\nRemoved stale map {}\n",
            native("./dist/page.md"),
            native("./dist/page.md.map")
        ),
    );
    assert!(
        !proj.join("dist/page.md.map").exists(),
        "the stale map was removed"
    );
    assert_eq!(leak(&stderr, dir.path()), None, "stderr: {stderr}");
}

/// The warning that `mds.json` `build.source_map` has no effect on stdout names the
/// `mds.json` it came from as the argument reached it, as a config error does — never
/// the canonical config directory.
#[test]
fn the_config_source_map_warning_names_mds_json_as_reached() {
    let dir = scratch();
    let proj = dir.path().join("proj");
    put(&proj, "mds.json", r#"{"build":{"source_map":true}}"#);
    put(&proj, "page.mds", "Page\n");
    put(&proj, "src/deep.mds", "Deep\n");

    for (args, config, stdout) in [
        (["build", "page.mds", "-o", "-"], "./mds.json", "Page\n"),
        (
            ["build", "src/deep.mds", "-o", "-"],
            "src/../mds.json",
            "Deep\n",
        ),
    ] {
        let out = run(&proj, &typed_args(&args));
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: stderr: {stderr}");
        assert_eq!(
            stderr,
            format!(
                "warning: source_map in {} has no effect when writing to stdout (sidecar \
                 requires -o <file> or --out-dir); use --inline to embed the map, or \
                 --no-source-map to silence this warning\n",
                native(config)
            ),
            "{args:?}"
        );
        assert_eq!(
            text(&out.stdout),
            stdout,
            "{args:?}: the output went to stdout"
        );
        assert_eq!(
            leak(&stderr, dir.path()),
            None,
            "{args:?}: stderr: {stderr}"
        );
    }
}

// ── lint `Clean:` ────────────────────────────────────────────────────────────

/// `Clean:` names a file argument as typed — its directory part and an absolute path
/// included — as `Fixed:` and `Would fix:` already do, not by its file name alone.
#[test]
fn clean_names_a_file_argument_as_typed() {
    let dir = scratch();
    let file = put(
        dir.path(),
        "sub/page.mds",
        "---\ngreeting: Hello\n---\n\n{{greeting}}, world!\n",
    );

    let out = run(dir.path(), &typed_args(&["lint", "sub/page.mds"]));
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr, format!("Clean: {}\n", native("sub/page.mds")));

    let out = run(&dir.path().join("sub"), &["lint", "page.mds"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr, "Clean: page.mds\n", "a bare name stays bare");

    let typed = file.to_str().unwrap();
    let out = run(dir.path(), &["lint", typed]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!("Clean: {typed}\n"),
        "an absolute argument is named as typed"
    );
}

/// The two lines that open a unified diff of `path` (written with `/`, printed through
/// [`native`]).
fn diff_header(path: &str) -> String {
    let path = native(path);
    format!("--- {path}\n+++ {path}\n")
}

/// `mds lint` names a file argument as typed in `Fixed:`, `Would fix:` and the header of a
/// `--fix --diff`, a directory's entry below the directory argument as typed, and stdin as
/// `<stdin>`. A finding's source frame and the JSON `file` key name a file argument by its
/// file name and a directory's entry by its path relative to the directory argument,
/// `/`-separated on every OS.
#[test]
fn lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name() {
    const WARN: &str = "---\ngreeting: Hello\nunused_key: x\n---\n\n{{greeting}}, world!\n";
    const FIXABLE: &str = "@if \"x\" == \"y\":\nhidden\n@end\nHello\n";
    let dir = scratch();
    let root = dir.path();
    put(root, "sub/warn.mds", WARN);
    put(root, "lints/deep/warn.mds", WARN);
    put(root, "sub/f.mds", FIXABLE);
    put(root, "fix/deep/f.mds", FIXABLE);
    let lint = |args: &[&str], stdin: Option<&'static str>| {
        let args = typed_args(args);
        let out = match stdin {
            Some(input) => run_with_stdin(root, &args, input),
            None => run(root, &args),
        };
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(leak(&stdout, root), None, "{args:?}: stdout: {stdout}");
        assert_eq!(leak(&stderr, root), None, "{args:?}: stderr: {stderr}");
        (
            out.status.code(),
            stdout,
            stderr,
            format!("mds {}", args.join(" ")),
        )
    };

    // (the input, stdin, the source frame's header, the JSON `file` key)
    for (input, stdin, frame, key) in [
        ("sub/warn.mds", None, "[warn.mds:3:1]", "warn.mds"),
        ("lints", None, "[deep/warn.mds:3:1]", "deep/warn.mds"),
        ("-", Some(WARN), "[<stdin>:3:1]", "<stdin>"),
    ] {
        let (code, _, stderr, label) = lint(&["lint", input], stdin);
        assert_eq!(code, Some(1), "{label}: stderr: {stderr}");
        assert!(
            stderr.contains(frame),
            "{label}: the finding's frame names the input as {frame:?}; stderr: {stderr}"
        );
        assert!(
            !stderr.contains(&format!("[{}", native(input))) || input == "-",
            "{label}: never by the path as typed; stderr: {stderr}"
        );
        let (code, stdout, _, label) = lint(&["lint", input, "--format", "json"], stdin);
        assert_eq!(code, Some(1), "{label}: stdout: {stdout}");
        let key = format!("\"file\":\"{key}\"");
        assert!(stdout.contains(&key), "{label}: {key}; stdout: {stdout}");
    }

    // Read-only first, then the fixes.
    let (code, _, stderr, label) = lint(&["lint", "--fix", "--check", "sub/f.mds"], None);
    assert_eq!(code, Some(1), "{label}: stderr: {stderr}");
    let would = format!("Would fix: {}", native("sub/f.mds"));
    assert!(
        stderr.lines().any(|line| line == would),
        "{label}: {would:?}; stderr: {stderr}"
    );
    let (code, _, stderr, label) = lint(&["lint", "--fix", "--check", "-"], Some(FIXABLE));
    assert_eq!(code, Some(1), "{label}: stderr: {stderr}");
    assert!(
        stderr.lines().any(|line| line == "Would fix: <stdin>"),
        "{label}: stderr: {stderr}"
    );
    for (input, shown) in [("sub/f.mds", "sub/f.mds"), ("fix", "fix/deep/f.mds")] {
        let (code, stdout, stderr, label) = lint(&["lint", "--fix", "--diff", input], None);
        assert_eq!(code, Some(1), "{label}: stderr: {stderr}");
        assert!(
            stdout.starts_with(&diff_header(shown)),
            "{label}: the diff names the input as {shown:?}; stdout: {stdout}"
        );
    }
    let (code, _, stderr, label) = lint(&["lint", "--fix", "sub/f.mds"], None);
    assert_eq!(code, Some(0), "{label}: stderr: {stderr}");
    assert_eq!(
        stderr,
        format!("Fixed: {}\n", native("sub/f.mds")),
        "{label}"
    );
    let (code, _, stderr, label) = lint(&["lint", "--fix", "fix"], None);
    assert_eq!(code, Some(0), "{label}: stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Fixed: {}\n1 clean, 0 with warnings, 0 with errors, 0 resource-limited\n",
            native("fix/deep/f.mds")
        ),
        "{label}"
    );
}

// ── `mds fmt` and `mds init` ─────────────────────────────────────────────────

/// `mds fmt` names a file argument as typed, a directory's entry below the directory
/// argument as typed and stdin as `<stdin>`, in `Formatted:`, `Unchanged:` and
/// `Would reformat:` and in the header of a `--diff`.
#[test]
fn fmt_names_a_file_argument_as_typed_and_a_directory_s_entries_below_it() {
    let dir = scratch();
    let root = dir.path();
    // Without a final newline each of these reformats; `fine.mds` does not.
    put(root, "sub/messy.mds", "Hello");
    put(root, "sub/fine.mds", "Fine\n");
    put(root, "src/a.mds", "A");
    put(root, "src/inner/b.mds", "B");
    let fmt = |args: &[&str], stdin: Option<&'static str>| {
        let args = typed_args(args);
        let out = match stdin {
            Some(input) => run_with_stdin(root, &args, input),
            None => run(root, &args),
        };
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(leak(&stdout, root), None, "{args:?}: stdout: {stdout}");
        assert_eq!(leak(&stderr, root), None, "{args:?}: stderr: {stderr}");
        (
            out.status.code(),
            stdout,
            stderr,
            format!("mds {}", args.join(" ")),
        )
    };

    // Read-only first: (arguments, stdin, exit, the start of stdout, all of stderr).
    let cases = [
        (
            &["fmt", "--check", "sub/messy.mds"][..],
            None,
            1,
            String::new(),
            format!("Would reformat: {}\n", native("sub/messy.mds")),
        ),
        (
            &["fmt", "--check", "sub/fine.mds"][..],
            None,
            0,
            String::new(),
            format!("Unchanged: {}\n", native("sub/fine.mds")),
        ),
        (
            &["fmt", "--diff", "sub/messy.mds"][..],
            None,
            0,
            diff_header("sub/messy.mds"),
            String::new(),
        ),
        (
            &["fmt", "--check", "-"][..],
            Some("Q"),
            1,
            String::new(),
            "Would reformat: <stdin>\n".to_owned(),
        ),
        (
            &["fmt", "--diff", "-"][..],
            Some("Q"),
            0,
            diff_header("<stdin>"),
            String::new(),
        ),
    ];
    for (args, stdin, exit, stdout_start, expected_stderr) in cases {
        let (code, stdout, stderr, label) = fmt(args, stdin);
        assert_eq!(code, Some(exit), "{label}: stderr: {stderr}");
        assert!(
            stdout.starts_with(&stdout_start),
            "{label}: stdout starts {stdout_start:?}; stdout: {stdout}"
        );
        assert_eq!(stderr, expected_stderr, "{label}");
    }
    // A directory's entries come in the order the walk finds them.
    let (code, stdout, stderr, label) = fmt(&["fmt", "--diff", "src"], None);
    assert_eq!(code, Some(0), "{label}: stderr: {stderr}");
    for entry in ["src/a.mds", "src/inner/b.mds"] {
        assert!(
            stdout.contains(&diff_header(entry)),
            "{label}: {entry} is named below the directory as typed; stdout: {stdout}"
        );
    }
    assert_eq!(
        stderr, "2 would reformat, 0 unchanged, 0 failed\n",
        "{label}"
    );

    let (code, _, stderr, label) = fmt(&["fmt", "sub/messy.mds"], None);
    assert_eq!(code, Some(0), "{label}: stderr: {stderr}");
    assert_eq!(
        stderr,
        format!("Formatted: {}\n", native("sub/messy.mds")),
        "{label}"
    );
    let (code, _, stderr, label) = fmt(&["fmt", "src"], None);
    assert_eq!(code, Some(0), "{label}: stderr: {stderr}");
    assert_eq!(
        lines_starting(&stderr, "Formatted: "),
        [
            format!("Formatted: {}", native("src/a.mds")),
            format!("Formatted: {}", native("src/inner/b.mds")),
        ],
        "{label}: stderr: {stderr}"
    );
    assert!(
        stderr.ends_with("2 formatted, 0 unchanged, 0 failed\n"),
        "{label}: stderr: {stderr}"
    );
}

/// `mds init` names the file it creates as typed, in `Created` and in its `Try:` hint, and
/// in its refusal to overwrite the file; with no argument the file is `hello.mds`.
#[test]
fn init_names_the_file_it_creates_as_typed() {
    let dir = scratch();
    let root = dir.path();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(root.join("bare")).unwrap();
    let typed = native("sub/new.mds");

    let out = run(root, &typed_args(&["init", "sub/new.mds"]));
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!("Created {typed}\n  Try: mds build {typed}\n")
    );
    assert!(root.join("sub/new.mds").is_file(), "the file was created");
    assert_eq!(leak(&stderr, root), None, "stderr: {stderr}");

    let out = run(root, &typed_args(&["init", "sub/new.mds"]));
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    let refusal = format!("{typed} already exists (use --force to overwrite)");
    assert!(stderr.contains(&refusal), "{refusal:?}; stderr: {stderr}");
    assert_eq!(leak(&stderr, root), None, "stderr: {stderr}");

    let out = run(&root.join("bare"), &["init"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stderr, "Created hello.mds\n  Try: mds build hello.mds\n");
    assert_eq!(leak(&stderr, root), None, "stderr: {stderr}");
}

// ── The build, check and lint runs listed in `ROWS` ──────────────────────────

/// One run of the sweep below: where it runs, what it runs, what it reads on stdin, its
/// exit code, and a line it must print — proof that the sink the absence check looks at
/// fired.
struct Row {
    cwd: &'static str,
    args: &'static [&'static str],
    stdin: Option<&'static str>,
    exit: i32,
    stdout: Option<&'static str>,
    stderr: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        cwd: ".",
        args: &["build", "x.mds"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to ./x.md\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "src/a.mds"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to src/a.md\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "x.mds", "--source-map"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Source map written to ./x.md.map\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "x.mds", "--out-dir", "o1"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to o1/x.md\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "x.mds", "-o", "o2/y.md"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to o2/y.md\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "-", "--out-dir", "o3"],
        stdin: Some("Hi\n"),
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to o3/output.md\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "src"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to src/sub/b.md\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "src", "--out-dir", "o4", "--source-map"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Source map written to o4/sub/b.md.map\n"),
    },
    Row {
        cwd: ".",
        args: &["build", "bad", "--out-dir", "o5"],
        stdin: None,
        exit: 1,
        stdout: None,
        stderr: Some("undefined variable 'nope'"),
    },
    Row {
        cwd: "proj",
        args: &["build", "p.mds"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Compiled to ./dist/p.md\n"),
    },
    Row {
        cwd: "proj",
        args: &["build", "src", "--source-map"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Source map written to src/../dist/q.md.map\n"),
    },
    Row {
        cwd: "smap",
        args: &["build", "s.mds", "-o", "-"],
        stdin: None,
        exit: 0,
        stdout: Some("S\n"),
        stderr: Some("warning: source_map in ./mds.json has no effect"),
    },
    Row {
        cwd: ".",
        args: &["check", "x.mds"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("OK: x.mds\n"),
    },
    Row {
        cwd: ".",
        args: &["check", "src/a.mds"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("OK: src/a.mds\n"),
    },
    Row {
        cwd: ".",
        args: &["check", "-"],
        stdin: Some("Hi\n"),
        exit: 0,
        stdout: None,
        stderr: Some("OK: <stdin>\n"),
    },
    Row {
        cwd: ".",
        args: &["check", "src"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("2 passed, 0 failed\n"),
    },
    Row {
        cwd: ".",
        args: &["check", "bad"],
        stdin: None,
        exit: 1,
        stdout: None,
        stderr: Some("undefined variable 'nope'"),
    },
    Row {
        cwd: ".",
        args: &["lint", "lint/clean.mds"],
        stdin: None,
        exit: 0,
        stdout: None,
        stderr: Some("Clean: lint/clean.mds\n"),
    },
    Row {
        cwd: ".",
        args: &["lint", "lint/warn.mds"],
        stdin: None,
        exit: 1,
        stdout: None,
        stderr: Some("unused_key"),
    },
    Row {
        cwd: ".",
        args: &["lint", "lint"],
        stdin: None,
        exit: 1,
        stdout: None,
        stderr: Some("1 clean, 1 with warnings, 0 with errors, 0 resource-limited\n"),
    },
    Row {
        cwd: ".",
        args: &["lint", "lint", "--format", "json"],
        stdin: None,
        exit: 1,
        stdout: Some("\"file\":\"warn.mds\""),
        stderr: None,
    },
    Row {
        cwd: ".",
        args: &["lint", "fix", "--fix", "--check"],
        stdin: None,
        exit: 1,
        stdout: None,
        stderr: Some("Would fix: fix/f.mds\n"),
    },
];

/// Each build, check and lint run in [`ROWS`], run from the scratch directory with
/// relative arguments, names no spelling of the scratch directory on stdout or stderr —
/// and printed the line that shows its sink ran.
#[test]
fn no_listed_build_check_or_lint_run_names_the_working_directory() {
    let dir = scratch();
    let root = dir.path();
    put(root, "x.mds", "Hello\n");
    put(root, "src/a.mds", "A\n");
    put(root, "src/sub/b.mds", "B\n");
    put(root, "bad/undef.mds", "Hi {{nope}}\n");
    put(root, "proj/mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(root, "proj/p.mds", "P\n");
    put(root, "proj/src/q.mds", "Q\n");
    put(root, "smap/mds.json", r#"{"build":{"source_map":true}}"#);
    put(root, "smap/s.mds", "S\n");
    put(
        root,
        "lint/clean.mds",
        "---\ngreeting: Hello\n---\n\n{{greeting}}, world!\n",
    );
    put(
        root,
        "lint/warn.mds",
        "---\ngreeting: Hello\nunused_key: never referenced in the body\n---\n\n{{greeting}}, world!\n",
    );
    put(
        root,
        "fix/f.mds",
        "@if \"x\" == \"y\":\nhidden\n@end\nHello\n",
    );

    for row in ROWS {
        let cwd = root.join(row.cwd);
        let args = typed_args(row.args);
        let out = match row.stdin {
            Some(input) => run_with_stdin(&cwd, &args, input),
            None => run(&cwd, &args),
        };
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        let label = format!("(in {}) mds {}", row.cwd, args.join(" "));
        assert_eq!(
            out.status.code(),
            Some(row.exit),
            "{label}: stdout: {stdout}; stderr: {stderr}"
        );
        if let Some(line) = row.stdout {
            assert!(
                stdout.contains(&native(line)),
                "{label}: stdout must hold {line:?}; got: {stdout}"
            );
        }
        if let Some(line) = row.stderr {
            assert!(
                stderr.contains(&native(line)),
                "{label}: stderr must hold {line:?}; got: {stderr}"
            );
        }
        assert_eq!(leak(&stdout, root), None, "{label}: stdout: {stdout}");
        assert_eq!(leak(&stderr, root), None, "{label}: stderr: {stderr}");
    }
}

// ── `mds watch`, where it shares build's output lines ─────────────────────────

/// `mds watch` announces its startup outputs through build's `Compiled to` line, so an
/// output under `--out-dir` (directory mode) or under `mds.json` `build.output_dir`
/// (both modes) is named as `mds build` names it — for an entry typed as the volume
/// spells it (see `an_entry_typed_in_another_case_names_its_output_by_the_name_on_disk`).
/// Watch's own lines are pinned in the section below.
#[test]
fn watch_startup_names_an_out_dir_and_a_config_output_dir_as_build_does() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");
    let proj = dir.path().join("proj");
    put(&proj, "mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(&proj, "p.mds", "P\n");
    put(&proj, "src/q.mds", "Q\n");

    let cases: [(&Path, &[&str], String); 3] = [
        (
            dir.path(),
            &["watch", "src", "--out-dir", "out"],
            format!("Compiled to {}\n", native("out/sub/page.md")),
        ),
        (
            &proj,
            &["watch", "src"],
            format!("Compiled to {}\n", native("src/../dist/q.md")),
        ),
        (
            &proj,
            &["watch", "p.mds"],
            format!("Compiled to {}\n", native("./dist/p.md")),
        ),
    ];
    for (cwd, args, expected) in cases {
        let (child, tap, _) = common::spawn_watch_ready(
            mds_bin()
                .current_dir(cwd)
                .args(args)
                .args(["--debounce", "0"])
                .stdout(Stdio::null()),
        );
        let mut child = common::ChildGuard(child);
        let stderr = tap.finish_text(&mut child);
        assert!(
            stderr.contains(&expected),
            "{args:?}: stderr must hold {expected:?}; got: {stderr}"
        );
        let canonical = cwd.canonicalize().unwrap();
        assert!(
            !stderr.contains(&format!("Compiled to {}", canonical.display())),
            "{args:?}: never below the canonical directory; got: {stderr}"
        );
    }
}

// ── `mds watch`'s own lines ──────────────────────────────────────────────────

/// Bound for one step of a watch session — a rebuild, a deletion. A failure bound only:
/// each wait returns as soon as its line is on stderr.
const WATCH_STEP: Duration = Duration::from_secs(10);

/// `mds watch <args> --debounce 0` in `cwd`, live — every watch armed, its startup output
/// printed. stdout is tapped when `tap_stdout` (`-o -`), discarded otherwise.
fn watch_live(
    cwd: &Path,
    args: &[impl AsRef<OsStr>],
    tap_stdout: bool,
) -> (
    common::ChildGuard,
    common::StderrTap,
    Option<common::StdoutTap>,
) {
    let stdout = if tap_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let (child, stderr, stdout) = common::spawn_watch_ready(
        mds_bin()
            .current_dir(cwd)
            .args(args)
            .args(["--debounce", "0"])
            .stdout(stdout),
    );
    (common::ChildGuard(child), stderr, stdout)
}

/// Makes a directory writable again on drop, so the scratch directory can go.
#[cfg(unix)]
struct Writable(PathBuf);

#[cfg(unix)]
impl Drop for Writable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// The lines of `text` that start with `prefix`, sorted: a directory's sources are
/// compiled in the order the walk finds them, which the filesystem decides.
fn lines_starting(text: &str, prefix: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// `Watching` names the entry as typed — below a subdirectory, through `..` uncollapsed,
/// and by its bare name when auto-detection found it — and `Watching directory` names the
/// directory argument as typed, `.` included; never by the canonical absolute path the
/// session matches events against. Each session's startup output is compared whole, the
/// `Compiled to` beside each source included, so every line the absence check covers is
/// proven printed.
#[test]
fn watching_names_the_entry_and_the_directory_as_typed() {
    let dir = scratch();
    let root = dir.path();
    put(root, "sub/page.mds", "Page\n");
    put(root, "solo/x.mds", "X\n");
    put(root, "src/a.mds", "A\n");
    put(root, "src/sub/b.mds", "B\n");

    // (working directory, arguments, the banner, the startup `Compiled to` names)
    let cases: [(&str, &[&str], &str, &[&str]); 6] = [
        (
            ".",
            &["watch", "sub/page.mds"],
            "Watching sub/page.mds",
            &["sub/page.md"],
        ),
        (
            ".",
            &["watch", "sub/../sub/page.mds"],
            "Watching sub/../sub/page.mds",
            &["sub/../sub/page.md"],
        ),
        ("solo", &["watch"], "Watching x.mds", &["./x.md"]),
        (
            ".",
            &["watch", "src"],
            "Watching directory src",
            &["src/a.md", "src/sub/b.md"],
        ),
        (
            "src",
            &["watch", "."],
            "Watching directory .",
            &["./a.md", "./sub/b.md"],
        ),
        (
            ".",
            &["watch", "src/sub/.."],
            "Watching directory src/sub/..",
            &["src/sub/../a.md", "src/sub/../sub/b.md"],
        ),
    ];
    for (cwd, args, banner, compiled) in cases {
        let args = typed_args(args);
        let label = format!("(in {cwd}) mds {}", args.join(" "));
        let (mut child, tap, _) = watch_live(&root.join(cwd), &args, false);
        let stderr = tap.finish_text(&mut child);

        let mut expected: Vec<String> = compiled
            .iter()
            .map(|name| format!("Compiled to {}", native(name)))
            .collect();
        expected.sort();
        assert_eq!(
            stderr.lines().next(),
            Some(native(banner).as_str()),
            "{label}: the banner names the argument as typed; stderr: {stderr}"
        );
        assert_eq!(
            lines_starting(&stderr, "Compiled to "),
            expected,
            "{label}: stderr: {stderr}"
        );
        assert_eq!(
            stderr.lines().count(),
            1 + compiled.len(),
            "{label}: nothing but the banner and the startup outputs; stderr: {stderr}"
        );
        assert_eq!(leak(&stderr, root), None, "{label}: stderr: {stderr}");
    }
}

/// An entry, and a directory argument, reached through a symlinked directory are named by
/// the link, as typed — in the banner, the startup `Compiled to` and a rebuild's
/// `Recompiled` — never by the directory the link resolves to, which is what the session
/// watches.
///
/// Unix-only: it creates a directory symlink; the rule itself is platform-independent.
#[cfg(unix)]
#[test]
fn watching_names_a_path_reached_through_a_symlink_by_the_link() {
    let dir = scratch();
    let root = dir.path();
    put(root, "real-target/page.mds", "Page\n");
    put(root, "real-target/sub/b.mds", "B\n");
    std::os::unix::fs::symlink(root.join("real-target"), root.join("alias-link")).unwrap();

    // (arguments, the banner, the output's name, the source edited, the output written)
    let cases: [(&[&str], &str, &str, &str, &str); 2] = [
        (
            &["watch", "alias-link/page.mds"],
            "Watching alias-link/page.mds",
            "alias-link/page.md",
            "real-target/page.mds",
            "real-target/page.md",
        ),
        (
            &["watch", "alias-link/sub"],
            "Watching directory alias-link/sub",
            "alias-link/sub/b.md",
            "real-target/sub/b.mds",
            "real-target/sub/b.md",
        ),
    ];
    for (args, banner, output, edit, written) in cases {
        let (mut child, tap, _) = watch_live(root, args, false);
        write_atomic(&root.join(edit), "Edited\n");
        common::wait_for_tap(&tap, "Recompiled ", WATCH_STEP);
        let stderr = tap.finish_text(&mut child);

        assert_eq!(
            stderr.lines().next(),
            Some(banner),
            "{args:?}: stderr: {stderr}"
        );
        assert_eq!(
            lines_starting(&stderr, "Compiled to "),
            [format!("Compiled to {output}")],
            "{args:?}: stderr: {stderr}"
        );
        let recompiled = lines_starting(&stderr, "Recompiled ");
        assert!(
            !recompiled.is_empty()
                && recompiled
                    .iter()
                    .all(|line| line.starts_with(&format!("Recompiled {output} ("))),
            "{args:?}: every rebuild names the output by the link; stderr: {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(written)).unwrap(),
            "Edited\n",
            "{args:?}: the rebuild was written through the link"
        );
        assert!(
            !stderr.contains("real-target"),
            "{args:?}: never by the link's target; stderr: {stderr}"
        );
        assert_eq!(leak(&stderr, root), None, "{args:?}: stderr: {stderr}");
    }
}

/// One `mds watch` session of [`SESSIONS`]: where it runs and what it runs, the outputs
/// its startup names, the source it edits once live and the name the rebuild's
/// `Recompiled` line gives, and — in directory mode — the source it deletes after that
/// and the name the `Removed` line gives its output. Every path is written with `/`,
/// relative to `cwd`.
struct Session {
    cwd: &'static str,
    args: &'static [&'static str],
    /// `-o -`: stdout is the session's product and is checked as well.
    stdout: bool,
    compiled: &'static [&'static str],
    edit: &'static str,
    recompiled: &'static str,
    removed: Option<(&'static str, &'static str)>,
}

const SESSIONS: &[Session] = &[
    Session {
        cwd: ".",
        args: &["watch", "w1/page.mds"],
        stdout: false,
        compiled: &["w1/page.md"],
        edit: "w1/page.mds",
        recompiled: "w1/page.md",
        removed: None,
    },
    Session {
        cwd: ".",
        args: &["watch", "w2/page.mds", "--out-dir", "o2"],
        stdout: false,
        compiled: &["o2/page.md"],
        edit: "w2/page.mds",
        recompiled: "o2/page.md",
        removed: None,
    },
    Session {
        cwd: ".",
        args: &["watch", "w3/page.mds", "-o", "o3/y.md"],
        stdout: false,
        compiled: &["o3/y.md"],
        edit: "w3/page.mds",
        recompiled: "o3/y.md",
        removed: None,
    },
    Session {
        cwd: ".",
        args: &["watch", "w4/page.mds", "-o", "-"],
        stdout: true,
        compiled: &[],
        edit: "w4/page.mds",
        recompiled: "<stdout>",
        removed: None,
    },
    Session {
        cwd: "w5",
        args: &["watch"],
        stdout: false,
        compiled: &["./x.md"],
        edit: "x.mds",
        recompiled: "./x.md",
        removed: None,
    },
    Session {
        cwd: "p6",
        args: &["watch", "p.mds"],
        stdout: false,
        compiled: &["./dist/p.md"],
        edit: "p.mds",
        recompiled: "./dist/p.md",
        removed: None,
    },
    Session {
        cwd: ".",
        args: &["watch", "d7"],
        stdout: false,
        compiled: &["d7/a.md", "d7/sub/b.md"],
        edit: "d7/sub/b.mds",
        recompiled: "d7/sub/b.md",
        removed: Some(("d7/sub/b.mds", "d7/sub/b.md")),
    },
    Session {
        cwd: ".",
        args: &["watch", "d8", "--out-dir", "o8"],
        stdout: false,
        compiled: &["o8/a.md", "o8/sub/b.md"],
        edit: "d8/sub/b.mds",
        recompiled: "o8/sub/b.md",
        removed: Some(("d8/sub/b.mds", "o8/sub/b.md")),
    },
    Session {
        cwd: "d9",
        args: &["watch", "."],
        stdout: false,
        compiled: &["./a.md", "./sub/b.md"],
        edit: "sub/b.mds",
        recompiled: "./sub/b.md",
        removed: Some(("sub/b.mds", "./sub/b.md")),
    },
    Session {
        cwd: "p10",
        args: &["watch", "src"],
        stdout: false,
        compiled: &["src/../dist/q.md", "src/../dist/sub/r.md"],
        edit: "src/sub/r.mds",
        recompiled: "src/../dist/sub/r.md",
        removed: Some(("src/sub/r.mds", "src/../dist/sub/r.md")),
    },
];

/// Every `mds watch` session in [`SESSIONS`] names each output as typed, in every line
/// that names one — the startup `Compiled to`, a rebuild's `Recompiled` and, in directory
/// mode, the `Removed … (source deleted)` of a deleted source's output — and so never
/// mixes a typed `Compiled to` with an absolute `Recompiled`. Run from the scratch
/// directory with relative arguments, no session names any spelling of it on stdout or
/// stderr; each line the absence check covers is proven printed first.
#[test]
fn recompiled_and_removed_name_each_output_as_typed() {
    let dir = scratch();
    let root = dir.path();
    for src in ["w1", "w2", "w3", "w4"] {
        put(root, &format!("{src}/page.mds"), "Page\n");
    }
    put(root, "w5/x.mds", "X\n");
    put(root, "p6/mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(root, "p6/p.mds", "P\n");
    for src in ["d7", "d8", "d9"] {
        put(root, &format!("{src}/a.mds"), "A\n");
        put(root, &format!("{src}/sub/b.mds"), "B\n");
    }
    put(root, "p10/mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(root, "p10/src/q.mds", "Q\n");
    put(root, "p10/src/sub/r.mds", "R\n");

    for session in SESSIONS {
        let cwd = root.join(session.cwd);
        let args = typed_args(session.args);
        let label = format!("(in {}) mds {}", session.cwd, args.join(" "));
        let (mut child, tap, stdout_tap) = watch_live(&cwd, &args, session.stdout);

        write_atomic(&cwd.join(native(session.edit)), "Edited\n");
        common::wait_for_tap(&tap, "Recompiled ", WATCH_STEP);
        if let Some((source, _)) = session.removed {
            std::fs::remove_file(cwd.join(native(source))).expect("delete the source");
            common::wait_for_tap(&tap, "(source deleted)", WATCH_STEP);
        }
        let stderr = tap.finish_text(&mut child);
        let stdout = stdout_tap.map_or_else(String::new, |tap| tap.finish_text(&mut child));

        let mut compiled: Vec<String> = session
            .compiled
            .iter()
            .map(|name| format!("Compiled to {}", native(name)))
            .collect();
        compiled.sort();
        assert_eq!(
            lines_starting(&stderr, "Compiled to "),
            compiled,
            "{label}: stderr: {stderr}"
        );
        let recompiled = lines_starting(&stderr, "Recompiled ");
        let prefix = format!("Recompiled {} (", native(session.recompiled));
        assert!(
            !recompiled.is_empty() && recompiled.iter().all(|line| line.starts_with(&prefix)),
            "{label}: every rebuild names the output as {prefix:?}; stderr: {stderr}"
        );
        let removed: Vec<String> = session
            .removed
            .iter()
            .map(|(_, output)| format!("Removed {} (source deleted)", native(output)))
            .collect();
        assert_eq!(
            lines_starting(&stderr, "Removed "),
            removed,
            "{label}: stderr: {stderr}"
        );
        if session.stdout {
            assert_eq!(
                stdout, "Page\nEdited\n",
                "{label}: the startup output and the rebuild went to stdout"
            );
        }
        assert_eq!(leak(&stderr, root), None, "{label}: stderr: {stderr}");
        assert_eq!(leak(&stdout, root), None, "{label}: stdout: {stdout}");
    }
}

/// A source deleted in the same batch as an edit to the `--vars` file — the batch that
/// recompiles every source — has its output named as typed in the `Removed … (source
/// deleted)` line, as a deletion alone does.
#[test]
fn removed_names_the_output_as_typed_when_the_vars_file_changes_in_the_same_batch() {
    let dir = scratch();
    let root = dir.path();
    put(root, "d/a.mds", "A {{name}}\n");
    put(root, "d/sub/b.mds", "B\n");
    put(root, "vars.json", r#"{"name": "one"}"#);

    // A debounce window long enough to take both changes into one batch.
    let (child, tap, _) = common::spawn_watch_ready(
        mds_bin()
            .current_dir(root)
            .args(typed_args(&["watch", "d", "--vars", "vars.json"]))
            .args(["--debounce", "500"])
            .stdout(Stdio::null()),
    );
    let mut child = common::ChildGuard(child);
    std::fs::remove_file(root.join(native("d/sub/b.mds"))).expect("delete the source");
    write_atomic(&root.join("vars.json"), r#"{"name": "two"}"#);
    common::wait_for_tap(&tap, "(source deleted)", WATCH_STEP);
    common::wait_for_tap(&tap, "Recompiled ", WATCH_STEP);
    let stderr = tap.finish_text(&mut child);

    assert_eq!(
        lines_starting(&stderr, "Removed "),
        [format!("Removed {} (source deleted)", native("d/sub/b.md"))],
        "stderr: {stderr}"
    );
    let prefix = format!("Recompiled {} (", native("d/a.md"));
    let recompiled = lines_starting(&stderr, "Recompiled ");
    assert!(
        !recompiled.is_empty() && recompiled.iter().all(|line| line.starts_with(&prefix)),
        "the vars change rebuilt the other source, named as {prefix:?}; stderr: {stderr}"
    );
    assert_eq!(leak(&stderr, root), None, "stderr: {stderr}");
}

/// A deleted source's output that cannot be removed — its directory is read-only — is
/// named as typed in the warning that says so, as a removed one is in `Removed`: after a
/// deletion alone, and after one in the same batch as an edit to the `--vars` file.
///
/// Unix-only: it makes a directory read-only; skipped with a reason where the mode does
/// not stop a removal (running as root).
#[cfg(unix)]
#[test]
fn an_output_that_cannot_be_removed_is_named_as_typed() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch();
    let root = dir.path();
    // (fixture directory, the extra arguments, whether the `--vars` file is edited too)
    for (fixture, extra, edit_vars) in [
        ("alone", &["--debounce", "0"][..], false),
        (
            "with-vars",
            &["--vars", "vars.json", "--debounce", "500"][..],
            true,
        ),
    ] {
        let cwd = root.join(fixture);
        put(&cwd, "d/sub/b.mds", "B\n");
        put(&cwd, "vars.json", r#"{"name": "one"}"#);
        let (child, tap, _) = common::spawn_watch_ready(
            mds_bin()
                .current_dir(&cwd)
                .args(["watch", "d", "--out-dir", "o"])
                .args(extra)
                .stdout(Stdio::null()),
        );
        let mut child = common::ChildGuard(child);
        let sub = cwd.join("o/sub");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let _writable = Writable(sub.clone());
        if std::fs::write(sub.join(".write-probe"), b"").is_ok() {
            let _ = std::fs::remove_file(sub.join(".write-probe"));
            eprintln!(
                "skipped: {} is writable at mode 0o555 (running as root?)",
                sub.display()
            );
            return;
        }

        std::fs::remove_file(cwd.join("d/sub/b.mds")).expect("delete the source");
        if edit_vars {
            write_atomic(&cwd.join("vars.json"), r#"{"name": "two"}"#);
        }
        common::wait_for_tap(&tap, "could not remove", WATCH_STEP);
        let stderr = tap.finish_text(&mut child);

        assert!(
            stderr.contains("warning: could not remove o/sub/b.md: "),
            "{fixture}: the output is named below the out-dir as typed; stderr: {stderr}"
        );
        assert!(
            sub.join("b.md").is_file(),
            "{fixture}: the output is still there"
        );
        assert_eq!(leak(&stderr, root), None, "{fixture}: stderr: {stderr}");
    }
}

/// An entry typed in another case than its name on a case-insensitive volume: `mds watch`
/// derives its output's file name from the name the volume holds — the file it resolves
/// the entry to — and writes and names the output by it, beside the entry as typed or
/// below `--out-dir` as typed; `mds build` keeps the case as typed.
///
/// Unix-only, and skipped with a reason where the volume tells case apart (Linux, a
/// case-sensitive macOS volume).
#[cfg(unix)]
#[test]
fn an_entry_typed_in_another_case_names_its_output_by_the_name_on_disk() {
    let dir = scratch();
    let root = dir.path();
    put(root, "b/page.mds", "Page\n");
    if !root.join("b/PAGE.mds").exists() {
        eprintln!("skipped: {} tells case apart", root.display());
        return;
    }

    let build = run(&root.join("b"), &["build", "PAGE.mds"]);
    let built = String::from_utf8_lossy(&build.stderr);
    assert!(
        built.lines().any(|line| line == "Compiled to ./PAGE.md"),
        "control: mds build names the output by the case typed; stderr: {built}"
    );

    // (the session's directory, its arguments, the output as named, where it is written)
    for (cwd, args, output, written) in [
        ("w", &["watch", "PAGE.mds"][..], "./page.md", "w"),
        (
            "o",
            &["watch", "PAGE.mds", "--out-dir", "out"],
            "out/page.md",
            "o/out",
        ),
    ] {
        put(root, &format!("{cwd}/page.mds"), "Page\n");
        let (mut child, tap, _) = watch_live(&root.join(cwd), args, false);
        write_atomic(&root.join(cwd).join("page.mds"), "Edited\n");
        common::wait_for_tap(&tap, "Recompiled ", WATCH_STEP);
        let stderr = tap.finish_text(&mut child);

        assert_eq!(
            stderr.lines().next(),
            Some("Watching PAGE.mds"),
            "{args:?}: stderr: {stderr}"
        );
        assert_eq!(
            lines_starting(&stderr, "Compiled to "),
            [format!("Compiled to {output}")],
            "{args:?}: stderr: {stderr}"
        );
        let recompiled = lines_starting(&stderr, "Recompiled ");
        assert!(
            !recompiled.is_empty()
                && recompiled
                    .iter()
                    .all(|line| line.starts_with(&format!("Recompiled {output} ("))),
            "{args:?}: every rebuild names the output by the name on disk; stderr: {stderr}"
        );
        let names: Vec<String> = std::fs::read_dir(root.join(written))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".md"))
            .collect();
        assert_eq!(names, ["page.md"], "{args:?}: written under that name");
        assert_eq!(leak(&stderr, root), None, "{args:?}: stderr: {stderr}");
    }
}

/// `failed to watch vars directory` names the directory as the `--vars` file was typed:
/// a directory that does not exist, and — for a bare file name — the working directory,
/// which the session asks the watcher for as the empty path. Neither file exists, so each
/// session then ends at startup, refusing the `--vars` file.
///
/// The bare name runs on macOS only: its watcher refuses the empty path, while the Linux
/// and Windows watchers resolve it against the working directory, which exists, and watch
/// that, so nothing is printed there.
#[test]
fn a_vars_directory_that_cannot_be_watched_is_named_as_typed() {
    let dir = scratch();
    let root = dir.path();
    put(root, "d/a.mds", "A\n");

    // (the `--vars` argument, the directory the warning names)
    let mut cases = vec![("nodir/v.json", "nodir")];
    if cfg!(target_os = "macos") {
        cases.push(("v.json", "."));
    }
    for (vars, shown) in cases {
        let args = typed_args(&["watch", "d", "--vars", vars]);
        let label = format!("mds {}", args.join(" "));
        let (child, tap, _) = common::spawn_watch_unsynchronized(
            mds_bin()
                .current_dir(root)
                .args(&args)
                .stdout(Stdio::null()),
        );
        let mut child = common::ChildGuard(child);
        // Bounded: the session ends at startup on its own.
        let deadline = Instant::now() + WATCH_STEP;
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("wait for mds watch") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "{label}: the session did not end within {WATCH_STEP:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let stderr = tap.finish_text(&mut child);

        assert!(
            !status.success(),
            "{label}: a missing --vars file ends the session at startup; stderr: {stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "warning: failed to watch vars directory {}: ",
                native(shown)
            )),
            "{label}: the directory is named as the --vars file was typed; stderr: {stderr}"
        );
        assert_eq!(leak(&stderr, root), None, "{label}: stderr: {stderr}");
    }
}

// ── Carve-outs: the text of an error writing an output ───────────────────────

/// `root/rel`'s canonical path, as `mds` prints a path it was given in that form.
#[cfg(unix)]
fn canonical(root: &Path, rel: &str) -> String {
    root.join(rel)
        .canonicalize()
        .expect("the path exists")
        .display()
        .to_string()
}

/// `path`'s refusal as a symlink at an output's path.
#[cfg(unix)]
fn refused(path: &str) -> String {
    format!("cannot write {path}: refusing to replace a symlink")
}

/// Plant a symlink at `root/rel` to a regular file, creating `rel`'s directories.
#[cfg(unix)]
fn plant_symlink(root: &Path, rel: &str) {
    let target = root.join("target.md");
    if !target.exists() {
        put(root, "target.md", "T\n");
    }
    let at = root.join(rel);
    std::fs::create_dir_all(at.parent().expect("a path below the root")).unwrap();
    std::os::unix::fs::symlink(target, at).expect("plant a symlink");
}

/// Carve-out (spec §7.10): the text of an error writing an output names the path the write
/// was given. Below a directory argument's `--out-dir` and below `mds.json`
/// `build.output_dir`, in either mode, that is the canonical path — in `cannot write …` and
/// in `could not remove stale output …` — though the status line names the output as typed.
/// Under `-o`, below a file argument's `--out-dir` and beside the source it is the path as
/// typed (the controls). The cause tempfile gives, `at path "…"`, names the temporary
/// file's absolute path even then.
///
/// Unix-only: it plants symlinks and makes a directory read-only; the read-only arm is
/// skipped with a reason where the mode does not stop a write (running as root).
#[cfg(unix)]
#[test]
fn an_error_writing_below_a_resolved_output_directory_names_the_canonical_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch();
    let root = dir.path();
    put(root, "src/a.mds", "A\n");
    plant_symlink(root, "out/a.md");
    put(root, "proj/mds.json", r#"{"build":{"output_dir":"dist"}}"#);
    put(root, "proj/p.mds", "P\n");
    put(root, "proj/src/q.mds", "Q\n");
    plant_symlink(root, "proj/dist/p.md");
    plant_symlink(root, "proj/dist/q.md");
    put(root, "nts/n.mds", "N\n");
    plant_symlink(root, "nts/n.md");
    plant_symlink(root, "o4/a.md");
    plant_symlink(root, "o5/y.md");

    // (working directory, arguments, the error's text)
    let canonical_cases = [
        (
            ".",
            &["build", "src", "--out-dir", "out"][..],
            refused(&format!("{}/a.md", canonical(root, "out"))),
        ),
        (
            "proj",
            &["build", "p.mds"][..],
            refused(&format!("{}/p.md", canonical(root, "proj/dist"))),
        ),
        (
            "proj",
            &["build", "src"][..],
            refused(&format!("{}/q.md", canonical(root, "proj/dist"))),
        ),
    ];
    for (cwd, args, error) in canonical_cases {
        let out = run(&root.join(cwd), args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            squash(&stderr).contains(&squash(&error)),
            "{args:?}: the error names the canonical path, {error:?}; stderr: {stderr}"
        );
    }

    // A stale `x.md` that is a directory, which no file removal removes, beside the
    // `x.json` a messages template writes now: below `--out-dir` and below
    // `build.output_dir`.
    put(root, "msrc/x.mds", "@message user:\nHi\n@end\n");
    put(root, "stale/x.md/keep", "keep\n");
    put(root, "proj/msrc/x.mds", "@message user:\nHi\n@end\n");
    put(root, "proj/dist/x.md/keep", "keep\n");
    // (working directory, arguments, the status line, the directory the error names)
    for (cwd, args, status, written) in [
        (
            ".",
            &["build", "msrc", "--out-dir", "stale"][..],
            "Compiled to stale/x.json\n",
            "stale",
        ),
        (
            "proj",
            &["build", "msrc"][..],
            "Compiled to msrc/../dist/x.json\n",
            "proj/dist",
        ),
    ] {
        let out = run(&root.join(cwd), args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            stderr.contains(status),
            "{args:?}: the status line names the output as typed; stderr: {stderr}"
        );
        let error = format!(
            "could not remove stale output {}/x.md: ",
            canonical(root, written)
        );
        assert!(
            squash(&stderr).contains(&squash(&error)),
            "{args:?}: the error names the canonical path, {error:?}; stderr: {stderr}"
        );
    }

    // Controls: the write was given the path as typed, and the error names it so.
    for (args, shown) in [
        (&["build", "src/a.mds", "-o", "o5/y.md"][..], "o5/y.md"),
        (&["build", "src/a.mds", "--out-dir", "o4"][..], "o4/a.md"),
        (&["build", "nts/n.mds"][..], "nts/n.md"),
        (&["build", "nts"][..], "nts/n.md"),
    ] {
        let out = run(root, args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: stderr: {stderr}");
        assert!(
            squash(&stderr).contains(&squash(&refused(shown))),
            "{args:?}: the error names the path as typed, {shown:?}; stderr: {stderr}"
        );
        assert_eq!(leak(&stderr, root), None, "{args:?}: stderr: {stderr}");
    }

    // tempfile's cause text: a temporary file in a read-only directory.
    std::fs::create_dir(root.join("ro")).unwrap();
    std::fs::set_permissions(root.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
    let _writable = Writable(root.join("ro"));
    if std::fs::write(root.join("ro/.write-probe"), b"").is_ok() {
        let _ = std::fs::remove_file(root.join("ro/.write-probe"));
        eprintln!("skipped the read-only arm: ro is writable at mode 0o555 (running as root?)");
        return;
    }
    let out = run(root, &["build", "src/a.mds", "-o", "ro/y.md"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
    for part in [
        "cannot create temp file for ro/y.md: ".to_owned(),
        format!("at path \"{}/.mds-tmp-", canonical(root, "ro")),
    ] {
        assert!(
            squash(&stderr).contains(&squash(&part)),
            "{part:?}; stderr: {stderr}"
        );
    }
}

/// Carve-out (spec §7.10): `mds watch` names the path the write was given in the text of
/// an error writing an output — the canonical path beside the entry and beside a directory
/// argument's source, the path as typed under `-o` and below file mode's `--out-dir` (the
/// controls) — and the canonical path below a directory argument's `--out-dir` in the
/// warning that a stale output of the other kind could not be removed, though the
/// `Recompiled` line names the output as typed.
///
/// Unix-only: it plants symlinks.
#[cfg(unix)]
#[test]
fn watch_names_the_path_written_in_an_error_writing_an_output() {
    let dir = scratch();
    let root = dir.path();
    for (source, output) in [
        ("d1/a.mds", "d1/a.md"),
        ("w1/page.mds", "w1/page.md"),
        ("w2/page.mds", "o2/y.md"),
        ("w3/page.mds", "o3/page.md"),
    ] {
        put(root, source, "Page\n");
        plant_symlink(root, output);
    }

    // (arguments, the startup write's refusal, whether it names the path as typed)
    let cases = [
        (
            &["watch", "d1"][..],
            refused(&format!("{}/a.md", canonical(root, "d1"))),
            false,
        ),
        (
            &["watch", "w1/page.mds"][..],
            refused(&format!("{}/page.md", canonical(root, "w1"))),
            false,
        ),
        (
            &["watch", "w2/page.mds", "-o", "o2/y.md"][..],
            refused("o2/y.md"),
            true,
        ),
        (
            &["watch", "w3/page.mds", "--out-dir", "o3"][..],
            refused("o3/page.md"),
            true,
        ),
    ];
    for (args, error, typed) in cases {
        let (child, tap, _) = common::spawn_watch_unsynchronized(
            mds_bin()
                .current_dir(root)
                .args(args)
                .args(["--debounce", "0"])
                .stdout(Stdio::null()),
        );
        let mut child = common::ChildGuard(child);
        // The refusal's last word: an error frame may wrap the line between any two.
        common::wait_for_tap(&tap, "symlink", WATCH_STEP);
        let stderr = tap.finish_text(&mut child);
        assert!(
            squash(&stderr).contains(&squash(&error)),
            "{args:?}: {error:?}; stderr: {stderr}"
        );
        if typed {
            assert_eq!(leak(&stderr, root), None, "{args:?}: stderr: {stderr}");
        }
    }

    // The stale output this session wrote, replaced by a directory no file removal
    // removes, when its source turns into a messages template.
    put(root, "d5/a.mds", "A\n");
    let (mut child, tap, _) = watch_live(root, &["watch", "d5", "--out-dir", "o5"], false);
    std::fs::remove_file(root.join("o5/a.md")).expect("remove the startup output");
    put(root, "o5/a.md/keep", "keep\n");
    write_atomic(&root.join("d5/a.mds"), "@message user:\nHi\n@end\n");
    common::wait_for_tap(&tap, "stale", WATCH_STEP);
    let stderr = tap.finish_text(&mut child);
    let warning = format!(
        "warning: could not remove stale output {}/a.md: ",
        canonical(root, "o5")
    );
    assert!(
        squash(&stderr).contains(&squash(&warning)),
        "{warning:?}; stderr: {stderr}"
    );
    assert!(
        stderr.contains("Recompiled o5/a.json ("),
        "the rebuild names the output as typed; stderr: {stderr}"
    );
}

// ── Windows ──────────────────────────────────────────────────────────────────

/// Windows: a directory build under an out-dir that already exists — the case in which
/// canonicalizing it yields a verbatim `\\?\` path — names the output and its map below
/// the out-dir as typed, with no verbatim prefix anywhere.
#[cfg(windows)]
#[test]
fn compiled_to_and_map_lines_under_an_existing_out_dir_carry_no_verbatim_prefix() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");
    std::fs::create_dir(dir.path().join("out")).unwrap();
    // Control: canonicalizing the out-dir IS verbatim on this host, so the absence
    // assertions below can fail.
    assert!(
        dir.path()
            .join("out")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .starts_with(r"\\?\"),
        "test assumption: canonicalize must yield a verbatim path on Windows"
    );

    let out = run(
        dir.path(),
        &["build", "src", "--out-dir", "out", "--source-map"],
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(
        stderr,
        format!(
            "Compiled to {}\nSource map written to {}\n1 built, 0 failed\n",
            native("out/sub/page.md"),
            native("out/sub/page.md.map")
        ),
    );
    assert!(!stdout.contains(r"\\?\"), "stdout: {stdout}");
    assert!(!stderr.contains(r"\\?\"), "stderr: {stderr}");
    assert_eq!(leak(&stderr, dir.path()), None, "stderr: {stderr}");
}

/// Windows: `mds watch` matches events against canonical — verbatim `\\?\` — paths, yet
/// its banner and a rebuild's `Recompiled` line name the argument, and the output below
/// it, as typed, with no verbatim prefix, in directory mode under an out-dir that already
/// exists and in file mode.
#[cfg(windows)]
#[test]
fn watch_banner_and_recompiled_lines_carry_no_verbatim_prefix() {
    let dir = scratch();
    put(dir.path(), "src/sub/page.mds", "Page\n");
    std::fs::create_dir(dir.path().join("out")).unwrap();
    // Control: canonicalizing IS verbatim on this host, so the absence assertions below
    // can fail.
    assert!(
        dir.path()
            .join("src")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .starts_with(r"\\?\"),
        "test assumption: canonicalize must yield a verbatim path on Windows"
    );

    // (arguments, the banner, the output's name)
    let cases: [(&[&str], &str, &str); 2] = [
        (
            &["watch", "src", "--out-dir", "out"],
            "Watching directory src",
            "out/sub/page.md",
        ),
        (
            &["watch", "src/sub/page.mds"],
            "Watching src/sub/page.mds",
            "src/sub/page.md",
        ),
    ];
    for (args, banner, output) in cases {
        let args = typed_args(args);
        let (mut child, tap, _) = watch_live(dir.path(), &args, false);
        write_atomic(&dir.path().join(native("src/sub/page.mds")), "Edited\n");
        common::wait_for_tap(&tap, "Recompiled ", WATCH_STEP);
        let stderr = tap.finish_text(&mut child);

        assert_eq!(
            stderr.lines().next(),
            Some(native(banner).as_str()),
            "{args:?}: stderr: {stderr}"
        );
        let prefix = format!("Recompiled {} (", native(output));
        let recompiled = lines_starting(&stderr, "Recompiled ");
        assert!(
            !recompiled.is_empty() && recompiled.iter().all(|line| line.starts_with(&prefix)),
            "{args:?}: every rebuild names the output as {prefix:?}; stderr: {stderr}"
        );
        assert!(!stderr.contains(r"\\?\"), "{args:?}: stderr: {stderr}");
        assert_eq!(
            leak(&stderr, dir.path()),
            None,
            "{args:?}: stderr: {stderr}"
        );
    }
}

// ── spec.md §7.10: the table of these lines ──────────────────────────────────

/// The heading of the section of spec.md that holds the path-label table.
const LABEL_SECTION: &str = "### 7.10 Path labels";

/// The table's header row, cell by cell.
const LABEL_COLUMNS: [&str; 4] = ["Command", "Line", "Form", "Pinned by"];

/// The sweep the table cites for many rows.
const SWEEP: &str = "no_listed_build_check_or_lint_run_names_the_working_directory";

/// The tests each row of spec.md §7.10's table cites, row by row, in the table's order.
/// The table and this list change together: a row added, removed or moved, or a test
/// cited or no longer cited, fails [`the_spec_s_path_label_table_cites_exactly_these_tests`]
/// until the other follows.
const LABEL_TABLE: &[&[&str]] = &[
    &["auto_detection_names_the_file_as_found_in_the_working_directory"],
    &[SWEEP],
    &[SWEEP],
    &[SWEEP],
    &[SWEEP],
    &[
        "compiled_to_under_a_relative_out_dir_names_the_out_dir_as_typed",
        "compiled_to_under_a_symlinked_out_dir_names_the_link_not_its_target",
        "dir_build_out_dir_status_line_names_the_out_dir_as_typed",
    ],
    &["a_config_output_dir_is_named_through_the_directory_mds_json_was_reached_by"],
    &[
        "source_map_lines_under_an_out_dir_name_the_out_dir_as_typed",
        "a_config_output_dir_names_the_map_written_and_the_stale_map_removed",
        SWEEP,
    ],
    &["a_config_output_dir_names_the_map_written_and_the_stale_map_removed"],
    &["an_output_directory_that_cannot_be_created_is_named_as_its_output_is"],
    &["the_config_source_map_warning_names_mds_json_as_reached"],
    &["r3_error_frame_names_root_relative_path_for_subdir_file"],
    &["an_error_writing_below_a_resolved_output_directory_names_the_canonical_path"],
    &[SWEEP],
    &["fmt_names_a_file_argument_as_typed_and_a_directory_s_entries_below_it"],
    &["fmt_names_a_file_argument_as_typed_and_a_directory_s_entries_below_it"],
    &["clean_names_a_file_argument_as_typed", SWEEP],
    &[
        "lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name",
        "a_partial_fix_of_a_file_is_announced_after_the_findings_it_leaves",
        SWEEP,
    ],
    &["lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name"],
    &[
        "lint_names_a_file_argument_as_typed_and_its_findings_by_its_file_name",
        SWEEP,
    ],
    &[
        "watching_names_the_entry_and_the_directory_as_typed",
        "watching_names_a_path_reached_through_a_symlink_by_the_link",
    ],
    &[
        "watching_names_the_entry_and_the_directory_as_typed",
        "watching_names_a_path_reached_through_a_symlink_by_the_link",
        "watch_dot_forms_watch_the_canonical_directory",
    ],
    &[
        "watch_startup_names_an_out_dir_and_a_config_output_dir_as_build_does",
        "recompiled_and_removed_name_each_output_as_typed",
    ],
    &[
        "recompiled_and_removed_name_each_output_as_typed",
        "watching_names_a_path_reached_through_a_symlink_by_the_link",
    ],
    &[
        "recompiled_and_removed_name_each_output_as_typed",
        "removed_names_the_output_as_typed_when_the_vars_file_changes_in_the_same_batch",
    ],
    &["an_output_that_cannot_be_removed_is_named_as_typed"],
    &["an_entry_typed_in_another_case_names_its_output_by_the_name_on_disk"],
    &["a_watched_directory_is_named_as_the_user_typed_it"],
    &[
        "a_vars_directory_that_cannot_be_watched_is_named_as_typed",
        "a_watched_directory_is_named_as_the_user_typed_it",
    ],
    &["a_watched_directory_is_named_as_the_user_typed_it"],
    &["watch_names_the_path_written_in_an_error_writing_an_output"],
    &["init_names_the_file_it_creates_as_typed"],
    &[
        "a_relative_output_location_is_refused_where_the_working_directory_is_gone",
        "watch_with_a_relative_out_dir_stops_at_startup_where_the_working_directory_is_gone",
    ],
    &["auto_detection_in_a_working_directory_it_cannot_list_names_it_as_a_dot"],
    &[
        "compiled_to_and_map_lines_under_an_existing_out_dir_carry_no_verbatim_prefix",
        "dir_build_out_dir_status_line_has_no_verbatim_prefix_on_windows",
    ],
    &["watch_banner_and_recompiled_lines_carry_no_verbatim_prefix"],
];

/// One body row of the table: the spec.md line it is on (from 1) and the tests it cites.
#[derive(Debug, PartialEq)]
struct LabelRow {
    line: usize,
    tests: Vec<String>,
}

/// The cells of a table line, `| a | b |` → `["a", "b"]`: split at each `|` not escaped
/// as `\|`, each cell trimmed.
fn table_cells(line: &str) -> Vec<String> {
    let inner = line.trim();
    let inner = inner.strip_prefix('|').unwrap_or(inner);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for c in inner.chars() {
        if c == '|' && !escaped {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(c);
        }
        escaped = c == '\\' && !escaped;
    }
    cells.push(cell.trim().to_owned());
    cells
}

/// Is `name` spelled as a test function is: lower-case ASCII letters, digits and `_`,
/// starting with a letter?
fn is_test_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The tests a `Pinned by` cell cites: one or more test names, each in backticks,
/// separated by `, `. Anything else — an empty cell, a dash, a name without backticks,
/// prose — is refused, so no row can stand without a test.
fn cited_tests(cell: &str) -> Result<Vec<String>, String> {
    if cell.is_empty() {
        return Err("the Pinned by cell cites no test".to_owned());
    }
    cell.split(", ")
        .map(|item| {
            item.strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
                .filter(|name| is_test_name(name))
                .map(str::to_owned)
                .ok_or_else(|| format!("{item:?} is not a test name in backticks"))
        })
        .collect()
}

/// The body rows of the table in `spec`'s §7.10. The section must appear once and hold
/// one table, headed by [`LABEL_COLUMNS`], whose every row has four cells, none of them
/// empty, and cites at least one test. A line may end in CRLF.
fn label_table(spec: &str) -> Result<Vec<LabelRow>, String> {
    let lines: Vec<&str> = spec.lines().map(str::trim_end).collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == LABEL_SECTION)
        .map(|(at, _)| at)
        .collect();
    let [start] = starts.as_slice() else {
        return Err(format!(
            "expected one {LABEL_SECTION:?} heading, found {}",
            starts.len()
        ));
    };
    // The section runs to the next heading or horizontal rule. Line numbers count from 1.
    let table: Vec<(usize, &str)> = lines[start + 1..]
        .iter()
        .enumerate()
        .map(|(at, line)| (start + 2 + at, *line))
        .take_while(|(_, line)| !line.starts_with('#') && *line != "---")
        .filter(|(_, line)| line.starts_with('|'))
        .collect();
    let (Some(&(first, header)), Some(&(last, _))) = (table.first(), table.last()) else {
        return Err("the section holds no table".to_owned());
    };
    if table.len() != last - first + 1 {
        return Err(format!(
            "the table lines {first}..={last} are not one table: other lines lie between them"
        ));
    }
    if table_cells(header) != LABEL_COLUMNS {
        return Err(format!(
            "line {first}: the header is {:?}, not {LABEL_COLUMNS:?}",
            table_cells(header)
        ));
    }
    let is_rule = |cell: &String| {
        let dashes = cell.trim_start_matches(':').trim_end_matches(':');
        dashes.len() >= 3 && dashes.chars().all(|c| c == '-')
    };
    match table.get(1) {
        Some(&(_, separator))
            if table_cells(separator).len() == LABEL_COLUMNS.len()
                && table_cells(separator).iter().all(is_rule) => {}
        _ => return Err(format!("line {}: no separator row", first + 1)),
    }
    let rows = table[2..]
        .iter()
        .map(|&(line, text)| {
            let cells = table_cells(text);
            if cells.len() != LABEL_COLUMNS.len() {
                return Err(format!(
                    "line {line}: {} cells, not {}",
                    cells.len(),
                    LABEL_COLUMNS.len()
                ));
            }
            if let Some(empty) = cells[..3].iter().position(String::is_empty) {
                return Err(format!(
                    "line {line}: the {} cell is empty",
                    LABEL_COLUMNS[empty]
                ));
            }
            let tests = cited_tests(&cells[3]).map_err(|e| format!("line {line}: {e}"))?;
            Ok(LabelRow { line, tests })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if rows.is_empty() {
        return Err(format!("line {first}: the table has no rows"));
    }
    Ok(rows)
}

/// Every `.rs` file directly under `crates/mds-cli/tests/` and `crates/mds-cli/src/`,
/// with its text.
fn cli_rust_sources() -> Vec<(PathBuf, String)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    for sub in ["tests", "src"] {
        for entry in std::fs::read_dir(crate_dir.join(sub)).expect("list a source directory") {
            let path = entry.expect("read a directory entry").path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("read a source file");
                sources.push((path, text));
            }
        }
    }
    sources.sort();
    sources
}

/// The files of `sources` that define a test named `name`: a line `fn <name>() {` with
/// `#[test]` among the attribute and doc-comment lines right above it.
fn test_definitions<'a>(name: &str, sources: &'a [(PathBuf, String)]) -> Vec<&'a Path> {
    let signature = format!("fn {name}() {{");
    sources
        .iter()
        .filter(|(_, text)| {
            let lines: Vec<&str> = text.lines().map(str::trim).collect();
            lines
                .iter()
                .enumerate()
                .filter(|(_, line)| **line == signature)
                .any(|(at, _)| {
                    lines[..at]
                        .iter()
                        .rev()
                        .take_while(|line| line.starts_with("#[") || line.starts_with("///"))
                        .any(|line| *line == "#[test]")
                })
        })
        .map(|(path, _)| path.as_path())
        .collect()
}

/// spec.md §7.10's table and [`LABEL_TABLE`] cite the same tests, row by row, and each
/// test they cite is a `#[test]` function defined once among mds-cli's test and source
/// files — so a row added or removed, a test cited or dropped, and a cited test renamed or
/// deleted each fail here.
#[test]
fn the_spec_s_path_label_table_cites_exactly_these_tests() {
    let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec.md");
    let spec = std::fs::read_to_string(&spec_path).expect("read spec.md");
    let rows = label_table(&spec).unwrap_or_else(|e| panic!("spec.md §7.10: {e}"));

    let cited: BTreeSet<&str> = rows
        .iter()
        .flat_map(|row| row.tests.iter().map(String::as_str))
        .collect();
    let listed: BTreeSet<&str> = LABEL_TABLE
        .iter()
        .flat_map(|row| row.iter().copied())
        .collect();
    let unlisted: Vec<&str> = cited.difference(&listed).copied().collect();
    let uncited: Vec<&str> = listed.difference(&cited).copied().collect();
    assert!(
        unlisted.is_empty() && uncited.is_empty(),
        "spec.md §7.10 cites tests LABEL_TABLE does not list: {unlisted:?}; \
         LABEL_TABLE lists tests spec.md §7.10 does not cite: {uncited:?}"
    );

    let table: Vec<Vec<&str>> = rows
        .iter()
        .map(|row| row.tests.iter().map(String::as_str).collect())
        .collect();
    let lines: Vec<usize> = rows.iter().map(|row| row.line).collect();
    assert_eq!(
        table, LABEL_TABLE,
        "spec.md §7.10's rows (lines {lines:?}) and LABEL_TABLE differ row by row"
    );

    let sources = cli_rust_sources();
    let undefined: Vec<String> = listed
        .iter()
        .filter_map(|name| {
            let defined = test_definitions(name, &sources);
            (defined.len() != 1).then(|| format!("{name}: defined in {defined:?}"))
        })
        .collect();
    assert!(
        undefined.is_empty(),
        "each test spec.md §7.10 cites must be a #[test] fn defined once in \
         crates/mds-cli/tests/*.rs or crates/mds-cli/src/*.rs: {undefined:#?}"
    );
}

/// The table check refuses each way a row could stand without a test, reads a CRLF spec
/// as an LF one, and finds a test only where one `#[test]` function of that name is
/// defined (positive controls).
#[test]
fn the_table_check_refuses_a_row_without_a_test_and_finds_only_defined_tests() {
    let spec = |rows: &str| {
        format!(
            "## 7. CLI\n\n{LABEL_SECTION}\n\nProse.\n\n| Command | Line | Form | Pinned by |\n\
             |---|---|---|---|\n{rows}\n\n---\n\n## 8. Next\n\n| a | b |\n|---|---|\n"
        )
    };
    let good = spec("| `mds x` | `X` | as typed | `a_test`, `b_test` |");
    assert_eq!(
        label_table(&good),
        Ok(vec![LabelRow {
            line: 9,
            tests: vec!["a_test".to_owned(), "b_test".to_owned()],
        }])
    );
    assert_eq!(
        label_table(&good.replace('\n', "\r\n")),
        label_table(&good),
        "a CRLF spec reads as the LF one"
    );
    assert!(
        label_table(&spec("| `mds x` | `a \\| b` | as typed | `a_test` |")).is_ok(),
        "an escaped pipe stays inside its cell"
    );
    for (rows, what) in [
        ("| `mds x` | `X` | as typed | — |", "a dash"),
        ("| `mds x` | `X` | as typed |  |", "an empty Pinned by cell"),
        ("| `mds x` | `X` | as typed | a_test |", "a name without backticks"),
        ("| `mds x` | `X` | as typed | `a_test` and prose |", "prose"),
        ("| `mds x` | `X` | as typed | `a_test`,`b_test` |", "another separator"),
        ("| `mds x` | `X` | as typed | `A_Test` |", "a name no test has"),
        ("| `mds x` | `X` | `a_test` |", "three cells"),
        ("| `mds x` |  | as typed | `a_test` |", "an empty Line cell"),
        (
            "| `mds x` | `X` | as typed | `a_test` |\n\nProse.\n\n| `mds y` | `Y` | as typed | `b_test` |",
            "a second table",
        ),
    ] {
        assert!(
            label_table(&spec(rows)).is_err(),
            "{what} must be refused: {rows:?}"
        );
    }
    assert!(label_table("# spec\n").is_err(), "no section");
    assert!(
        label_table(&format!("{good}{good}")).is_err(),
        "the section twice"
    );
    assert!(
        label_table(&good.replace("| Pinned by |", "| Tests |")).is_err(),
        "another header"
    );
    assert!(label_table(&spec("")).is_err(), "a table with no rows");

    let planted =
        vec![
        (
            PathBuf::from("a.rs"),
            "/// Doc.\n#[cfg(unix)]\n#[test]\nfn planted_test() {\n}\n\nfn planted_helper() {\n}\n"
                .to_owned(),
        ),
        (PathBuf::from("b.rs"), "#[test]\nfn twice() {\n}\n".to_owned()),
        (PathBuf::from("c.rs"), "#[test]\nfn twice() {\n}\n".to_owned()),
    ];
    assert_eq!(
        test_definitions("planted_test", &planted),
        [Path::new("a.rs")]
    );
    assert!(
        test_definitions("planted_helper", &planted).is_empty(),
        "a function without #[test] is no test"
    );
    assert!(
        test_definitions("planted", &planted).is_empty(),
        "no prefix match"
    );
    assert_eq!(
        test_definitions("twice", &planted).len(),
        2,
        "a test defined twice is found twice, which the table check refuses"
    );
    let sources = cli_rust_sources();
    let here = test_definitions("clean_names_a_file_argument_as_typed", &sources);
    assert!(
        here.len() == 1 && here[0].ends_with("path_labels.rs"),
        "a real test is found where it is defined: {here:?}"
    );
    assert!(
        test_definitions("scratch", &sources).is_empty(),
        "a real helper is no test"
    );
}
