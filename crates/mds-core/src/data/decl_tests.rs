use super::*;
use crate::resolver::parse_frontmatter_yaml;

const MODULE: &str = "sub/main.mds";
const URL: &str = "https://example.test/path";

fn parse(raw: &str) -> Result<Vec<Entry>, MdsError> {
    let tree = parse_frontmatter_yaml(&format!("data: {raw}")).unwrap();
    parse_declarations(&tree["data"], MODULE, &mut MemBudget::default())
}

fn refusal(raw: &str, field: Option<&str>) -> crate::SerializedError {
    let error = parse(raw).unwrap_err().serialize();
    assert_eq!(error.code, "mds::data", "{raw}");
    assert!(
        error.message.starts_with("data error in \"sub/main.mds\""),
        "{error:?}"
    );
    if let Some(field) = field {
        assert!(
            error.message.contains(&format!("field \"{field}\"")),
            "{error:?}"
        );
    }
    assert!(error.span.is_none(), "{error:?}");
    error
}

fn refs(raw: &str) -> Vec<String> {
    let mut entries = parse(raw).unwrap();
    match entries.remove(0).source {
        Source::Url(references) => references,
        other => panic!("expected a URL, got {other:?}"),
    }
}

#[test]
fn unknown_forms_have_rename_help() {
    assert_eq!(
        parse("{info: ./info.json}").unwrap()[0].source,
        Source::File("./info.json".into())
    );
    for raw in [
        "[./info.json]",
        "Alice",
        "null",
        "{name: Alice}",
        "{name: 1}",
        "{name: {info: Alice}}",
    ] {
        let error = refusal(raw, None);
        assert_eq!(
            error.help.as_deref(),
            Some("`data` is reserved for data sources; rename the variable")
        );
    }
}

#[test]
fn names_must_be_non_reserved_identifiers() {
    for name in [
        "prompt",
        "imports",
        "type",
        "extends",
        "data",
        "indented-directives",
        "9name",
        "two.parts",
        "café",
    ] {
        refusal(&format!("{{'{name}': ./info.json}}"), None);
    }
    refusal("{7: ./info.json}", None);
    assert_eq!(parse("{_name42: ../info.yml}").unwrap()[0].name, "_name42");
}

/// A reserved output or merge key is a reserved entry name, so a new reserved key
/// cannot become an entry name. Control: an ordinary key is not reserved.
#[test]
fn reserved_frontmatter_keys_are_reserved_entry_names() {
    let output = crate::RESERVED_OUTPUT_KEYS.iter().map(|key| key.name);
    let merge = crate::resolver::RESERVED_MERGE_KEYS.iter().copied();
    for name in output.chain(merge) {
        assert!(RESERVED_NAMES.contains(&name), "{name}");
    }
    assert!(!RESERVED_NAMES.contains(&"info"));
}

#[test]
fn names_cannot_hide_frontmatter_or_aliases() {
    let entries = parse("{item: ./info.json}").unwrap();
    let frontmatter = ["item"];
    let aliases = ["alias"];
    let check = |entries: &[Entry]| {
        check_collisions(entries, |name| {
            frontmatter.contains(&name) || aliases.contains(&name)
        })
    };
    let alias_entry = parse("{alias: ./info.json}").unwrap();
    for entries in [&entries, &alias_entry] {
        let error = check(entries).unwrap_err().serialize();
        assert_eq!(error.code, "mds::name_collision");
        assert!(error.message.contains(&entries[0].name));
        assert!(error.span.is_none());
    }
    // The caller does not pass its runtime variables as name claims.
    assert!(check(&parse("{replay: ./info.json}").unwrap()).is_ok());
}

#[test]
fn diagnostics_escape_names_and_fields_but_do_not_show_values() {
    let text = char::from_u32(27).unwrap();
    let error = refusal(r#"{"foo\e": './info.json'}"#, None);
    assert!(
        error.message.contains(concat!(r#"entry "foo\"#, "u001B\"")),
        "{error:?}"
    );
    assert!(!error.message.contains(text));

    let error = refusal(
        r#"{item: {url: 'https://example.test/path', "field\e": value_marker}}"#,
        None,
    );
    assert!(
        error
            .message
            .contains(concat!(r#"entry "item", field "field\"#, "u001B\"")),
        "{error:?}"
    );
    assert!(!error.message.contains(text));
    assert!(!error.message.contains("value_marker"));
    assert!(parse("{foo: ./info.json}").is_ok());
}

#[test]
fn entry_limit_applies_before_entry_analysis() {
    let entries = (0..MAX_DATA_ENTRIES_PER_MODULE)
        .map(|i| format!("item{i}: ./info.json"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        parse(&format!("{{{entries}}}")).unwrap().len(),
        MAX_DATA_ENTRIES_PER_MODULE
    );
    let error = parse(&format!("{{{entries}, forbidden: false}}"))
        .unwrap_err()
        .serialize();
    assert_eq!(error.code, "mds::resource_limit");
    assert!(error.message.contains(MODULE));
    assert!(error.message.contains("data entries"));
    assert!(error.span.is_none());
}

#[test]
fn exact_method_accept_and_cache_grammars() {
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        assert!(parse(&format!("{{item: {{url: '{URL}', method: {method}}}}}")).is_ok());
    }
    for accept in ["json", "yaml", "text"] {
        assert!(parse(&format!("{{item: {{url: '{URL}', accept: {accept}}}}}")).is_ok());
    }
    for cache in ["1s", "20m", "300h", "4d", "forever", "off"] {
        assert!(parse(&format!("{{item: {{url: '{URL}', cache: {cache}}}}}")).is_ok());
    }
    for (field, value) in [
        ("method", "get"),
        ("method", "TRACE"),
        ("method", "null"),
        ("accept", "JSON"),
        ("accept", "'{{name}}'"),
        ("cache", "'{{ttl}}'"),
        ("cache", "'1.5s'"),
        ("cache", "s"),
        ("cache", "'+1s'"),
        ("cache", "18446744073709551615d"),
        ("cache", "4ms"),
        ("cache", "1"),
        ("extra", "true"),
        ("url", "false"),
    ] {
        let raw = if field == "url" {
            format!("{{item: {{url: {value}}}}}")
        } else {
            format!("{{item: {{url: '{URL}', {field}: {value}}}}}")
        };
        refusal(&raw, Some(field));
    }
    refusal("{item: {method: GET}}", Some("url"));
}

#[test]
fn only_string_leaves_in_url_headers_and_body_produce_references() {
    assert_eq!(
        refs(
            r"{item: {url: 'https://example.test/{{obj.path}}?q={{query}}', headers: {X-List: '{{header}} \{{literal}} {{ next }}', X-Auth: {secret: token, prefix: 'Scheme '}}, body: {'{{literalKey}}': ['é{{body.field}}', '\{{literal}}', 3, null]}}}"
        ),
        ["obj.path", "query", "header", "next", "body.field"]
    );
    assert_eq!(
        refs("{item: 'https://example.test/{{first}}/{{first}}'}"),
        ["first", "first"]
    );
    assert!(refs(r"{item: {url: 'https://example.test/', body: '\{{literal}}'}}").is_empty());
    refusal(
        "{item: {url: 'https://example.test/', body: '{{name}}'}}",
        Some("body"),
    );
}

#[test]
fn references_accept_only_identifier_paths_within_the_segment_limit() {
    for expr in [
        "fn('}}')",
        "42",
        "true",
        "null",
        "a + b",
        "a | filter",
        "a..b",
        "a. b",
        "",
        "café",
    ] {
        refusal(
            &format!("{{item: \"https://example.test/{{{{{expr}}}}}\"}}"),
            Some("url"),
        );
    }
    refusal("{item: 'https://example.test/{{name'}", Some("url"));
    let path = vec!["a"; crate::limits::MAX_DOT_SEGMENTS].join(".");
    assert_eq!(
        refs(&format!("{{item: 'https://example.test/{{{{{path}}}}}'}}")),
        std::slice::from_ref(&path)
    );
    refusal(
        &format!("{{item: 'https://example.test/{{{{{path}.a}}}}'}}"),
        Some("url"),
    );
}

#[test]
fn bare_flow_reference_shape_gets_quote_help() {
    for (raw, field) in [
        (
            "{item: {url: 'https://example.test/', headers: {X-Value: {{name}}}}}",
            "X-Value",
        ),
        (
            "{item: {url: 'https://example.test/', body: {value: {{name}}}}}",
            "body",
        ),
    ] {
        let help = refusal(raw, Some(field)).help.unwrap();
        assert!(help.starts_with("quote the value"));
        let quoted = raw.replace("{{name}}", "'{{name}}'");
        assert_eq!(refs(&quoted), ["name"]);
    }
}

#[test]
fn header_grammar_keeps_names_and_secret_parts_literal() {
    assert_eq!(refs("{item: {url: 'https://example.test/', headers: {X-Token_1: '{{name}}', X-Auth: {secret: token, prefix: 'Scheme '}}}}"), ["name"]);
    for raw in [
        "{'{{name}}': 'value'}",
        "{'bad name': 'value'}",
        "{'café': 'value'}",
        "{X-Test: 7}",
        "{X-Test: {secret: token}}",
        "{X-Test: {secret: token, prefix: '', extra: ''}}",
        "{X-Test: {secret: 7, prefix: ''}}",
        "{X-Test: {secret: token, prefix: '{{name}}'}}",
        "{X-Test: {secret: '{{name}}', prefix: ''}}",
    ] {
        refusal(&format!("{{item: {{url: '{URL}', headers: {raw}}}}}"), None);
    }
}

#[test]
fn both_url_forms_require_a_literal_https_origin() {
    for value in [
        "http://example.test/path",
        "file:///example.json",
        "data:text/plain,test",
        "custom:test",
        "https://{{host}}/path",
        "https://example.test:{{port}}/path",
        "{{scheme}}://example.test/path",
        "https://user@example.test/path",
        "https://example.test?q={{name}}",
        "https://example.test?q=x",
        r"https://example.test\path",
        r"https://example.test/\{{name}}",
        "https://example.test/path#fragment",
        "HTTPS://example.test/path",
        "https://example.test./path",
        "https://example.test.:443/path",
        "https://example%2etest/path",
        "https://example .test/path",
        "https:///path",
    ] {
        let error = refusal(&format!("{{item: '{value}'}}"), Some("url"));
        assert!(!error.message.contains(value));
        refusal(&format!("{{item: {{url: '{value}'}}}}"), Some("url"));
    }
    assert_eq!(
        refs("{item: 'https://example.test:443/{{name}}?q=abc'}"),
        ["name"]
    );
    assert_eq!(
        refs("{item: {url: 'https://example.test/{{name}}'}}"),
        ["name"]
    );
}

#[test]
fn file_paths_do_not_accept_references_or_their_escape() {
    assert_eq!(
        parse("{item: ./name.json}").unwrap()[0].source,
        Source::File("./name.json".into())
    );
    for path in ["./{{name}}.json", r"./\{{name}}.json"] {
        refusal(&format!("{{item: '{path}'}}"), Some("path"));
    }
}

#[test]
fn declaration_memory_is_admitted_before_it_is_retained() {
    let tree =
        parse_frontmatter_yaml("one: ./info.json\ntwo: 'https://example.test/{{name}}'").unwrap();
    let mut admitted = MemBudget::default();
    assert_eq!(
        parse_declarations(&tree, MODULE, &mut admitted)
            .unwrap()
            .len(),
        2
    );
    assert!(admitted.buffers > 1);

    let mut limited = MemBudget::default();
    limited.headroom = admitted.buffers - 1;
    let original = limited;
    assert_eq!(
        parse_declarations(&tree, MODULE, &mut limited)
            .unwrap_err()
            .serialize()
            .code,
        "mds::resource_limit"
    );
    assert_eq!(limited, original);
}
