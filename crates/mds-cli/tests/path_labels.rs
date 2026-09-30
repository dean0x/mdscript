//! Every path `mds build`, `mds check` and `mds lint` name is the path as typed, or the
//! part below a directory the user named, as typed — the directory argument, `--out-dir`,
//! or the directory `mds.json` was reached by — never a canonical or absolute spelling
//! the user did not type (#390).
//!
//! Each run starts in a scratch directory with relative arguments, so the scratch
//! directory's own absolute path has no business in any output: [`leak`] looks for it in
//! every spelling it can take. Every absence check sits beside a check that the line it
//! is about was printed, so no test passes on a run that printed nothing.
//!
//! An expected path is written with `/` and printed through [`native`], so it names the
//! path in the platform's separator, exactly as `mds` prints it.

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use common::mds_bin;

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

/// `mds <args>` in `cwd`, stdin empty.
fn run(cwd: &Path, args: &[&str]) -> Output {
    mds_bin()
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run mds")
}

/// `mds <args>` in `cwd`, with `input` on stdin.
fn run_with_stdin(cwd: &Path, args: &[&str], input: &'static str) -> Output {
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
        let out = run(&proj, args);
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
        let out = run(&proj, &args);
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

    let out = run(dir.path(), &["lint", "sub/page.mds"]);
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

// ── Every build, check and lint surface ──────────────────────────────────────

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

/// Every build, check and lint surface, run from the scratch directory with relative
/// arguments, names no spelling of the scratch directory on stdout or stderr — and
/// printed the line that shows its sink ran.
#[test]
fn no_build_check_or_lint_run_with_relative_arguments_names_the_working_directory() {
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
        let out = match row.stdin {
            Some(input) => run_with_stdin(&cwd, row.args, input),
            None => run(&cwd, row.args),
        };
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        let label = format!("(in {}) mds {}", row.cwd, row.args.join(" "));
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
/// (both modes) is named as `mds build` names it. The rest of watch's own lines are not
/// pinned here.
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
