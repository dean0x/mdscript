use mds::{compile_virtual, MdsError};

const MARKER: &str = "./fixture-marker.yml";
const BODY: &str = "Body\n";

fn compile(entry: &str, source: String) -> Result<String, MdsError> {
    compile_virtual(
        [
            (entry.into(), source),
            ("fixture-marker.yml".into(), "Local".into()),
        ]
        .into(),
        entry,
        None,
    )?
    .into_markdown()
}

#[test]
fn plain_data_headers_do_not_reach_output() {
    for (header, kept) in [
        (format!("data:\n  local: {MARKER}"), ""),
        (
            format!("data:\n# Data comment\n  local: {MARKER}"),
            "# Data comment\n",
        ),
        (format!("data: {{local: {MARKER}}}"), ""),
    ] {
        let info = format!("info: {MARKER}");
        let source = format!("---\n{header}\n{info}\n---\n{{{{local}}}}\n");
        let output = compile("main.mds", source).unwrap();
        assert_eq!(
            output,
            format!("---\n{kept}{info}\n---\nLocal\n"),
            "{header}"
        );
    }
}

#[test]
fn markdown_with_mds_type_strips_data() {
    let info = format!("info: {MARKER}");
    let source = format!("---\ntype: mds\ndata: {{local: {MARKER}}}\n{info}\n---\n{{{{local}}}}\n");
    assert_eq!(
        compile("main.md", source).unwrap(),
        format!("---\n{info}\n---\nLocal\n")
    );
}

#[test]
fn non_plain_data_key_forms_return_the_fixed_form_error() {
    let plain = format!("data: {{local: {MARKER}}}");
    let control = format!("---\n'info': {{local: {MARKER}}}\n---\n{BODY}");
    assert_eq!(compile("main.mds", control.clone()).unwrap(), control);
    for header in [
        format!("'data': {{local: {MARKER}}}"),
        format!("{{{plain}}}"),
    ] {
        let error = compile("main.mds", format!("---\n{header}\n---\n{BODY}"))
            .expect_err("a non-plain data key must fail")
            .serialize();
        assert_eq!(error.code, "mds::yaml", "{header}");
        assert_eq!(
            error.message,
            "YAML parse error: write `data:` at the start of a line"
        );
    }
}

#[test]
fn extends_merges_info_but_omits_both_data_headers() {
    let child = format!(
        "---\ndata: {{local: {MARKER}}}\ninfo: {{child: kept}}\n---\n@extends \"./base.mds\"\n"
    );
    let base = format!("---\ndata: {{base: {MARKER}}}\ninfo: {{base: kept}}\n---\n@block body:\n{{{{base}}}} {{{{local}}}}\n@end\n");
    let output = compile_virtual(
        [
            ("main.mds".into(), child),
            ("base.mds".into(), base),
            ("fixture-marker.yml".into(), "Local".into()),
        ]
        .into(),
        "main.mds",
        None,
    )
    .unwrap()
    .into_markdown()
    .unwrap();
    assert!(
        output.contains("info:\n  base: kept\n  child: kept\n"),
        "{output}"
    );
    assert!(output.ends_with("Local Local\n"), "{output}");
    assert!(!output.contains("data:"), "{output}");
    assert!(!output.contains(MARKER), "{output}");
}
