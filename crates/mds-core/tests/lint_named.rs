//! #179: `mds::lint_str_named` lints a source string under a file name the caller
//! supplies. The name is the `file` each finding carries, and a name whose last
//! component starts with `_` is linted as a partial — exactly as a file of that name
//! is linted by `mds::lint`. A name carrying a forbidden path character is refused
//! with `mds::io` before anything is linted. `mds::lint_str_with` lints under
//! `mds::STRING_SOURCE_MAP_LABEL` and returns, byte for byte, what `lint_str_named`
//! returns under that name.
//!
//! Black-box: every test drives the public API. Each hostile character is built at
//! runtime from the forbidden class itself, so no live control byte is ever written
//! into this file.

use std::collections::HashMap;
use std::path::Path;

use mds::{is_forbidden_path_char, LintConfig, LintResult, MdsError, Value};

// ── Helpers ─────────────────────────────────────────────────────────────────

/// A source with one finding that partials are spared: its frontmatter variable is
/// never used in the body.
const UNUSED_VARIABLE: &str = "---\nunused: 1\n---\nHello!\n";

/// The rule ids of a result's findings, in order.
fn rules(result: &LintResult) -> Vec<&str> {
    result.diagnostics.iter().map(|d| d.rule.as_str()).collect()
}

/// The `file` of each of a result's findings, in order.
fn files(result: &LintResult) -> Vec<Option<&str>> {
    result
        .diagnostics
        .iter()
        .map(|d| d.file.as_deref())
        .collect()
}

/// The `mds::…` diagnostic code of an error.
fn code_of(err: &MdsError) -> String {
    miette::Diagnostic::code(err)
        .map(|c| c.to_string())
        .unwrap_or_default()
}

fn named(source: &str, name: &str) -> Result<LintResult, MdsError> {
    mds::lint_str_named(source, None, None, &LintConfig::default(), name)
}

// ── A name starting with `_` is a partial ───────────────────────────────────

/// Under an ordinary name the unused frontmatter variable is reported and the source
/// is standalone; under a name whose last component starts with `_` it is a partial:
/// nothing is reported and it is not standalone. Only the last component counts.
#[test]
fn an_underscore_name_lints_the_source_as_a_partial() {
    for name in ["x.mds", "_d/x.mds"] {
        let result = named(UNUSED_VARIABLE, name).expect("an ordinary name lints");
        assert_eq!(rules(&result), ["unused-variable"], "{name}");
        assert_eq!(files(&result), [Some(name)], "{name}");
        assert!(
            result.is_standalone,
            "{name}: an ordinary file is standalone"
        );
    }
    for name in ["_x.mds", "d/_x.mds"] {
        let result = named(UNUSED_VARIABLE, name).expect("a partial's name lints");
        assert_eq!(rules(&result), Vec::<&str>::new(), "{name}");
        assert!(!result.is_standalone, "{name}: a partial is not standalone");
    }
}

/// `lint_str_named` under a file's name returns what `mds::lint` returns for that
/// file, for a partial and for an ordinary file alike.
#[test]
fn a_named_source_lints_as_the_file_of_that_name_does() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::write(dir.path().join(".mdsroot"), "").expect("the root marker");
    for name in ["x.mds", "_x.mds"] {
        let path = dir.path().join(name);
        std::fs::write(&path, UNUSED_VARIABLE).expect("the fixture file");
        let from_file = mds::lint(&path, None, &LintConfig::default()).expect("the file lints");
        let from_string = named(UNUSED_VARIABLE, name).expect("the string lints");
        assert_eq!(
            from_string.to_canonical_json(),
            from_file.to_canonical_json(),
            "{name}"
        );
        assert_eq!(from_string.is_standalone, from_file.is_standalone, "{name}");
    }
}

// ── A forbidden name is refused ─────────────────────────────────────────────

/// Every one of the 80 forbidden codepoints in the name is refused with `mds::io`,
/// naming the codepoint and the name escaped; the message carries none of the 80.
/// Refused before the source is compiled: an invalid source changes nothing.
#[test]
fn a_name_with_a_forbidden_character_is_refused_with_the_io_code() {
    let forbidden: Vec<char> = (0..=0x10_FFFF_u32)
        .filter_map(char::from_u32)
        .filter(|&c| is_forbidden_path_char(c))
        .collect();
    assert_eq!(forbidden.len(), 80, "non-vacuity: the forbidden class");

    for ch in forbidden {
        let name = format!("x{ch}.mds");
        let expected = format!(
            "file name contains forbidden character U+{:04X}: \"{}\"",
            u32::from(ch),
            mds::escape_path_for_message(&name)
        );
        for source in [UNUSED_VARIABLE, "@if x:\nhello\n"] {
            let err = named(source, &name).expect_err("a forbidden name is refused");
            assert_eq!(code_of(&err), "mds::io", "U+{:04X}: {err:?}", u32::from(ch));
            let msg = err.to_string();
            assert_eq!(msg, expected);
            assert!(
                !msg.chars().any(is_forbidden_path_char),
                "the message carries a raw forbidden character: {msg:?}"
            );
        }
    }

    // Positive control: spaces and non-ASCII letters are not in the class.
    let result = named(UNUSED_VARIABLE, "caf\u{e9} page.mds").expect("an accepted name");
    assert_eq!(files(&result), [Some("caf\u{e9} page.mds")]);
}

// ── `lint_str_with` is unchanged ────────────────────────────────────────────

/// `lint_str_with` returns exactly what `lint_str_named` returns under
/// `STRING_SOURCE_MAP_LABEL`, findings, errors and all, over sources that lint clean,
/// carry findings (a fixable one and one partials are spared), look like a partial's
/// library, need runtime variables, or fail to lint.
#[test]
fn lint_str_with_is_lint_str_named_under_the_string_label() {
    let vars = || Some(HashMap::from([("name".to_string(), Value::from("Ada"))]));
    let cases: [(&str, Option<HashMap<String, Value>>); 7] = [
        ("Hello!\n", None),
        (UNUSED_VARIABLE, None),
        ("@if \"x\" == \"x\":\nhello\n@else:\nworld\n@end\n", None),
        ("@define greet():\nHi\n@end\n@export greet\n", None),
        ("Hello {{name}}!\n", vars()),
        ("Hello {{name}}!\n", None),
        ("@if x:\nhello\n", None),
    ];
    let config = LintConfig::default();
    let (mut oks, mut errs, mut labelled) = (0, 0, 0);
    for base_dir in [None, Some(Path::new("."))] {
        for (source, runtime_vars) in &cases {
            let old = mds::lint_str_with(source, base_dir, runtime_vars.clone(), &config);
            let new = mds::lint_str_named(
                source,
                base_dir,
                runtime_vars.clone(),
                &config,
                mds::STRING_SOURCE_MAP_LABEL,
            );
            assert_eq!(format!("{old:?}"), format!("{new:?}"), "{source:?}");
            match (&old, &new) {
                (Ok(old), Ok(new)) => {
                    assert_eq!(old.to_canonical_json(), new.to_canonical_json());
                    oks += 1;
                    labelled += files(old)
                        .iter()
                        .filter(|f| **f == Some(mds::STRING_SOURCE_MAP_LABEL))
                        .count();
                }
                (Err(old), Err(new)) => {
                    assert_eq!(old.to_string(), new.to_string());
                    errs += 1;
                }
                _ => panic!("{source:?}: one lints and the other does not"),
            }
        }
    }
    // Non-vacuity: both outcomes compared, and findings labelled with the string label.
    assert_eq!((oks, errs), (10, 4));
    assert!(labelled >= 4, "findings carry the string label: {labelled}");
}
