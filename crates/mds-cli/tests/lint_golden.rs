//! Characterization goldens for `mds lint` (#309): stdin and single-file output.
//!
//! Every cell of the matrix — input (stdin with cwd = the fixture dir, or a relative
//! file) × format (human, json) × `--quiet` (off, on) × fix mode (none, `--fix`,
//! `--fix --check`, `--fix --diff`, `--fix --check --diff`) × outcome fixture — runs
//! `mds lint` once and must reproduce its golden byte for byte: exit code, stdout,
//! stderr and the source file's bytes afterwards. The goldens pin today's output,
//! including output that is known to be wrong; a commit that changes lint output
//! regenerates them (see `golden_print_mode`) and names the changed cell ids.
//!
//! Machinery, fixtures and normalization rules: `lint_golden/harness.rs`.
//! Generated data: `lint_golden/single.rs`.

#[path = "lint_golden/harness.rs"]
mod harness;
#[path = "lint_golden/single.rs"]
mod single;

use harness::{
    Cell, Digest, FileAfter, FixMode, Fixture, Format, Golden, Input, Normalizer, Observed,
    ObservedFile, Quiet, Stream,
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

// ── Generator ────────────────────────────────────────────────────────────────

/// With `MDS_GOLDEN_PRINT=1`, runs every cell and prints the data module for
/// `lint_golden/single.rs` on stderr between BEGIN/END markers; never writes a file.
/// Without it, does nothing.
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
}

// ── Coverage self-checks ─────────────────────────────────────────────────────

#[test]
fn matrix_has_the_literal_cell_counts() {
    let all = harness::all_cells();
    assert_eq!(all.len(), 520, "13 fixtures x 20 variants x 2 inputs");
    let active = harness::active_cells();
    let stdin = active.iter().filter(|c| c.input == Input::Stdin).count();
    let file = active.iter().filter(|c| c.input == Input::File).count();
    if cfg!(unix) {
        assert_eq!((active.len(), stdin, file), (520, 260, 260));
    } else {
        assert_eq!((active.len(), stdin, file), (480, 240, 240));
    }
}

#[test]
fn every_cell_has_exactly_one_golden_and_no_golden_is_dead() {
    let all: Vec<String> = harness::all_cells().iter().map(Cell::id).collect();
    let mut table: Vec<&str> = single::CELLS.iter().map(|(id, _)| *id).collect();
    table.sort_unstable();
    let before = table.len();
    table.dedup();
    assert_eq!(before, table.len(), "a cell id appears twice in the table");
    let mut expected: Vec<&str> = all.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(
        table, expected,
        "the cell table must list exactly the matrix cells"
    );

    let mut used = vec![false; single::GOLDENS.len()];
    for (id, index) in single::CELLS {
        let slot = used
            .get_mut(usize::from(*index))
            .unwrap_or_else(|| panic!("cell {id} maps to missing golden {index}"));
        *slot = true;
    }
    let dead: Vec<usize> = (0..used.len()).filter(|&i| !used[i]).collect();
    assert!(dead.is_empty(), "goldens no cell maps to: {dead:?}");

    for (i, a) in single::GOLDENS.iter().enumerate() {
        for (j, b) in single::GOLDENS.iter().enumerate().skip(i + 1) {
            assert_ne!(a, b, "goldens {i} and {j} are identical (not deduplicated)");
        }
    }
}

#[test]
fn golden_storage_stays_within_bounds() {
    fn check_stream(i: usize, s: &Stream) {
        match s {
            Stream::Text(t) => assert!(
                t.len() <= harness::DIGEST_THRESHOLD,
                "golden {i}: a {}-byte inline stream must be a digest",
                t.len()
            ),
            Stream::Digest(d) => {
                assert!(
                    d.len > harness::DIGEST_THRESHOLD,
                    "golden {i}: a {}-byte stream must be inline",
                    d.len
                );
                assert!(d.head.len() <= harness::DIGEST_HEAD_MAX_BYTES);
                assert!(d.tail.len() <= harness::DIGEST_TAIL_MAX_BYTES);
                assert!(d.head.split_inclusive('\n').count() <= harness::DIGEST_HEAD_LINES);
                assert!(d.tail.split_inclusive('\n').count() <= harness::DIGEST_TAIL_LINES);
            }
        }
    }
    for (i, g) in single::GOLDENS.iter().enumerate() {
        check_stream(i, &g.stdout);
        check_stream(i, &g.stderr);
        if let FileAfter::Changed(s) = &g.file {
            check_stream(i, s);
        }
        for text in harness::golden_strings(g) {
            let bad: Vec<char> = text
                .chars()
                .filter(|c| c.is_control() && *c != '\n')
                .collect();
            assert!(
                bad.is_empty(),
                "golden {i} holds control characters {bad:?}"
            );
            assert!(!text.contains('\\'), "golden {i} holds a backslash");
        }
    }
    let bytes = harness::golden_data_bytes(single::GOLDENS);
    assert!(
        bytes <= harness::GOLDEN_DATA_CAP,
        "golden data is {bytes} bytes, over the {} cap",
        harness::GOLDEN_DATA_CAP
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

fn observed_from(g: &Golden) -> Observed {
    fn text(s: &Stream) -> String {
        match s {
            Stream::Text(t) => (*t).to_string(),
            Stream::Digest(_) => panic!("pick a golden with inline streams"),
        }
    }
    Observed {
        exit: g.exit,
        stdout: text(&g.stdout),
        stderr: text(&g.stderr),
        file: match &g.file {
            FileAfter::Unchanged => ObservedFile::Unchanged,
            FileAfter::Missing => ObservedFile::Missing,
            FileAfter::Changed(s) => ObservedFile::Changed(text(s)),
        },
        tmp_paths: g.tmp_paths,
        tmp_names: g.tmp_names,
    }
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
        Err(skip) => eprintln!("skipping the write-fail marker: {skip:?}"),
    }
}
