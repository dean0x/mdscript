mod common;

use common::mds_bin;
use std::fs;
use std::io::Write;
use std::path::{Path, MAIN_SEPARATOR_STR};
use std::process::Stdio;

const MARKDOWN: &str = "Hello!\n";
const EMBED_WARNING: &str = "warning: embed_sources has no effect without source maps; \
                             set build.source_map=true or pass --source-map\n";
const INLINE_WARNING: &str = "warning: --embed-sources with --inline ships full source text \
                              in the output (AC-SEC-02)\n";
const CARRIER: &str = "<!--# sourceMappingURL=data:application/json;base64,";

fn project(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("page.mds"), source).unwrap();
    fs::write(dir.path().join("mds.json"), "{}").unwrap();
    dir
}

fn build(dir: &Path, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut cmd = mds_bin();
    cmd.current_dir(dir)
        .env("NO_GRAPHICS", "0")
        .arg("build")
        .args(args);
    let output = if let Some(source) = stdin {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    } else {
        cmd.stdin(Stdio::null()).output().unwrap()
    };
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn stdin_and_file_warn_once_about_inline_embed_unless_quiet() {
    for (input, stdin) in [("-", Some(MARKDOWN)), ("page.mds", None)] {
        let dir = project(MARKDOWN);
        let mut args = vec![
            input,
            "-o",
            "out.md",
            "--source-map",
            "--inline",
            "--embed-sources",
        ];
        let (code, stdout, stderr) = build(dir.path(), &args, stdin);
        assert_eq!(code, 0, "{input}: {stderr}");
        assert_eq!(stdout, "", "{input}");
        assert_eq!(
            stderr,
            format!("{INLINE_WARNING}Compiled to out.md\n"),
            "{input}"
        );
        let content = fs::read_to_string(dir.path().join("out.md")).unwrap();
        assert!(content.starts_with(MARKDOWN), "{content}");
        assert!(
            content.lines().last().unwrap().starts_with(CARRIER),
            "{content}"
        );

        args.push("--quiet");
        let (code, stdout, stderr) = build(dir.path(), &args, stdin);
        assert_eq!(code, 0, "{input}: {stderr}");
        assert_eq!(stdout, "", "{input}");
        assert_eq!(stderr, "", "{input}");
    }
}

#[test]
fn cli_rejects_inline_embed_without_the_source_map_flag() {
    for input in ["-", "page.mds"] {
        let dir = project(MARKDOWN);
        let (code, stdout, stderr) = build(
            dir.path(),
            &[input, "-o", "out.md", "--inline", "--embed-sources"],
            None,
        );
        assert_eq!(code, 2, "{input}: {stderr}");
        assert_eq!(stdout, "", "{input}");
        assert_eq!(
            stderr,
            format!(
                concat!(
                    "error: the following required arguments were not provided:\n  --source-map\n\n",
                    "Usage: mds{} build --source-map --output <OUTPUT> --inline --embed-sources <INPUT>\n\n",
                    "For more information, try '--help'.\n",
                ),
                std::env::consts::EXE_SUFFIX,
            ),
            "{input}"
        );
        assert!(!dir.path().join("out.md").exists(), "{input}");
    }
}

#[test]
fn directory_warns_only_without_maps_and_embeds_the_inline_map_when_enabled() {
    let dir = project(MARKDOWN);
    fs::write(
        dir.path().join("mds.json"),
        r#"{"build":{"embed_sources":true}}"#,
    )
    .unwrap();
    let args = [".", "--out-dir", "out"];
    let status = format!("Compiled to out{MAIN_SEPARATOR_STR}page.md");
    let output = dir.path().join("out").join("page.md");

    let (code, stdout, stderr) = build(dir.path(), &args, None);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [EMBED_WARNING.trim_end(), &status, "1 built, 0 failed"]
    );
    assert_eq!(fs::read_to_string(&output).unwrap(), MARKDOWN);

    let (code, stdout, stderr) = build(
        dir.path(),
        &[
            ".",
            "--out-dir",
            "out",
            "--source-map",
            "--inline",
            "--embed-sources",
        ],
        None,
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [&status, "1 built, 0 failed"]
    );
    let content = fs::read_to_string(output).unwrap();
    assert!(content.starts_with(MARKDOWN), "{content}");
    assert!(
        content.lines().last().unwrap().starts_with(CARRIER),
        "{content}"
    );
}

#[test]
fn auto_detect_banner_precedes_flag_compile_and_extension_warnings() {
    let dir = project("@import \"./parts/_empty.mds\" as e\n@include e\nHello!\n");
    fs::create_dir(dir.path().join("parts")).unwrap();
    fs::write(dir.path().join("parts").join("_empty.mds"), "").unwrap();

    fs::write(
        dir.path().join("mds.json"),
        r#"{"build":{"embed_sources":true}}"#,
    )
    .unwrap();

    for (flags, warning) in [
        (&[][..], EMBED_WARNING),
        (
            &["--source-map", "--inline", "--embed-sources"][..],
            INLINE_WARNING,
        ),
    ] {
        let mut args = vec!["-o", "out.txt"];
        args.extend_from_slice(flags);
        let (code, stdout, stderr) = build(dir.path(), &args, None);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        assert_eq!(stdout, "", "{args:?}");
        assert_eq!(
            stderr,
            format!(
                "Building page.mds\n{warning}\
             warning: @include of 'e' produced empty output — module has no body text; \
             to use the module's functions, call them directly: {{{{e.fn()}}}}\n\
             warning: output path 'out.txt' has extension '.txt' but compiled output is \
             markdown (.md); writing to 'out.txt' anyway\n\
             Compiled to out.txt\n"
            ),
            "{args:?}"
        );
        let content = fs::read_to_string(dir.path().join("out.txt")).unwrap();
        if flags.is_empty() {
            assert_eq!(content, MARKDOWN);
        } else {
            assert!(content.starts_with(MARKDOWN), "{content}");
            assert!(
                content.lines().last().unwrap().starts_with(CARRIER),
                "{content}"
            );
        }
    }
}

#[test]
fn sidecar_to_stdout_writes_messages_without_a_map() {
    const MESSAGES: &str = "@message user:\nHello!\n@end\n";
    for (input, stdin) in [("-", Some(MESSAGES)), ("page.mds", None)] {
        let dir = project(MESSAGES);
        let (code, stdout, stderr) = build(dir.path(), &[input, "-o", "-", "--source-map"], stdin);
        assert_eq!(code, 0, "{input}: {stderr}");
        assert_eq!(
            stdout, "[\n  {\n    \"role\": \"user\",\n    \"content\": \"Hello!\"\n  }\n]\n",
            "{input}"
        );
        assert_eq!(stderr, "source maps are not supported for messages-mode templates (@message blocks); no source map will be generated\n", "{input}");
    }
}

#[test]
fn explicit_markdown_sidecar_to_stdout_is_refused() {
    for (input, stdin) in [("-", Some(MARKDOWN)), ("page.mds", None)] {
        let dir = project(MARKDOWN);
        let (code, stdout, stderr) = build(dir.path(), &[input, "-o", "-", "--source-map"], stdin);
        assert_eq!(code, 1, "{input}: {stderr}");
        assert_eq!(stdout, "", "{input}");
        assert_eq!(stderr, "  × --source-map (sidecar) requires an output file; use -o <file> or --out-\n  │ dir, --inline to embed the map for stdout, or --no-source-map to skip it\n\n", "{input}");
    }
}

#[test]
fn configured_markdown_sidecar_to_stdout_warns_and_writes_the_markdown() {
    let dir = project(MARKDOWN);
    fs::write(
        dir.path().join("mds.json"),
        r#"{"build":{"source_map":true}}"#,
    )
    .unwrap();

    let (code, stdout, stderr) = build(dir.path(), &["page.mds", "-o", "-"], None);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, MARKDOWN);
    assert_eq!(
        stderr,
        format!(
            "warning: source_map in .{MAIN_SEPARATOR_STR}mds.json has no effect when writing to \
         stdout (sidecar requires -o <file> or --out-dir); use --inline to embed the map, \
         or --no-source-map to silence this warning\n"
        )
    );
}

#[test]
fn sidecar_announcement_follows_the_output_announcement() {
    for (input, stdin) in [("-", Some(MARKDOWN)), ("page.mds", None)] {
        let dir = project(MARKDOWN);
        let (code, stdout, stderr) =
            build(dir.path(), &[input, "-o", "out.md", "--source-map"], stdin);
        assert_eq!(code, 0, "{input}: {stderr}");
        assert_eq!(stdout, "", "{input}");
        assert_eq!(
            stderr, "Compiled to out.md\nSource map written to out.md.map\n",
            "{input}"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("out.md")).unwrap(),
            MARKDOWN
        );
        let map = fs::read(dir.path().join("out.md.map")).unwrap();
        let map: serde_json::Value = serde_json::from_slice(&map).unwrap();
        assert_eq!(map["file"], "out.md");
    }
}

#[test]
fn file_removes_a_stale_sidecar_but_stdin_leaves_it_unchanged() {
    for (input, stdin) in [("-", Some(MARKDOWN)), ("page.mds", None)] {
        for flags in [&[][..], &["--source-map", "--inline"][..]] {
            let dir = project(MARKDOWN);
            let map = dir.path().join("out.md.map");
            let (code, _, stderr) =
                build(dir.path(), &[input, "-o", "out.md", "--source-map"], stdin);
            assert_eq!(code, 0, "fixture: {input}: {stderr}");
            let before = fs::read(&map).unwrap();
            assert!(!before.is_empty());
            let mut args = vec![input, "-o", "out.md"];
            args.extend_from_slice(flags);

            let (code, stdout, stderr) = build(dir.path(), &args, stdin);
            assert_eq!(code, 0, "{args:?}: {stderr}");
            assert_eq!(stdout, "", "{args:?}");
            let content = fs::read_to_string(dir.path().join("out.md")).unwrap();
            if flags.is_empty() {
                assert_eq!(content, MARKDOWN);
            } else {
                assert!(content.starts_with(MARKDOWN), "{content}");
                assert!(
                    content.lines().last().unwrap().starts_with(CARRIER),
                    "{content}"
                );
            }
            if stdin.is_some() {
                assert_eq!(stderr, "Compiled to out.md\n", "{args:?}");
                assert_eq!(fs::read(map).unwrap(), before, "{args:?}");
            } else {
                assert_eq!(
                    stderr, "Compiled to out.md\nRemoved stale map out.md.map\n",
                    "{args:?}"
                );
                assert!(!map.exists(), "{args:?}");
            }
        }
    }
}

#[test]
fn directory_sidecar_failure_counts_once_and_keeps_the_empty_count() {
    let dir = project("");
    let output = dir.path().join("out");
    let map = output.join("page.md.map");
    fs::create_dir_all(&map).unwrap();
    // Use the operating system's own cause for a rename over a directory.
    let probe = tempfile::NamedTempFile::new_in(&output).unwrap();
    let cause = fs::rename(probe.path(), &map).unwrap_err();
    drop(probe);

    let (code, stdout, stderr) =
        build(dir.path(), &[".", "--out-dir", "out", "--source-map"], None);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr.lines().collect::<Vec<_>>(),
        [
            &format!("Compiled to out{MAIN_SEPARATOR_STR}page.md"),
            "mds::io",
            "",
            &format!("  × cannot write out{MAIN_SEPARATOR_STR}page.md.map: {cause}"),
            "",
            "0 built (1 empty), 1 failed",
        ]
    );
    assert_eq!(fs::read(output.join("page.md")).unwrap(), b"");
    assert!(map.is_dir());
}
