//! Integration tests for local data sources in the CLI (#405): `mds build` binds the
//! entries, and `mds check`, `mds lint`, `mds lint --fix` and `mds fmt` read no data
//! file. `mds watch` is tested in `cli_watch.rs`.

mod common;
use common::{count_occurrences, mds_bin};

use std::fs;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

/// A `.json`, a `.yaml` and a `.yml` entry, each bound under its name: the fixture of
/// the napi, WASM and Python data tests.
const DATA_FIXTURE: &str = concat!(
    "---\ndata:\n  cfg: ./cfg.json\n  list: ./list.yaml\n  more: ./more.yml\n---\n",
    "{{cfg.name}}: {{list.count}} {{more.unit}}\n",
);

/// The data files of [`DATA_FIXTURE`].
const DATA_FILES: [(&str, &str); 3] = [
    ("cfg.json", "{\"name\": \"Ada\"}\n"),
    ("list.yaml", "count: 3\n"),
    ("more.yml", "unit: items\n"),
];

/// The output of [`DATA_FIXTURE`] over [`DATA_FILES`].
const RENDERED: &str = "Ada: 3 items\n";

/// Text that no data parser accepts: a read of it fails with `mds::yaml`.
const BAD_DATA: &str = "[";

/// The help that names the way to replay a denied entry.
const REPLAY_HELP: &str =
    "provide a runtime value for each entry to replay it without network access";

/// Why an output is not written over a file the run reads.
const INPUT_CAUSE: &str = "refusing to replace a file this run reads";

/// A project directory (a `.mdsroot` marker) that holds `files`.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".mdsroot"), "").unwrap();
    for (name, text) in files {
        fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

/// Run `mds <args>` in `dir` with no stdin.
fn run(dir: &Path, args: &[&str]) -> Output {
    mds_bin()
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The exit code and the stdout of `out`.
fn code_and_stdout(out: &Output) -> (Option<i32>, String) {
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `text` without whitespace or miette's frame marker, so that a message miette wrapped
/// compares equal to the unwrapped one.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{2502}')
        .collect()
}

/// Whether `stderr` refuses, as `mds::io`, a write of `shown` with [`INPUT_CAUSE`].
fn refuses(stderr: &str, shown: &Path) -> bool {
    stderr.contains("mds::io")
        && squash(stderr).contains(&squash(&format!(
            "cannot write {}: {INPUT_CAUSE}",
            shown.display()
        )))
}

/// Whether `listener` (non-blocking) accepts a connection before `timeout` elapses.
fn accepts_within(listener: &TcpListener, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok(_) => return true,
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

/// `mds build` binds a `.json`, a `.yaml` and a `.yml` entry under their names, to
/// stdout and to the default output file (#405, AC-8).
#[test]
fn build_binds_json_and_yaml_entries_to_the_expected_bytes() {
    let mut files = DATA_FILES.to_vec();
    files.push(("main.mds", DATA_FIXTURE));
    let dir = project(&files);

    let out = run(dir.path(), &["build", "main.mds", "-o", "-"]);
    assert_eq!(
        code_and_stdout(&out),
        (Some(0), RENDERED.to_owned()),
        "stderr: {}",
        stderr_of(&out)
    );
    let out = run(dir.path(), &["build", "main.mds"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        fs::read_to_string(dir.path().join("main.md")).unwrap(),
        RENDERED
    );
}

/// A `data` header never reaches the `mds build` output: a marker in its value is absent
/// in the plain block form, the block form with a column-0 comment, and the one-line flow
/// form. A quoted or flow `data` key fails with the fixed form message (#405). Controls:
/// the same marker under the key `info` stays, and a quoted `info` key compiles.
#[test]
fn build_omits_every_plain_data_header_and_refuses_the_other_forms() {
    const MARKER: &str = "./fixture-marker.yml";
    let dir = project(&[("fixture-marker.yml", "Local")]);
    let build = |source: &str| {
        fs::write(dir.path().join("main.mds"), source).unwrap();
        run(dir.path(), &["build", "main.mds", "-o", "-"])
    };

    let info = format!("info: {MARKER}");
    for (header, kept) in [
        (format!("data:\n  local: {MARKER}"), ""),
        (
            format!("data:\n# Data comment\n  local: {MARKER}"),
            "# Data comment\n",
        ),
        (format!("data: {{local: {MARKER}}}"), ""),
    ] {
        let out = build(&format!("---\n{header}\n{info}\n---\n{{{{local}}}}\n"));
        assert_eq!(
            code_and_stdout(&out),
            (Some(0), format!("---\n{kept}{info}\n---\nLocal\n")),
            "{header}; stderr: {}",
            stderr_of(&out)
        );
    }

    let control = format!("---\n'info': {{local: {MARKER}}}\n---\nBody\n");
    assert_eq!(
        code_and_stdout(&build(&control)),
        (Some(0), control.clone())
    );
    for header in [
        format!("'data': {{local: {MARKER}}}"),
        format!("{{data: {{local: {MARKER}}}}}"),
    ] {
        let out = build(&format!("---\n{header}\n---\nBody\n"));
        let stderr = stderr_of(&out);
        assert_eq!(code_and_stdout(&out), (Some(1), String::new()), "{stderr}");
        assert!(
            stderr.contains("mds::yaml")
                && squash(&stderr).contains(&squash(
                    "YAML parse error: write `data:` at the start of a line"
                )),
            "{header}: {stderr}"
        );
    }
}

/// An `https://` string entry and a request-map entry with no replay fail `mds build`
/// with `mds::network_denied`, exit 2, naming both frontier entries with the replay help,
/// and nothing connects to the address that they name; `mds check` opens no connection
/// either (#405, AC-15). Test-side control: a connection to the listener is seen. Control:
/// replay renders the runtime values.
#[test]
fn build_denies_network_entries_without_replay_and_opens_no_connection() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    {
        let _probe = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert!(
            accepts_within(&listener, Duration::from_secs(2)),
            "control: the listener sees a connection"
        );
    }
    let source = format!(
        "---\ndata:\n  web: https://127.0.0.1:{port}/web\n  api:\n    url: https://127.0.0.1:{port}/api\n    method: GET\n---\n{{{{web}}}} {{{{api}}}}\n"
    );
    let dir = project(&[("main.mds", &source)]);

    let out = run(dir.path(), &["build", "main.mds", "-o", "-"]);
    let stderr = stderr_of(&out);
    assert_eq!(code_and_stdout(&out), (Some(2), String::new()), "{stderr}");
    assert!(stderr.contains("mds::network_denied"), "{stderr}");
    assert!(
        squash(&stderr).contains(&squash(
            "network access denied for data entries: \"main.mds\", entry \"web\"; \"main.mds\", entry \"api\""
        )),
        "{stderr}"
    );
    assert!(squash(&stderr).contains(&squash(REPLAY_HELP)), "{stderr}");
    let check = run(dir.path(), &["check", "main.mds"]);
    assert_eq!(check.status.code(), Some(0), "{}", stderr_of(&check));
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == ErrorKind::WouldBlock),
        "no command connects to the listener"
    );

    let out = run(
        dir.path(),
        &[
            "build",
            "main.mds",
            "-o",
            "-",
            "--set-string",
            "web=Web",
            "--set-string",
            "api=Api",
        ],
    );
    assert_eq!(
        code_and_stdout(&out),
        (Some(0), "Web Api\n".to_owned()),
        "control: replay renders; stderr: {}",
        stderr_of(&out)
    );
}

/// A runtime variable with an entry's name replaces the entry, through `--vars`, `--set`
/// and `--set-string`, and its data file is not read (#405, AC-16). Control: with no
/// replay, the build reads the file and fails with `mds::yaml`.
#[test]
fn replay_through_vars_file_set_and_set_string_reads_no_file() {
    let source = "---\ndata:\n  title: ./title.json\n---\nTitle: {{title}}\n";
    let dir = project(&[
        ("main.mds", source),
        ("title.json", BAD_DATA),
        ("vars.json", "{\"title\": \"Replayed\"}\n"),
    ]);
    for replay in [
        ["--vars", "vars.json"],
        ["--set", "title=Replayed"],
        ["--set-string", "title=Replayed"],
    ] {
        let out = run(
            dir.path(),
            &[&["build", "main.mds", "-o", "-"][..], &replay[..]].concat(),
        );
        assert_eq!(
            code_and_stdout(&out),
            (Some(0), "Title: Replayed\n".to_owned()),
            "{replay:?}: stderr: {}",
            stderr_of(&out)
        );
    }

    let out = run(dir.path(), &["build", "main.mds", "-o", "-"]);
    let stderr = stderr_of(&out);
    assert_eq!(code_and_stdout(&out), (Some(1), String::new()), "{stderr}");
    assert!(
        stderr.contains("mds::yaml")
            && squash(&stderr).contains(&squash(
                "data file \"title.json\", entry \"title\": invalid JSON data"
            )),
        "control: {stderr}"
    );
}

/// A runtime variable named `data` is an ordinary variable: through `--vars`, `--set` and
/// `--set-string` it defines no entry and reads no file (#405, AC-6). Control: the same
/// entry in frontmatter reads the file and fails once with `mds::yaml`.
#[test]
fn a_runtime_data_variable_defines_no_entry() {
    let dir = project(&[
        ("map.mds", "{{data.cfg}}\n"),
        ("text.mds", "{{data}}\n"),
        ("bad.json", BAD_DATA),
        ("vars.json", "{\"data\": {\"cfg\": \"./bad.json\"}}\n"),
    ]);
    for args in [
        ["map.mds", "--vars", "vars.json"],
        ["text.mds", "--set", "data=./bad.json"],
        ["text.mds", "--set-string", "data=./bad.json"],
    ] {
        let out = run(dir.path(), &[&["build", "-o", "-"][..], &args[..]].concat());
        assert_eq!(
            code_and_stdout(&out),
            (Some(0), "./bad.json\n".to_owned()),
            "{args:?}: stderr: {}",
            stderr_of(&out)
        );
    }

    fs::write(
        dir.path().join("entry.mds"),
        "---\ndata:\n  cfg: ./bad.json\n---\n{{cfg}}\n",
    )
    .unwrap();
    let out = run(dir.path(), &["build", "entry.mds", "-o", "-"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert_eq!(count_occurrences(&stderr, "mds::yaml"), 1, "{stderr}");
}

/// `mds check`, `mds lint`, `mds lint --fix` and `mds fmt` read no data file (#405,
/// AC-19): a data file that does not parse and one that does not exist fail none of them,
/// and none reports `mds::yaml`. `mds check` exits 0 with one warning that names the
/// pending entries; `mds fmt` still formats the file, through the structural gate.
/// Controls: `mds build` reads the files and fails once with `mds::yaml`, and a misspelled
/// name that is not an entry still fails `mds check` with `mds::undefined_var`.
#[test]
fn tooling_reads_no_data_file() {
    const SOURCE: &str = "---\ndata:\n  rows: ./bad.yml\n  other: ./absent.json\n---\n@for row in rows:\n{{row.name}}\n@end\n{{other}}\n";
    let dir = project(&[("main.mds", SOURCE), ("bad.yml", BAD_DATA)]);
    let main = dir.path().join("main.mds");

    let check = run(dir.path(), &["check", "main.mds"]);
    let stderr = stderr_of(&check);
    assert_eq!(check.status.code(), Some(0), "{stderr}");
    let warning = "data pending; output was not evaluated: \"main.mds\", entry \"rows\"; \"main.mds\", entry \"other\"\n";
    assert_eq!(count_occurrences(&stderr, warning), 1, "{stderr}");
    assert_eq!(count_occurrences(&stderr, "data pending"), 1, "{stderr}");

    for args in [
        &["lint", "main.mds"][..],
        &["lint", "--fix", "main.mds"][..],
    ] {
        let out = run(dir.path(), args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("mds::"), "{args:?}: {stderr}");
    }
    assert_eq!(fs::read_to_string(&main).unwrap(), SOURCE);

    fs::write(&main, format!("{SOURCE}\n\n")).unwrap();
    for (args, status) in [
        (&["fmt", "main.mds"][..], "Formatted: main.mds"),
        (&["fmt", "--check", "main.mds"][..], "Unchanged: main.mds"),
    ] {
        let out = run(dir.path(), args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(
            stderr.contains(status) && !stderr.contains("mds::"),
            "{args:?}: {stderr}"
        );
    }
    assert_eq!(fs::read_to_string(&main).unwrap(), SOURCE);

    let build = run(dir.path(), &["build", "main.mds", "-o", "-"]);
    let stderr = stderr_of(&build);
    assert_eq!(
        code_and_stdout(&build),
        (Some(1), String::new()),
        "{stderr}"
    );
    assert_eq!(count_occurrences(&stderr, "mds::yaml"), 1, "{stderr}");
    assert!(
        squash(&stderr).contains(&squash("data file \"bad.yml\", entry \"rows\"")),
        "control: {stderr}"
    );

    fs::write(&main, SOURCE.replace("{{other}}", "{{typo}}")).unwrap();
    let check = run(dir.path(), &["check", "main.mds"]);
    let stderr = stderr_of(&check);
    assert_eq!(check.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("mds::undefined_var"), "control: {stderr}");
}

/// `mds build` never writes its output over a data file that the run reads (#405): the
/// data file `chat.json` at the default output of the messages template `chat.mds`, and
/// the data file `cfg.json` named by `-o` — each refused, `mds::io`, exit 2, and left as it
/// was. Control: another output path is written.
#[test]
fn build_never_writes_over_a_data_file_it_reads() {
    const CHAT: &str =
        "---\ndata:\n  cfg: ./chat.json\n---\n@message user:\nHi {{cfg.name}}\n@end\n";
    const CHAT_DATA: &str = "{\"name\": \"Dean\"}\n";
    let mut files = DATA_FILES.to_vec();
    files.extend([
        ("main.mds", DATA_FIXTURE),
        ("chat.mds", CHAT),
        ("chat.json", CHAT_DATA),
    ]);
    let dir = project(&files);
    let text = |name: &str| fs::read_to_string(dir.path().join(name)).unwrap();

    for (args, shown, name, was) in [
        (
            &["build", "chat.mds"][..],
            Path::new(".").join("chat.json"),
            "chat.json",
            CHAT_DATA,
        ),
        (
            &["build", "main.mds", "-o", "cfg.json"][..],
            Path::new("cfg.json").to_path_buf(),
            "cfg.json",
            DATA_FILES[0].1,
        ),
    ] {
        let out = run(dir.path(), args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(refuses(&stderr, &shown), "{args:?}: {stderr}");
        assert_eq!(text(name), was, "{args:?}: the data file is left as it was");
    }

    let out = run(dir.path(), &["build", "chat.mds", "-o", "out.json"]);
    assert_eq!(out.status.code(), Some(0), "control: {}", stderr_of(&out));
    assert!(text("out.json").contains("Hi Dean"), "{}", text("out.json"));
}

/// A directory build applies the same rule per source: the messages template `chat.mds`
/// reads `./chat.json`, its default output, so that write is refused (`mds::io`, exit 2)
/// and the data file is left as it was. Control: the run goes on and writes the output
/// of the other source, `main.mds`.
#[test]
fn directory_build_never_writes_over_a_data_file_its_source_reads() {
    const CHAT: &str =
        "---\ndata:\n  cfg: ./chat.json\n---\n@message user:\nHi {{cfg.name}}\n@end\n";
    const CHAT_DATA: &str = "{\"name\": \"Dean\"}\n";
    let mut files = DATA_FILES.to_vec();
    files.extend([
        ("main.mds", DATA_FIXTURE),
        ("chat.mds", CHAT),
        ("chat.json", CHAT_DATA),
    ]);
    let dir = project(&files);

    let out = run(dir.path(), &["build", "."]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("mds::io") && squash(&stderr).contains(&squash(INPUT_CAUSE)),
        "{stderr}"
    );
    assert!(squash(&stderr).contains("chat.json"), "{stderr}");
    assert_eq!(
        fs::read_to_string(dir.path().join("chat.json")).unwrap(),
        CHAT_DATA
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("main.md")).unwrap(),
        RENDERED,
        "control: {stderr}"
    );
}
