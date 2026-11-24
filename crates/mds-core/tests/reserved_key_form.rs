//! Reserved frontmatter key forms through the public virtual APIs.

use std::collections::HashMap;

use mds::{CompileResult, LintConfig, MdsError};

const ENTRY: &str = "entry.mds";
const IMPORTS_FORM: &str = "write `imports:` at the start of a line";
const TYPE_FORM: &str = "write `type:` at the start of a line";
const GENERIC_FORM: &str = "write reserved frontmatter keys at the start of a line";
const IMPORTS_VALUE: &str = "[{path: ./fixture-marker.mds}]";

fn frontmatter(raw: &str, body: &str) -> String {
    format!("---\n{raw}\n---\n{body}")
}

fn with_entry(source: String) -> HashMap<String, String> {
    HashMap::from([
        (ENTRY.to_owned(), source),
        ("fixture-marker.mds".to_owned(), "Dependency\n".to_owned()),
    ])
}

fn assert_form_error(modules: HashMap<String, String>, form: &str, case: &str) {
    let results: [(&str, Result<(), MdsError>); 3] = [
        (
            "compile",
            mds::compile_virtual(modules.clone(), ENTRY, None).map(drop),
        ),
        ("check", mds::check_virtual(modules.clone(), ENTRY, None)),
        (
            "lint",
            mds::lint_virtual(modules, ENTRY, None, &LintConfig::default()).map(drop),
        ),
    ];
    for (api, result) in results {
        let error = result
            .expect_err(&format!("{api}: {case} must return no result"))
            .serialize();
        assert_eq!(error.code, "mds::yaml", "{api}: {case}");
        assert_eq!(
            error.message,
            format!("YAML parse error: {form}"),
            "{api}: {case}"
        );
    }
}

fn assert_accepts(modules: HashMap<String, String>) -> CompileResult {
    mds::check_virtual(modules.clone(), ENTRY, None).expect("check accepts the control");
    mds::lint_virtual(modules.clone(), ENTRY, None, &LintConfig::default())
        .expect("lint accepts the control");
    mds::compile_virtual(modules, ENTRY, None).expect("compile accepts the control")
}

#[test]
fn reserved_keys_in_non_plain_forms_return_fixed_errors() {
    for (raw, form) in [
        (format!("'imports': {IMPORTS_VALUE}"), IMPORTS_FORM),
        ("'type': mds".to_owned(), TYPE_FORM),
        (format!("imports : {IMPORTS_VALUE}"), IMPORTS_FORM),
        (format!("? imports\n: {IMPORTS_VALUE}"), IMPORTS_FORM),
        ("{type: mds, note: keep}".to_owned(), TYPE_FORM),
        (
            format!("{{note: keep,\nimports: {IMPORTS_VALUE}}}"),
            IMPORTS_FORM,
        ),
        (
            format!("imports: &shared {IMPORTS_VALUE}\ninfo: *shared"),
            IMPORTS_FORM,
        ),
    ] {
        assert_form_error(with_entry(frontmatter(&raw, "Body\n")), form, &raw);
    }
}

#[test]
fn column_zero_sequence_under_a_reserved_key_returns_the_key_form_error() {
    for suffix in ["", "\nnote: keep"] {
        let raw = format!("imports:\n  - path: ./fixture-marker.mds{suffix}");
        let output = assert_accepts(with_entry(frontmatter(&raw, "Body\n")))
            .into_markdown()
            .unwrap();
        let expected = if suffix.is_empty() {
            "Body\n".to_owned()
        } else {
            frontmatter("note: keep", "Body\n")
        };
        assert_eq!(output, expected);

        let raw = format!("imports:\n- path: ./fixture-marker.mds{suffix}");
        assert_form_error(with_entry(frontmatter(&raw, "Body\n")), IMPORTS_FORM, &raw);
    }
}

fn import_path_module(raw: &str, path: &str) -> HashMap<String, String> {
    HashMap::from([
        (ENTRY.to_owned(), frontmatter(raw, "{{lib.word()}}\n")),
        (path.to_owned(), "@define word():\nBody\n@end\n".to_owned()),
    ])
}

#[test]
fn folded_import_path_cannot_leave_a_scalar_line_in_output() {
    let path = "lib#x.mds";
    let control =
        "imports:\n  - path: \"./lib#x.mds\"\n# Keep this comment\n    as: lib\nname: World";
    let output = assert_accepts(import_path_module(control, path))
        .into_markdown()
        .unwrap();
    assert_eq!(
        output,
        frontmatter("# Keep this comment\nname: World", "Body\n")
    );

    let raw = "imports:\n  - path: \"./lib\\\n#x.mds\"\n    as: lib\nname: World";
    assert_form_error(import_path_module(raw, path), IMPORTS_FORM, "folded path");
}

#[test]
fn single_quoted_flow_import_path_cannot_leave_a_scalar_line_in_output() {
    let path = "x #y.mds";
    let control = "imports: [{path: './x #y.mds',\n# Keep this comment\n  as: lib}]\ntitle: T";
    let output = assert_accepts(import_path_module(control, path))
        .into_markdown()
        .unwrap();
    assert_eq!(
        output,
        frontmatter("# Keep this comment\ntitle: T", "Body\n")
    );

    let raw = "imports: [{path: './x\n#y.mds', as: lib}]\ntitle: T";
    assert_form_error(
        import_path_module(raw, path),
        IMPORTS_FORM,
        "single-quoted path",
    );
}

#[test]
fn block_frontmatter_accepts_a_flow_mapping_as_its_first_key() {
    for field in [
        "{a: 1}: x",
        r#"{a: {b: "}\""}} : "}""#,
        r#"{a: &a 1, *a:"}"}: x"#,
    ] {
        let raw = format!("{field}\nimports: []");
        let output = assert_accepts(with_entry(frontmatter(&raw, "Body\n")))
            .into_markdown()
            .unwrap();
        assert_eq!(output, frontmatter(field, "Body\n"));
    }
}

#[test]
fn reserved_keys_preserve_fields_after_yaml_only_line_breaks() {
    for separator in ['\u{85}', '\u{2028}', '\u{2029}'] {
        for reserved in ["type: mds", "imports: []", "imports: [] # marker"] {
            let raw = format!("{reserved}{separator}name: x");
            let output = assert_accepts(with_entry(frontmatter(&raw, "Body\n")))
                .into_markdown()
                .unwrap();
            assert_eq!(output, frontmatter("name: x", "Body\n"));
        }
    }
}

#[test]
fn a_strip_that_changes_only_other_yaml_returns_a_generic_error() {
    let raw = format!("info: 'before\nimports: {IMPORTS_VALUE}\nafter'");
    assert_form_error(
        with_entry(frontmatter(&raw, "Body\n")),
        GENERIC_FORM,
        "multiline value",
    );

    let raw = format!("info: 'before\nnote: {IMPORTS_VALUE}\nafter'");
    let control = frontmatter(&raw, "Body\n");
    let output = assert_accepts(with_entry(control.clone()))
        .into_markdown()
        .unwrap();
    assert_eq!(output, control);
}

#[test]
fn plain_reserved_keys_are_removed_without_changing_other_bytes() {
    let raw = concat!(
        "type: mds # file type\n",
        "imports:\n",
        "  - path: ./fixture-marker.mds\n",
        "\n",
        "# Keep this comment\n",
        "  - path: ./second.mds\n",
        "indented-directives: false\n",
        "info: [{path: ./fixture-marker.mds}]",
    );
    let mut modules = with_entry(frontmatter(raw, "Body\n"));
    modules.insert("second.mds".to_owned(), "Second dependency\n".to_owned());
    let output = assert_accepts(modules).into_markdown().unwrap();
    assert_eq!(
        output,
        frontmatter(
            &format!("# Keep this comment\ninfo: {IMPORTS_VALUE}"),
            "Body\n"
        )
    );
}

#[test]
fn frontmatter_without_reserved_keys_keeps_its_bytes() {
    for raw in [
        "type: post\ninfo: [./fixture-marker.mds]",
        "{type: post, note: keep}",
    ] {
        let source = frontmatter(raw, "Body\n");
        let output = assert_accepts(with_entry(source.clone()))
            .into_markdown()
            .unwrap();
        assert_eq!(output, source);
    }
}

#[test]
fn every_module_kind_reports_the_same_error_in_both_output_modes() {
    for messages in [false, true] {
        let body = if messages {
            "@message user:\nBody\n@end\n"
        } else {
            "Body\n"
        };
        let base = format!("@block content:\n{body}@end\n");
        let child = "@extends \"./target.mds\"\n";
        let importing = format!(
            "@import \"./target.mds\" as target\n{}",
            body.replace("Body", "{{target.word()}}")
        );
        for (case, path, entry, dependency) in [
            ("standalone entry", ENTRY, body, ""),
            (
                "imported module",
                "target.mds",
                &importing,
                "@define word():\nBody\n@end\n",
            ),
            ("extends base", "target.mds", child, &base),
            ("extends entry", ENTRY, child, &base),
        ] {
            let modules = |key| {
                let mut modules = with_entry(entry.to_owned());
                modules.insert("target.mds".to_owned(), dependency.to_owned());
                let source = modules.get_mut(path).unwrap();
                *source = frontmatter(&format!("{key}: {IMPORTS_VALUE}"), source);
                modules
            };
            let control = assert_accepts(modules("imports"));
            if messages {
                let output = control.into_messages().unwrap();
                assert_eq!(output.len(), 1, "{case}");
                assert_eq!(output[0].role, "user", "{case}");
                assert_eq!(output[0].content, "Body", "{case}");
            } else {
                assert_eq!(control.into_markdown().unwrap(), "Body\n", "{case}");
            }
            assert_form_error(modules("'imports'"), IMPORTS_FORM, case);
        }
    }
}
