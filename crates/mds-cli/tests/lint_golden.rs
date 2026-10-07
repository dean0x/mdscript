//! Characterization goldens for `mds lint` (#309): stdin, single-file and directory
//! output.
//!
//! Every cell of the matrix — input (stdin with cwd = the fixture dir, a relative
//! file, or the relative directory `d`) × format (human, json) × `--quiet` (off, on) ×
//! fix mode (none, `--fix`, `--fix --check`, `--fix --diff`, `--fix --check --diff`) ×
//! outcome fixture (for directories also: an empty tree, an all-excluded tree and a
//! mixed tree) — runs `mds lint` and must reproduce its golden byte for byte: exit
//! code, stdout, stderr and the source file's bytes afterwards (for a directory cell,
//! the state of every file in the fixture). A directory cell runs twice and both runs
//! must agree. The goldens pin today's output, including output that is known to be
//! wrong; a commit that changes lint output regenerates them (see `golden_print_mode`)
//! and names the changed cell ids.
//!
//! Machinery, fixtures and normalization rules: `lint_golden/harness.rs`.
//! Generated data: `lint_golden/single.rs` (stdin and file cells) and
//! `lint_golden/dir.rs` (directory cells).

#[path = "lint_golden/dir.rs"]
mod dir;
#[path = "lint_golden/harness.rs"]
mod harness;
#[path = "lint_golden/single.rs"]
mod single;

use harness::{
    Cell, Digest, DirFixture, DirGolden, DirObserved, FileAfter, FixMode, Fixture, Format, Golden,
    Input, Locking, Normalizer, Observed, ObservedFile, Order, Quiet, Stream,
};

// ── The cells, one test per (input, fixture) group ───────────────────────────

macro_rules! group_tests {
    ($($(#[$meta:meta])* $name:ident => $input:ident, $fixture:ident;)*) => {
        $(
            $(#[$meta])*
            #[test]
            fn $name() {
                harness::check_group(
                    Input::$input,
                    Fixture::$fixture,
                    single::GOLDENS,
                    single::CELLS,
                );
            }
        )*
    };
}

group_tests! {
    stdin_clean => Stdin, Clean;
    stdin_warn => Stdin, Warn;
    stdin_error => Stdin, Error;
    stdin_fixed => Stdin, Fixed;
    stdin_partial => Stdin, Partial;
    stdin_rejected => Stdin, Rejected;
    stdin_cap => Stdin, Cap;
    stdin_cap_fixable => Stdin, CapFixable;
    stdin_analysis_fail => Stdin, AnalysisFail;
    stdin_limit => Stdin, Limit;
    stdin_config_fail => Stdin, ConfigFail;
    #[cfg(unix)]
    stdin_write_fail => Stdin, WriteFail;
    stdin_partial_module => Stdin, PartialModule;
    file_clean => File, Clean;
    file_warn => File, Warn;
    file_error => File, Error;
    file_fixed => File, Fixed;
    file_partial => File, Partial;
    file_rejected => File, Rejected;
    file_cap => File, Cap;
    file_cap_fixable => File, CapFixable;
    file_analysis_fail => File, AnalysisFail;
    file_limit => File, Limit;
    file_config_fail => File, ConfigFail;
    #[cfg(unix)]
    file_write_fail => File, WriteFail;
    file_partial_module => File, PartialModule;
}

/// One test per directory fixture: its 20 cells, each run twice.
macro_rules! dir_group_tests {
    ($($(#[$meta:meta])* $name:ident => $fixture:expr;)*) => {
        $(
            $(#[$meta])*
            #[test]
            fn $name() {
                harness::check_dir_group($fixture, dir::GOLDENS, dir::CELLS);
            }
        )*
    };
}

dir_group_tests! {
    dir_clean => DirFixture::Outcome(Fixture::Clean);
    dir_warn => DirFixture::Outcome(Fixture::Warn);
    dir_error => DirFixture::Outcome(Fixture::Error);
    dir_fixed => DirFixture::Outcome(Fixture::Fixed);
    dir_partial => DirFixture::Outcome(Fixture::Partial);
    dir_rejected => DirFixture::Outcome(Fixture::Rejected);
    dir_cap => DirFixture::Outcome(Fixture::Cap);
    dir_cap_fixable => DirFixture::Outcome(Fixture::CapFixable);
    dir_analysis_fail => DirFixture::Outcome(Fixture::AnalysisFail);
    dir_limit => DirFixture::Outcome(Fixture::Limit);
    dir_config_fail => DirFixture::Outcome(Fixture::ConfigFail);
    #[cfg(unix)]
    dir_write_fail => DirFixture::Outcome(Fixture::WriteFail);
    dir_partial_module => DirFixture::Outcome(Fixture::PartialModule);
    dir_empty => DirFixture::Empty;
    dir_all_excluded => DirFixture::AllExcluded;
    dir_mixed => DirFixture::Mixed;
}

// ── Generator ────────────────────────────────────────────────────────────────

/// With `MDS_GOLDEN_PRINT=1`, runs every cell and prints the data modules for
/// `lint_golden/single.rs` and `lint_golden/dir.rs` on stderr, each between its
/// BEGIN/END markers; never writes a file. Without it, does nothing.
#[test]
fn golden_print_mode() {
    if std::env::var(harness::PRINT_ENV).as_deref() != Ok("1") {
        return;
    }
    if !cfg!(unix) {
        panic!("generate the goldens on unix: the write-failure cells exist only there");
    }
    let (module, summary) =
        harness::render_data_module("stdin and single-file cells", &harness::all_cells());
    harness::print_module("lint_golden/single.rs", &module, &summary);
    let (module, summary) =
        harness::render_dir_module("directory cells", &harness::all_dir_cells());
    harness::print_module("lint_golden/dir.rs", &module, &summary);
}

// ── Coverage self-checks ─────────────────────────────────────────────────────

#[test]
fn matrix_has_the_literal_cell_counts() {
    assert_eq!(
        harness::all_cells().len(),
        520,
        "13 fixtures x 20 variants x 2 inputs"
    );
    assert_eq!(
        harness::all_dir_cells().len(),
        320,
        "16 directory fixtures x 20 variants"
    );
    let all = harness::all_cell_ids();
    assert_eq!(all.len(), 840, "the whole matrix, on every platform");

    let active = harness::active_cells();
    let stdin = active.iter().filter(|c| c.input == Input::Stdin).count();
    let file = active.iter().filter(|c| c.input == Input::File).count();
    let dir = harness::active_dir_cells().len();
    let ids = harness::active_cell_ids();
    let prefixed = |p: &str| ids.iter().filter(|id| id.starts_with(p)).count();
    assert_eq!(
        (prefixed("stdin/"), prefixed("file/"), prefixed("dir/")),
        (stdin, file, dir)
    );
    if cfg!(unix) {
        assert_eq!((ids.len(), stdin, file, dir), (840, 260, 260, 320));
    } else {
        assert_eq!((ids.len(), stdin, file, dir), (780, 240, 240, 300));
    }
}

/// Every golden index a table uses exists, and every golden is used.
fn assert_no_dead_golden(table_name: &str, goldens: usize, cells: &[(&str, u16)]) {
    let mut used = vec![false; goldens];
    for (id, index) in cells {
        let slot = used
            .get_mut(usize::from(*index))
            .unwrap_or_else(|| panic!("{table_name}: cell {id} maps to missing golden {index}"));
        *slot = true;
    }
    let dead: Vec<usize> = (0..used.len()).filter(|&i| !used[i]).collect();
    assert!(
        dead.is_empty(),
        "{table_name}: goldens no cell maps to: {dead:?}"
    );
}

/// No two goldens of one table are identical (the generator deduplicates).
fn assert_deduplicated<G: PartialEq + std::fmt::Debug>(table_name: &str, goldens: &[G]) {
    for (i, a) in goldens.iter().enumerate() {
        for (j, b) in goldens.iter().enumerate().skip(i + 1) {
            assert_ne!(
                a, b,
                "{table_name}: goldens {i} and {j} are identical (not deduplicated)"
            );
        }
    }
}

#[test]
fn every_cell_has_exactly_one_golden_and_no_golden_is_dead() {
    // Each table holds only its own input's cells ...
    for (id, _) in single::CELLS {
        assert!(
            id.starts_with("stdin/") || id.starts_with("file/"),
            "single.rs holds {id}"
        );
    }
    for (id, _) in dir::CELLS {
        assert!(id.starts_with("dir/"), "dir.rs holds {id}");
    }
    // ... and together they list every cell of the whole matrix exactly once.
    let mut table: Vec<&str> = single::CELLS
        .iter()
        .chain(dir::CELLS)
        .map(|(id, _)| *id)
        .collect();
    table.sort_unstable();
    let before = table.len();
    table.dedup();
    assert_eq!(before, table.len(), "a cell id appears twice in the tables");
    let all = harness::all_cell_ids();
    let mut expected: Vec<&str> = all.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(
        table, expected,
        "the cell tables must list exactly the matrix cells"
    );

    assert_no_dead_golden("single.rs", single::GOLDENS.len(), single::CELLS);
    assert_no_dead_golden("dir.rs", dir::GOLDENS.len(), dir::CELLS);
    assert_deduplicated("single.rs", single::GOLDENS);
    assert_deduplicated("dir.rs", dir::GOLDENS);
}

fn check_stream_bounds(golden: &str, s: &Stream) {
    match s {
        Stream::Text(t) => assert!(
            t.len() <= harness::DIGEST_THRESHOLD,
            "{golden}: a {}-byte inline stream must be a digest",
            t.len()
        ),
        Stream::Digest(d) => {
            assert!(
                d.len > harness::DIGEST_THRESHOLD,
                "{golden}: a {}-byte stream must be inline",
                d.len
            );
            assert!(d.head.len() <= harness::DIGEST_HEAD_MAX_BYTES, "{golden}");
            assert!(d.tail.len() <= harness::DIGEST_TAIL_MAX_BYTES, "{golden}");
            assert!(
                d.head.split_inclusive('\n').count() <= harness::DIGEST_HEAD_LINES,
                "{golden}"
            );
            assert!(
                d.tail.split_inclusive('\n').count() <= harness::DIGEST_TAIL_LINES,
                "{golden}"
            );
        }
    }
}

/// Where a golden may hold a backslash. On Windows every backslash in the output is
/// rewritten to `/`, so a golden that a cell running there maps to must hold none. A
/// golden only unix-only cells map to never meets that rule and may hold the one
/// backslash JSON puts in front of a quote inside a string (`\"`), nothing else.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backslashes {
    None,
    EscapedQuotesOnly,
}

/// Every backslash in `text` is immediately followed by a double quote.
fn backslashes_escape_quotes_only(text: &str) -> bool {
    text.match_indices('\\')
        .all(|(at, _)| text[at + 1..].starts_with('"'))
}

fn check_stored_strings(golden: &str, strings: Vec<&'static str>, backslashes: Backslashes) {
    for text in strings {
        let bad: Vec<char> = text
            .chars()
            .filter(|c| c.is_control() && *c != '\n')
            .collect();
        assert!(bad.is_empty(), "{golden} holds control characters {bad:?}");
        match backslashes {
            Backslashes::None => assert!(!text.contains('\\'), "{golden} holds a backslash"),
            Backslashes::EscapedQuotesOnly => assert!(
                backslashes_escape_quotes_only(text),
                "{golden} holds a backslash that does not escape a quote"
            ),
        }
    }
}

/// For each golden of a table: whether a cell that also runs on Windows maps to it.
fn reached_from_windows(goldens: usize, cells: &[(&str, u16)], unix_only: &[String]) -> Vec<bool> {
    let mut reached = vec![false; goldens];
    for (id, index) in cells {
        if !unix_only.iter().any(|u| u == id) {
            reached[usize::from(*index)] = true;
        }
    }
    reached
}

#[test]
fn golden_storage_stays_within_bounds() {
    // Positive controls for the backslash rule.
    assert!(backslashes_escape_quotes_only("at path \\\"$TMP\\\""));
    assert!(!backslashes_escape_quotes_only("C:\\Users"));
    assert!(!backslashes_escape_quotes_only("trailing \\"));

    let unix_only: Vec<String> = harness::all_cells()
        .iter()
        .filter(|c| c.fixture.unix_only())
        .map(Cell::id)
        .chain(
            harness::all_dir_cells()
                .iter()
                .filter(|c| c.fixture.unix_only())
                .map(harness::DirCell::id),
        )
        .collect();
    let policy = |reached: bool| {
        if reached {
            Backslashes::None
        } else {
            Backslashes::EscapedQuotesOnly
        }
    };

    let reached = reached_from_windows(single::GOLDENS.len(), single::CELLS, &unix_only);
    for (i, g) in single::GOLDENS.iter().enumerate() {
        let name = format!("single.rs golden {i}");
        check_stream_bounds(&name, &g.stdout);
        check_stream_bounds(&name, &g.stderr);
        if let FileAfter::Changed(s) = &g.file {
            check_stream_bounds(&name, s);
        }
        check_stored_strings(&name, harness::golden_strings(g), policy(reached[i]));
    }
    let reached = reached_from_windows(dir::GOLDENS.len(), dir::CELLS, &unix_only);
    for (i, g) in dir::GOLDENS.iter().enumerate() {
        let name = format!("dir.rs golden {i}");
        check_stream_bounds(&name, &g.stdout);
        check_stream_bounds(&name, &g.stderr);
        let names: Vec<&str> = g.files.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "{name}: file records sorted and distinct");
        for (_, file) in g.files {
            if let FileAfter::Changed(s) = file {
                check_stream_bounds(&name, s);
            }
        }
        check_stored_strings(&name, harness::dir_golden_strings(g), policy(reached[i]));
    }
    let bytes =
        harness::golden_data_bytes(single::GOLDENS) + harness::dir_golden_data_bytes(dir::GOLDENS);
    assert!(
        bytes <= harness::GOLDEN_DATA_CAP,
        "golden data (both tables) is {bytes} bytes, over the {} cap",
        harness::GOLDEN_DATA_CAP
    );
}

/// What a golden's counts and stored strings record of a leaked path: a temporary
/// directory replaced by `$TMP`, an atomic-write temp name replaced by
/// `.mds-tmp-RANDOM.tmp`, or either replacement in a stored string. Empty when nothing.
fn leaked_paths(tmp_paths: u32, tmp_names: u32, strings: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    if tmp_paths != 0 {
        found.push(format!(
            "{tmp_paths} temporary-directory path(s) replaced by {}",
            harness::TMP_TOKEN
        ));
    }
    if tmp_names != 0 {
        found.push(format!(
            "{tmp_names} atomic-write temp name(s) replaced by {}",
            harness::TMP_NAME_REPLACEMENT
        ));
    }
    for text in strings {
        for token in [harness::TMP_TOKEN, harness::TMP_NAME_REPLACEMENT] {
            if text.contains(token) {
                found.push(format!("a stored string holds {token}: {text:?}"));
            }
        }
    }
    found
}

/// No golden records a leaked path. A cell's `$TMP` and temp-name counts are part of its
/// golden, so a leak that came back and was regenerated into the goldens would otherwise
/// pass as the new truth; with this test it needs an edit here as well.
///
/// Controls: a run's stream naming a real temporary directory, and one naming an
/// atomic-write temp file, normalized as every cell's streams are, are each reported;
/// the same stream naming neither is not.
#[test]
fn no_golden_records_a_leaked_path() {
    let dir = harness::fixture_dir();
    let normalizer = Normalizer::for_dir(dir.path());
    let leaked = format!(
        "error writing {}: denied\n",
        dir.path().join("x.mds").display()
    );
    let (text, paths, names) = normalizer.apply(&leaked);
    assert_eq!(
        (paths, names),
        (1, 0),
        "control: the tempdir is counted in {text:?}"
    );
    let found = leaked_paths(paths, names, &[&text]);
    assert_eq!(
        found.len(),
        2,
        "control: the count and the token: {found:?}"
    );
    let (text, paths, names) = normalizer.apply("error writing d/.mds-tmp-Ab3dE9.tmp: denied\n");
    let found = leaked_paths(paths, names, &[&text]);
    assert_eq!(found.len(), 2, "control: the count and the name: {found:?}");
    let (text, paths, names) = normalizer.apply("error writing d/x.mds: denied\n");
    assert_eq!(leaked_paths(paths, names, &[&text]), Vec::<String>::new());

    let mut leaks = Vec::new();
    for (i, g) in single::GOLDENS.iter().enumerate() {
        let found = leaked_paths(g.tmp_paths, g.tmp_names, &harness::golden_strings(g));
        leaks.extend(
            found
                .into_iter()
                .map(|f| format!("single.rs golden {i}: {f}")),
        );
    }
    for (i, g) in dir::GOLDENS.iter().enumerate() {
        let found = leaked_paths(g.tmp_paths, g.tmp_names, &harness::dir_golden_strings(g));
        leaks.extend(found.into_iter().map(|f| format!("dir.rs golden {i}: {f}")));
    }
    assert!(
        leaks.is_empty(),
        "a golden records a leaked path; fix the leak, never regenerate it into the data:\n{}",
        leaks.join("\n")
    );
}

// ── Comparator and normalizer positive controls ──────────────────────────────

/// XOR the lowest bit of the ASCII byte at `at` (the result stays ASCII, so UTF-8).
fn flip_one_byte(s: &str, at: usize) -> String {
    let mut bytes = s.as_bytes().to_vec();
    assert!(bytes[at].is_ascii(), "flip an ASCII byte");
    bytes[at] ^= 1;
    String::from_utf8(bytes).expect("still UTF-8")
}

fn inline_text(s: &Stream) -> String {
    match s {
        Stream::Text(t) => (*t).to_string(),
        Stream::Digest(_) => panic!("pick a golden with inline streams"),
    }
}

fn observed_file_from(f: &FileAfter) -> ObservedFile {
    match f {
        FileAfter::Unchanged => ObservedFile::Unchanged,
        FileAfter::Missing => ObservedFile::Missing,
        FileAfter::Changed(s) => ObservedFile::Changed(inline_text(s)),
    }
}

fn observed_from(g: &Golden) -> Observed {
    Observed {
        exit: g.exit,
        stdout: inline_text(&g.stdout),
        stderr: inline_text(&g.stderr),
        file: observed_file_from(&g.file),
        tmp_paths: g.tmp_paths,
        tmp_names: g.tmp_names,
    }
}

fn dir_observed_from(g: &DirGolden) -> DirObserved {
    DirObserved {
        exit: g.exit,
        stdout: inline_text(&g.stdout),
        stderr: inline_text(&g.stderr),
        files: g
            .files
            .iter()
            .map(|(name, file)| ((*name).to_string(), observed_file_from(file)))
            .collect(),
        tmp_paths: g.tmp_paths,
        tmp_names: g.tmp_names,
    }
}

/// A copy of `obs` with one change applied.
fn changed(obs: &DirObserved, change: impl FnOnce(&mut DirObserved)) -> DirObserved {
    let mut copy = obs.clone();
    change(&mut copy);
    copy
}

#[test]
fn comparator_detects_a_one_byte_change_in_every_part() {
    let golden = Golden {
        exit: 1,
        stdout: Stream::Text("--- a\n+++ b\n@@ -1 +1 @@\n-x\n+y\n"),
        stderr: Stream::Text("line one\nline two\nline three\nline four\nline five\n"),
        file: FileAfter::Changed(Stream::Text("Hello\nworld\n")),
        tmp_paths: 2,
        tmp_names: 1,
    };
    let exact = observed_from(&golden);
    assert_eq!(harness::compare("synthetic", &golden, &exact), Ok(()));

    let mut exit = exact.clone();
    exit.exit ^= 1;
    let mut stdout = exact.clone();
    stdout.stdout = flip_one_byte(&exact.stdout, 7);
    let mut stderr = exact.clone();
    stderr.stderr = flip_one_byte(&exact.stderr, 30);
    let mut file = exact.clone();
    file.file = ObservedFile::Changed(flip_one_byte("Hello\nworld\n", 8));
    let mut unchanged = exact.clone();
    unchanged.file = ObservedFile::Unchanged;
    let mut paths = exact.clone();
    paths.tmp_paths += 1;
    let mut names = exact.clone();
    names.tmp_names += 1;
    for (part, obs) in [
        ("exit", exit),
        ("stdout", stdout),
        ("stderr", stderr),
        ("file", file),
        ("file", unchanged),
        ("$TMP", paths),
        ("temp-name", names),
    ] {
        let err = harness::compare("synthetic", &golden, &obs)
            .expect_err("a one-byte change must be detected");
        assert!(err.starts_with("cell synthetic:"), "names the cell: {err}");
        assert!(err.contains(part), "names the differing part {part}: {err}");
    }

    // The failure message is bounded: lengths plus the first differing line and at
    // most three lines of context on each side, never the whole stream.
    let long: String = (0..200).map(|i| format!("line {i:03}\n")).collect();
    let flipped = flip_one_byte(&long, long.find("line 100").expect("line 100") + 1);
    let message = harness::first_difference(&long, &flipped);
    // Line 101 holds "line 100"; lines 98..=104 hold "line 097".."line 103".
    assert!(
        message.contains("first difference at line 101"),
        "{message}"
    );
    assert!(
        message.contains("line 097") && message.contains("line 103"),
        "{message}"
    );
    assert!(
        !message.contains("line 096") && !message.contains("line 104"),
        "{message}"
    );

    // Digest: a byte flipped outside the stored head/tail is caught by the hash; one
    // inside the head is caught and located.
    let big: &'static str = Box::leak(long.repeat(20).into_boxed_str());
    assert!(big.len() > harness::DIGEST_THRESHOLD);
    let d = harness::digest_of(big);
    let digest = Stream::Digest(Digest {
        len: d.len,
        lines: d.lines,
        fnv: d.fnv,
        head: Box::leak(d.head.into_boxed_str()),
        tail: Box::leak(d.tail.into_boxed_str()),
    });
    assert_eq!(harness::compare_stream("stderr", &digest, big), Ok(()));
    let middle = flip_one_byte(big, big.len() / 2);
    let err = harness::compare_stream("stderr", &digest, &middle).expect_err("hash catches it");
    assert!(err.contains("outside the stored head and tail"), "{err}");
    let early = flip_one_byte(big, 3);
    let err = harness::compare_stream("stderr", &digest, &early).expect_err("head catches it");
    assert!(err.contains("in the stored head"), "{err}");

    // And on a real golden: an exact replay matches, a one-byte change does not.
    let (index, real) = single::GOLDENS
        .iter()
        .enumerate()
        .find(|(_, g)| {
            matches!(g.stdout, Stream::Text(_))
                && matches!(g.stderr, Stream::Text(t) if t.len() > 1)
                && !matches!(g.file, FileAfter::Changed(Stream::Digest(_)))
        })
        .expect("a golden with inline streams and a non-empty stderr");
    let replay = observed_from(real);
    let id = format!("golden {index}");
    assert_eq!(harness::compare(&id, real, &replay), Ok(()));
    let mut changed = replay.clone();
    let at = changed
        .stderr
        .bytes()
        .position(|b| b.is_ascii_alphanumeric())
        .expect("an ASCII byte in stderr");
    changed.stderr = flip_one_byte(&replay.stderr, at);
    assert!(harness::compare(&id, real, &changed).is_err());
}

#[test]
fn dir_comparator_and_double_run_detect_a_one_byte_change_in_every_part() {
    let golden = DirGolden {
        exit: 2,
        stdout: Stream::Text("{\"files\":[],\"truncated\":false,\"version\":1}\n"),
        stderr: Stream::Text(
            "error writing d/x.mds: denied\n0 clean, 0 with warnings, 1 with errors\n",
        ),
        files: &[
            ("d/a.mds", FileAfter::Unchanged),
            (
                "d/x.mds",
                FileAfter::Changed(Stream::Text("Hello\nworld\n")),
            ),
            ("d/y.mds", FileAfter::Missing),
        ],
        tmp_paths: 1,
        tmp_names: 1,
    };
    let exact = dir_observed_from(&golden);
    assert_eq!(harness::compare_dir("synthetic", &golden, &exact), Ok(()));
    assert_eq!(
        harness::compare_runs("synthetic", &exact, &exact.clone()),
        Ok(())
    );

    // (part the golden comparator names, part the double-run check names, change)
    let cases = [
        ("exit", "exit", changed(&exact, |o| o.exit ^= 1)),
        (
            "stdout",
            "stdout",
            changed(&exact, |o| o.stdout = flip_one_byte(&o.stdout, 3)),
        ),
        (
            "stderr",
            "stderr",
            changed(&exact, |o| o.stderr = flip_one_byte(&o.stderr, 20)),
        ),
        (
            "file d/x.mds",
            "files: first run d/x.mds",
            changed(&exact, |o| {
                o.files[1].1 = ObservedFile::Changed(flip_one_byte("Hello\nworld\n", 8));
            }),
        ),
        (
            "file d/a.mds",
            "files: first run d/a.mds",
            changed(&exact, |o| {
                o.files[0].1 = ObservedFile::Changed("x".to_string())
            }),
        ),
        (
            "file d/y.mds",
            "files: first run d/y.mds",
            changed(&exact, |o| o.files[2].1 = ObservedFile::Unchanged),
        ),
        (
            "unexpected [d/z.mds]",
            "files: first run 3 files, second run 4",
            changed(&exact, |o| {
                o.files
                    .push(("d/z.mds".to_string(), ObservedFile::Changed(String::new())));
            }),
        ),
        (
            "missing [d/y.mds]",
            "files: first run 3 files, second run 2",
            changed(&exact, |o| {
                o.files.pop();
            }),
        ),
        ("$TMP", "$TMP", changed(&exact, |o| o.tmp_paths += 1)),
        (
            "temp-name",
            "temp-name",
            changed(&exact, |o| o.tmp_names += 1),
        ),
    ];
    for (part, run_part, obs) in cases {
        let err = harness::compare_dir("synthetic", &golden, &obs)
            .expect_err("a one-byte change must be detected");
        assert!(err.starts_with("cell synthetic:"), "names the cell: {err}");
        assert!(err.contains(part), "names the differing part {part}: {err}");
        let err = harness::compare_runs("synthetic", &exact, &obs)
            .expect_err("the double-run check must detect a one-byte change");
        assert!(
            err.starts_with("cell synthetic: the two runs differ:"),
            "names the cell: {err}"
        );
        assert!(
            err.contains(run_part),
            "names the differing part {run_part}: {err}"
        );
    }

    // And on a real directory golden: an exact replay matches, a one-byte change in
    // stderr or a changed file record does not.
    let (index, real) = dir::GOLDENS
        .iter()
        .enumerate()
        .find(|(_, g)| {
            matches!(g.stdout, Stream::Text(_))
                && matches!(g.stderr, Stream::Text(t) if t.len() > 1)
                && !g.files.is_empty()
                && g.files
                    .iter()
                    .all(|(_, f)| !matches!(f, FileAfter::Changed(Stream::Digest(_))))
        })
        .expect("a directory golden with inline streams, a stderr and file records");
    let replay = dir_observed_from(real);
    let id = format!("dir golden {index}");
    assert_eq!(harness::compare_dir(&id, real, &replay), Ok(()));
    let at = replay
        .stderr
        .bytes()
        .position(|b| b.is_ascii_alphanumeric())
        .expect("an ASCII byte in stderr");
    let flipped = changed(&replay, |o| o.stderr = flip_one_byte(&replay.stderr, at));
    assert!(harness::compare_dir(&id, real, &flipped).is_err());
    let rewritten = changed(&replay, |o| {
        o.files[0].1 = match &o.files[0].1 {
            ObservedFile::Changed(t) => ObservedFile::Changed(format!("{t}x")),
            ObservedFile::Unchanged | ObservedFile::Missing => ObservedFile::Changed(String::new()),
        };
    });
    assert!(harness::compare_dir(&id, real, &rewritten).is_err());
}

#[test]
fn normalizer_rewrites_every_tempdir_spelling() {
    // Unix: typed and canonical (macOS `/private`) spellings.
    let n = Normalizer::from_spellings(["/var/folders/ab/T/.tmpXYZ12"]);
    let (text, paths, names) = n.apply(
        "a /var/folders/ab/T/.tmpXYZ12/x.mds b /private/var/folders/ab/T/.tmpXYZ12 c /tmp/other",
    );
    assert_eq!(text, "a $TMP/x.mds b $TMP c /tmp/other");
    assert_eq!((paths, names), (2, 0));

    // Windows: verbatim, plain, escaped (doubled separators) and forward-slash forms.
    let verbatim = r"\\?\C:\Users\me\AppData\Local\Temp\.tmpAB12";
    let plain = r"C:\Users\me\AppData\Local\Temp\.tmpAB12";
    let escaped = plain.replace('\\', r"\\");
    let forward = plain.replace('\\', "/");
    let n = Normalizer::from_spellings([verbatim]);
    let (text, paths, _) = n.apply(&format!("[{verbatim}] [{plain}] [{escaped}] [{forward}]"));
    assert_eq!(text, "[$TMP] [$TMP] [$TMP] [$TMP]");
    assert_eq!(paths, 4);

    // Negative control: an unrelated directory is left alone and not counted.
    let (text, paths, _) = n.apply("/var/folders/zz/T/.tmpOTHER");
    assert_eq!((text.as_str(), paths), ("/var/folders/zz/T/.tmpOTHER", 0));

    // Atomic-write temp names: exactly six ASCII alphanumerics between the affixes.
    let (text, count) = harness::replace_tmp_names(
        "x .mds-tmp-Ab3dE9.tmp y .mds-tmp-Ab3dE.tmp z .mds-tmp-Ab3dE9x.tmp w .mds-tmp-Ab_dE9.tmp",
    );
    assert_eq!(
        text,
        "x .mds-tmp-RANDOM.tmp y .mds-tmp-Ab3dE.tmp z .mds-tmp-Ab3dE9x.tmp w .mds-tmp-Ab_dE9.tmp"
    );
    assert_eq!(count, 1);
}

#[test]
fn ancestor_config_finds_an_mds_json_above_a_directory_only() {
    // `fixture_dir` itself refuses a directory with an `mds.json` above it.
    let outer = harness::fixture_dir();
    let inner = outer.path().join("inner");
    std::fs::create_dir(&inner).expect("create inner dir");
    // A directory's own mds.json belongs to its fixture and is not reported ...
    std::fs::write(inner.join("mds.json"), "{}").expect("write inner mds.json");
    assert_eq!(harness::ancestor_config(&inner), None);
    // ... one in the directory above is (positive control).
    std::fs::write(outer.path().join("mds.json"), "{}").expect("write outer mds.json");
    let canonical_outer = std::fs::canonicalize(outer.path()).expect("canonicalize");
    assert_eq!(
        harness::ancestor_config(&inner),
        Some(canonical_outer.join("mds.json"))
    );
}

// ── Fixture markers (checked against the tool, not the goldens) ──────────────

fn report_json(fixture: Fixture) -> (i32, serde_json::Value) {
    let cell = Cell {
        input: Input::File,
        format: Format::Json,
        quiet: Quiet::Loud,
        fix: FixMode::Report,
        fixture,
    };
    let obs = harness::run_cell(&cell).unwrap_or_else(|_| panic!("{} skipped", cell.id()));
    let json = serde_json::from_str(&obs.stdout)
        .unwrap_or_else(|e| panic!("{}: stdout is not JSON ({e})", cell.id()));
    (obs.exit, json)
}

/// The diagnostics of a single-file JSON report (a clean file has no entry).
fn diagnostics(json: &serde_json::Value) -> Vec<serde_json::Value> {
    let files = json["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files array in {json}"));
    match files.as_slice() {
        [] => Vec::new(),
        [file] => file["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("no diagnostics array in {json}")),
        _ => panic!("a single-file report has one entry at most: {json}"),
    }
}

fn rules_of(diags: &[serde_json::Value]) -> Vec<&str> {
    diags.iter().filter_map(|d| d["rule"].as_str()).collect()
}

// The cap fixtures produce more findings than the diagnostic cap.
const _: () = assert!(harness::CAP_FINDINGS > mds::MAX_DIAGNOSTICS);

#[test]
fn fixture_markers_hold() {
    for fixture in Fixture::ALL {
        let source = fixture.source();
        assert!(!source.contains('\\'), "{}: backslash", fixture.name());
        assert!(
            source.matches("@define").count() <= 1,
            "{}: at most one @define block per fixture",
            fixture.name()
        );
        if let Some(config) = fixture.config() {
            assert!(
                !config.contains('\\'),
                "{}: backslash in mds.json",
                fixture.name()
            );
        }
    }

    let (exit, json) = report_json(Fixture::Clean);
    assert_eq!((exit, diagnostics(&json).len()), (0, 0), "clean");

    let (exit, json) = report_json(Fixture::Warn);
    let diags = diagnostics(&json);
    assert_eq!(
        (exit, rules_of(&diags)),
        (1, vec!["unused-variable"]),
        "warn"
    );
    assert_eq!(diags[0]["severity"], "warn");

    let (exit, json) = report_json(Fixture::Error);
    let diags = diagnostics(&json);
    assert_eq!(
        (exit, rules_of(&diags)),
        (2, vec!["unused-variable"]),
        "error"
    );
    assert_eq!(diags[0]["severity"], "error", "mds.json raises the warning");

    let (exit, json) = report_json(Fixture::Fixed);
    let diags = diagnostics(&json);
    assert_eq!((exit, rules_of(&diags)), (2, vec!["unreachable-branch"]));
    assert_eq!(diags[0]["fixable"], true, "fixed: the finding is fixable");

    // partial: the unfixable warning sits AFTER the removed dead block.
    let (_, json) = report_json(Fixture::Partial);
    let diags = diagnostics(&json);
    let dead_end = harness::PARTIAL_SOURCE.find("@end\n").expect("dead block") + "@end\n".len();
    assert!(diags
        .iter()
        .any(|d| d["rule"] == "unreachable-branch" && d["fixable"] == true));
    assert!(
        diags.iter().any(|d| d["fixable"] == false
            && d["severity"] == "warn"
            && d["span"]["offset"]
                .as_u64()
                .is_some_and(|o| o as usize >= dead_end)),
        "partial: an unfixable warning after byte {dead_end}; got {json}"
    );

    let (_, json) = report_json(Fixture::Rejected);
    let diags = diagnostics(&json);
    assert_eq!(rules_of(&diags), vec!["empty-block"], "rejected");
    assert_eq!(diags[0]["fixable"], true);
    assert!(harness::REJECTED_SOURCE.contains("@export empty_fn"));

    for (fixture, rule, fixable) in [
        (Fixture::Cap, "unused-variable", false),
        (Fixture::CapFixable, "empty-block", true),
    ] {
        let (_, json) = report_json(fixture);
        let diags = diagnostics(&json);
        assert_eq!(json["truncated"], true, "{}: over the cap", fixture.name());
        assert_eq!(diags.len(), mds::MAX_DIAGNOSTICS, "{}", fixture.name());
        assert!(
            diags
                .iter()
                .all(|d| d["rule"] == rule && d["fixable"] == fixable),
            "{}: every finding is {rule}",
            fixture.name()
        );
    }
    assert!(!harness::cap_fixable_source().contains("@define"));

    let (exit, json) = report_json(Fixture::AnalysisFail);
    assert_eq!(exit, 2);
    assert!(json["error"]["code"].is_string(), "analysis-fail: {json}");

    assert_eq!(harness::LIMIT_BYTES as u64, mds::MAX_FILE_SIZE + 1);
    assert_eq!(harness::limit_source().len(), harness::LIMIT_BYTES);
    let (exit, _) = report_json(Fixture::Limit);
    assert_eq!(exit, 3, "limit: over the input limit");

    let (exit, json) = report_json(Fixture::ConfigFail);
    assert_eq!(exit, 2);
    assert!(json["error"]["code"].is_string(), "config-fail: {json}");

    // partial-module: the frontmatter key a partial may leave unused is reported when
    // the same source is not a partial (positive control).
    let (_, json) = report_json(Fixture::PartialModule);
    assert!(Fixture::PartialModule.file_name().starts_with('_'));
    assert!(!rules_of(&diagnostics(&json)).contains(&"unused-variable"));
    let (_, stdout) = harness::lint_json_report("x.mds", harness::PARTIAL_MODULE_SOURCE);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("JSON report");
    assert!(rules_of(&diagnostics(&json)).contains(&"unused-variable"));

    // write-fail: `--fix` rewrites the source in a writable dir (positive control) and
    // cannot in the read-only one.
    let fix = |fixture| Cell {
        input: Input::File,
        format: Format::Human,
        quiet: Quiet::Loud,
        fix: FixMode::Fix,
        fixture,
    };
    let fixed = harness::run_cell(&fix(Fixture::Fixed)).expect("writable");
    assert!(matches!(fixed.file, ObservedFile::Changed(_)));
    assert_eq!(Fixture::WriteFail.source(), Fixture::Fixed.source());
    #[cfg(unix)]
    match harness::run_cell(&fix(Fixture::WriteFail)) {
        Ok(obs) => {
            assert_eq!(obs.file, ObservedFile::Unchanged, "write-fail");
            assert_ne!(obs.exit, 0, "write-fail");
        }
        Err(skip) => harness::announce_skip("the write-fail marker", skip.reason()),
    }
}

// ── Directory fixture markers (checked against the tool, not the goldens) ────

/// Run `mds <args>` once on `fixture`'s layout; `None` when a lock does not hold.
fn dir_run(fixture: DirFixture, args: &[&str], locking: Locking) -> Option<DirObserved> {
    let id = format!("{} marker: mds {}", fixture.name(), args.join(" "));
    match harness::run_dir(fixture, &id, args, Order::Forward, locking) {
        Ok(obs) => Some(obs),
        Err(skip) => {
            harness::announce_skip(&id, skip.reason());
            None
        }
    }
}

fn unlocked_run(fixture: DirFixture, args: &[&str]) -> DirObserved {
    dir_run(fixture, args, Locking::Unlocked).expect("an unlocked run is never skipped")
}

fn stdout_json(obs: &DirObserved) -> serde_json::Value {
    serde_json::from_str(&obs.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", obs.stdout))
}

/// The `file` keys of a JSON report, in order.
fn file_keys(json: &serde_json::Value) -> Vec<String> {
    json["files"]
        .as_array()
        .unwrap_or_else(|| panic!("no files array in {json}"))
        .iter()
        .map(|entry| {
            entry["file"]
                .as_str()
                .unwrap_or_else(|| panic!("an entry without a file key in {json}"))
                .to_string()
        })
        .collect()
}

/// The entry of a JSON report for `file`.
fn entry<'j>(json: &'j serde_json::Value, file: &str) -> &'j serde_json::Value {
    json["files"]
        .as_array()
        .and_then(|files| files.iter().find(|e| e["file"] == file))
        .unwrap_or_else(|| panic!("no entry for {file} in {json}"))
}

const LINT_JSON: [&str; 3] = ["lint", "--format", "json"];

fn lint_json(target: &str) -> Vec<&str> {
    let mut args = LINT_JSON.to_vec();
    args.push(target);
    args
}

#[test]
fn dir_fixture_markers_hold() {
    for fixture in DirFixture::all() {
        let name = fixture.name();
        let layout = fixture.layout();
        let mut defines = 0;
        for (path, contents) in &layout.files {
            assert!(
                path.strip_prefix("d/").is_some_and(|rest| rest
                    .split('/')
                    .all(|c| !c.is_empty() && c != "." && c != "..")),
                "{name}: {path} is a plain path under d/"
            );
            assert!(!path.contains('\\'), "{name}: backslash in {path}");
            assert!(!contents.contains('\\'), "{name}: backslash in {path}");
            defines += contents.matches("@define").count();
        }
        assert!(
            defines <= 1,
            "{name}: at most one @define block per fixture"
        );
        assert_eq!(
            !layout.locks.is_empty(),
            fixture.unix_only(),
            "{name}: permission changes on exactly the unix-only fixture"
        );
        for lock in &layout.locks {
            let target = lock.path();
            assert!(
                target == harness::DIR_ARG
                    || layout
                        .files
                        .iter()
                        .any(|(p, _)| { p == target || p.starts_with(&format!("{target}/")) }),
                "{name}: {lock:?} names a fixture path"
            );
        }
    }

    // empty: `d` exists and holds nothing, and there is nothing to lint.
    let layout = DirFixture::Empty.layout();
    assert!(layout.files.is_empty());
    assert_eq!(layout.dirs, [harness::DIR_ARG]);
    let obs = unlocked_run(DirFixture::Empty, &lint_json("d"));
    assert!(obs.files.is_empty());
    assert_eq!(obs.exit, 2, "empty: {}", obs.stderr);

    // all-excluded: the one source has a finding when named directly (positive
    // control), but lies under node_modules, so the tree has nothing to lint.
    let obs = unlocked_run(DirFixture::AllExcluded, &lint_json("d/node_modules/a.mds"));
    assert_eq!(obs.exit, 1, "all-excluded: {}", obs.stderr);
    assert_eq!(
        rules_of(&diagnostics(&stdout_json(&obs))),
        vec!["unused-variable"]
    );
    let obs = unlocked_run(DirFixture::AllExcluded, &lint_json("d"));
    assert_eq!(obs.exit, 2, "all-excluded: {}", obs.stderr);

    // mixed: the report lists the files with findings or errors in byte order ...
    let obs = unlocked_run(DirFixture::Mixed, &lint_json("d"));
    let json = stdout_json(&obs);
    let keys = file_keys(&json);
    assert_eq!(
        keys,
        [
            "B.mds",
            "_unterminated.mds",
            "api-utils.mds",
            "api/x.mds",
            "big.mds",
            "strict/w.mds",
            "sub/z.mds"
        ]
    );
    // ... which the names make differ from path-component and case-insensitive order.
    let mut by_components = keys.clone();
    by_components.sort_by(|a, b| std::path::Path::new(a).cmp(std::path::Path::new(b)));
    assert_ne!(by_components, keys, "mixed: component order must differ");
    let mut caseless = keys.clone();
    caseless.sort_by_key(|k| k.to_lowercase());
    assert_ne!(caseless, keys, "mixed: case-insensitive order must differ");
    // Per-file error entries: an analysis failure, a malformed nested mds.json, a file
    // over the size limit.
    assert!(entry(&json, "_unterminated.mds")["error"]["code"].is_string());
    assert_eq!(entry(&json, "sub/z.mds")["error"]["code"], "mds::io");
    assert_eq!(
        entry(&json, "big.mds")["error"]["code"],
        "mds::resource_limit"
    );
    // The nested mds.json raises the warning to an error in its own subtree only.
    assert_eq!(
        entry(&json, "strict/w.mds")["diagnostics"][0]["severity"],
        "error"
    );
    assert_eq!(
        entry(&json, "api-utils.mds")["diagnostics"][0]["severity"],
        "warn"
    );
    // Every summary bucket holds a file.
    assert!(
        obs.stderr
            .contains("1 clean, 1 with warnings, 5 with errors, 1 resource-limited"),
        "mixed: {}",
        obs.stderr
    );
    // The entries the walk skips have findings when named directly (positive controls).
    for path in ["d/.hidden/h.mds", "d/node_modules/n.mds"] {
        let obs = unlocked_run(DirFixture::Mixed, &lint_json(path));
        assert_eq!(obs.exit, 1, "{path}: {}", obs.stderr);
    }

    #[cfg(unix)]
    {
        // write-fail: unlocked (positive control), all three sources are listed and
        // linted and `--fix` rewrites the fixable one ...
        let fixture = DirFixture::Outcome(Fixture::WriteFail);
        let obs = unlocked_run(fixture, &lint_json("d"));
        let json = stdout_json(&obs);
        assert_eq!(
            file_keys(&json),
            ["unreadable.mds", "unreadable/y.mds", "x.mds"]
        );
        for key in file_keys(&json) {
            assert_eq!(
                entry(&json, &key)["diagnostics"][0]["rule"],
                "unreachable-branch"
            );
        }
        let fix = ["lint", "--fix", "d"];
        let obs = unlocked_run(fixture, &fix);
        let x = obs
            .files
            .iter()
            .find(|(n, _)| n == "d/x.mds")
            .expect("d/x.mds");
        assert!(
            matches!(x.1, ObservedFile::Changed(_)),
            "write-fail control"
        );
        // ... and locked, the unreadable file is listed but cannot be read, and
        // nothing is rewritten.
        if let Some(obs) = dir_run(fixture, &lint_json("d"), Locking::Applied) {
            let json = stdout_json(&obs);
            assert!(entry(&json, "unreadable.mds")["error"]["code"].is_string());
            assert_eq!(
                entry(&json, "x.mds")["diagnostics"][0]["rule"],
                "unreachable-branch"
            );
        }
        if let Some(obs) = dir_run(fixture, &fix, Locking::Applied) {
            assert!(
                obs.files.iter().all(|(_, f)| *f == ObservedFile::Unchanged),
                "write-fail: {:?}",
                obs.files
            );
            assert_ne!(obs.exit, 0, "write-fail");
        }
    }
}
