//! Guard (#313): the spec §5 "Error Codes" registry stays in sync with the `code(mds::…)`
//! attributes `error.rs` actually declares.
//!
//! A registry hand-maintained in `spec.md` drifts silently the moment a new `MdsError`
//! variant is added or a `code(mds::…)` name changes and nobody updates the prose table
//! next to it. This test converts "is every code documented?" into a machine-checked
//! invariant: every `code(mds::<name>)` attribute in `crates/mds-core/src/error.rs` must
//! appear as a backticked `` `mds::<name>` `` cell inside the spec's "### Error Codes"
//! section. The same section must list the codes that only the bindings return.
//!
//! Both files are read `CARGO_MANIFEST_DIR`-relative, the same pattern used by
//! `yaml_funnel.rs` and `assert_promotions.rs`.

use std::path::Path;

use mds::MdsError;

#[test]
fn data_error_names_its_location_without_a_source_span() {
    let err = MdsError::data(
        "sub/mo\tdule.mds",
        Some("en\x1btry"),
        Some("fi\neld"),
        "expected a string",
        Some("quote the value".to_owned()),
    );
    let serialized = err.serialize();
    let message = concat!(
        r#"data error in "sub/mo\u"#,
        r#"0009dule.mds", entry "en\u"#,
        r#"001Btry", field "fi\u"#,
        r#"000Aeld": expected a string"#,
    );
    assert_eq!(serialized.code, "mds::data");
    assert_eq!(err.to_string(), message);
    assert_eq!(serialized.message, message);
    assert_eq!(serialized.help.as_deref(), Some("quote the value"));
    assert_eq!(serialized.span, None);
    assert_eq!(err.source_name(), None);
}

#[test]
fn data_error_can_name_the_module_without_an_entry_or_field() {
    let err = MdsError::data("module.mds", None, None, "expected a mapping", None);
    let serialized = err.serialize();
    assert_eq!(
        serialized.message,
        r#"data error in "module.mds": expected a mapping"#
    );
    assert_eq!(serialized.help, None);
}

#[test]
fn network_denied_names_each_frontier_entry_without_a_source_span() {
    let err = MdsError::network_denied(&[("sub/mod\nule.mds", "d\x1bata"), ("main.mds", "next")]);
    let serialized = err.serialize();
    let message = concat!(
        r#"network access denied for data entries: "sub/mod\u"#,
        r#"000Aule.mds", entry "d\u"#,
        r#"001Bata"; "main.mds", entry "next""#,
    );
    assert_eq!(serialized.code, "mds::network_denied");
    assert_eq!(err.to_string(), message);
    assert_eq!(serialized.message, message);
    assert_eq!(
        serialized.help.as_deref(),
        Some("provide a runtime value for each entry to replay it without network access")
    );
    assert_eq!(serialized.span, None);
    assert_eq!(err.source_name(), None);
}

/// Codes the napi, WASM, or Python bindings return but `mds-core` does not declare.
const BINDING_ONLY_CODES: &[&str] = &[
    "mds::invalid_options",
    "mds::filename_collision",
    "mds::invalid_backend_result",
];

fn read_error_rs() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/error.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn read_spec_md() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Extract every `mds::<name>` from a `code(mds::<name>)` attribute in `error.rs`
/// source text. Hand-rolled (no `regex` dependency): scans for the literal needle
/// `code(mds::`, then walks forward while the following bytes are `[a-z_]`, and keeps
/// the match only when the run is immediately closed by `)`.
///
/// The scan loop is bounded by the source length: each iteration advances `search_from`
/// past the just-found needle occurrence, so the number of iterations can never exceed
/// the byte length of `source`.
fn extract_codes(source: &str) -> Vec<String> {
    const NEEDLE: &str = "code(mds::";
    let bytes = source.as_bytes();
    let max_iterations = source.len() + 1;
    let mut codes = Vec::new();
    let mut search_from = 0usize;
    let mut iterations = 0usize;

    while let Some(rel_pos) = source[search_from..].find(NEEDLE) {
        iterations += 1;
        assert!(
            iterations <= max_iterations,
            "unbounded scan: extract_codes exceeded a byte-count-safe iteration bound"
        );

        let name_start = search_from + rel_pos + NEEDLE.len();
        let mut name_end = name_start;
        while name_end < bytes.len()
            && (bytes[name_end].is_ascii_lowercase() || bytes[name_end] == b'_')
        {
            name_end += 1;
        }

        if name_end < bytes.len() && bytes[name_end] == b')' && name_end > name_start {
            codes.push(format!("mds::{}", &source[name_start..name_end]));
        }

        // Always past the just-found needle text, so this strictly advances.
        search_from = name_start;
    }

    codes
}

/// Isolate the spec's "### Error Codes" section: from that heading (inclusive) up to
/// — but not including — the next line that is exactly `---` or starts with `## `.
///
/// Bounded by the number of lines in `spec` after the heading.
fn error_codes_section(spec: &str) -> Option<String> {
    let heading = "### Error Codes";
    let heading_start = spec.find(heading)?;
    let after = &spec[heading_start..];

    let max_iterations = after.matches('\n').count() + 1;
    let mut iterations = 0usize;
    let mut search_from = heading.len();

    loop {
        if search_from >= after.len() {
            return Some(after.to_string());
        }
        let rest = &after[search_from..];
        let line_end = rest.find('\n').map_or(after.len(), |i| search_from + i);
        let line = &after[search_from..line_end];
        if line == "---" || line.starts_with("## ") {
            return Some(after[..search_from].to_string());
        }

        search_from = line_end + 1;
        iterations += 1;
        assert!(
            iterations <= max_iterations,
            "unbounded scan: error_codes_section exceeded a line-count-safe iteration bound"
        );
    }
}

/// Whether `code` (e.g. `"mds::syntax"`) appears as a backticked table cell inside
/// `section`.
fn documented(section: &str, code: &str) -> bool {
    section.contains(&format!("`{code}`"))
}

#[test]
fn core_codes_are_documented_in_spec() {
    let error_rs = read_error_rs();
    let codes = extract_codes(&error_rs);

    // Non-vacuity precondition: the scan actually found something to check. No
    // hard-coded `== 26` here — a new code landing later must not make this test
    // brittle, only the missing-documentation check below should ever fail it.
    assert!(
        !codes.is_empty(),
        "non-vacuity: expected at least one code(mds::…) attribute in {}",
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/error.rs")
            .display()
    );

    let spec = read_spec_md();
    let section =
        error_codes_section(&spec).expect("spec.md must contain a \"### Error Codes\" section");
    assert!(
        !section.is_empty(),
        "the \"### Error Codes\" section must not be empty"
    );

    let missing: Vec<&str> = codes
        .iter()
        .map(String::as_str)
        .filter(|code| !documented(&section, code))
        .collect();

    assert!(
        missing.is_empty(),
        "code(mds::…) attributes in error.rs with no matching `mds::<name>` cell in \
         spec.md's \"### Error Codes\" section: {missing:?}"
    );
}

#[test]
fn data_codes_have_core_declarations_and_spec_exit_classes() {
    let codes = extract_codes(&read_error_rs());
    let section = error_codes_section(&read_spec_md()).expect("spec error registry");
    for (code, exits) in [("mds::data", "1 / 2"), ("mds::network_denied", "2 / 2")] {
        assert!(codes.iter().any(|declared| declared == code), "{code}");
        let row = section
            .lines()
            .find(|line| line.starts_with(&format!("| `{code}` |")))
            .expect("spec error row");
        assert_eq!(row.split('|').nth(4).map(str::trim), Some(exits), "{code}");
    }
}

#[test]
fn core_codes_are_not_binding_only() {
    let core = extract_codes(&read_error_rs());
    assert!(core.iter().any(|code| code == "mds::internal"));
    let overlap: Vec<_> = core
        .iter()
        .filter(|code| BINDING_ONLY_CODES.contains(&code.as_str()))
        .collect();
    assert!(
        overlap.is_empty(),
        "core codes listed as binding-only: {overlap:?}"
    );
}

#[test]
fn binding_only_codes_are_documented_in_spec() {
    let spec = read_spec_md();
    let section =
        error_codes_section(&spec).expect("spec.md must contain a \"### Error Codes\" section");

    let missing: Vec<&str> = BINDING_ONLY_CODES
        .iter()
        .copied()
        .filter(|code| !documented(&section, code))
        .collect();

    assert!(
        missing.is_empty(),
        "binding-only codes with no matching `mds::<name>` cell in spec.md's \
         \"### Error Codes\" section: {missing:?}"
    );
}

/// Non-vacuity control for `documented`: a code that was never registered anywhere
/// must read as undocumented. Without this, a `documented` that always returns `true`
/// (e.g. a typo that made the `contains` check vacuous) would pass both tests above
/// silently.
#[test]
fn documented_rejects_a_code_not_in_the_section() {
    let spec = read_spec_md();
    let section =
        error_codes_section(&spec).expect("spec.md must contain a \"### Error Codes\" section");

    assert!(
        !documented(&section, "mds::definitely_not_a_code"),
        "negative control failed: a bogus code must not read as documented"
    );
}
