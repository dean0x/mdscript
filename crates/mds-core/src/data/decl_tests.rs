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

fn template(raw: &str) -> RequestTemplate {
    let mut entries = parse(raw).unwrap();
    match entries.remove(0).source {
        Source::Url(template) => *template,
        other => panic!("expected a URL, got {other:?}"),
    }
}

fn refs(raw: &str) -> Vec<String> {
    template(raw).refs
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
        assert!(parse(&format!(
            "{{item: {{url: '{URL}', method: {method}, envelope: true}}}}"
        ))
        .is_ok());
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
            r"{item: {url: 'https://example.test/{{obj.path}}?q={{query}}', method: POST, headers: {X-List: '{{header}} \{{literal}} {{ next }}', X-Auth: {secret: token, prefix: 'Scheme '}}, body: {'{{literalKey}}': ['é{{body.field}}', '\{{literal}}', 3, null]}}}"
        ),
        ["obj.path", "query", "header", "next", "body.field"]
    );
    assert_eq!(
        refs("{item: 'https://example.test/{{first}}/{{first}}'}"),
        ["first", "first"]
    );
    assert!(
        refs(r"{item: {url: 'https://example.test/', method: POST, body: '\{{literal}}'}}")
            .is_empty()
    );
    refusal(
        "{item: {url: 'https://example.test/', method: POST, body: '{{name}}'}}",
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
            "{item: {url: 'https://example.test/', method: POST, body: {value: {{name}}}}}",
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

/// The string form keeps its https rule. A request map may name `http://`, and the
/// network policy decides on it. Control: the same text as a request map parses.
#[test]
fn http_is_accepted_in_the_request_map_form_only() {
    let error = refusal("{item: 'http://example.test/path'}", Some("url"));
    assert!(
        error
            .message
            .ends_with(": expected the literal https scheme"),
        "{error:?}"
    );
    assert_eq!(
        template("{item: {url: 'http://127.0.0.1:8080/{{name}}'}}").url,
        "http://127.0.0.1:8080/{{name}}"
    );
    for value in ["ftp://example.test/path", "HTTP://example.test/path"] {
        refusal(&format!("{{item: {{url: '{value}'}}}}"), Some("url"));
    }
}

/// `envelope` is a boolean that defaults to false, and `accept` defaults to `json`.
#[test]
fn envelope_is_a_boolean_and_accept_defaults_to_json() {
    for raw in [
        format!("{{item: '{URL}'}}"),
        format!("{{item: {{url: '{URL}'}}}}"),
    ] {
        let parsed = template(&raw);
        assert_eq!((parsed.accept, parsed.envelope), (Accept::Json, false));
    }
    let parsed = template(&format!(
        "{{item: {{url: '{URL}', accept: text, envelope: true}}}}"
    ));
    assert_eq!((parsed.accept, parsed.envelope), (Accept::Text, true));
    for value in ["'true'", "1", "null", "yes"] {
        refusal(
            &format!("{{item: {{url: '{URL}', envelope: {value}}}}}"),
            Some("envelope"),
        );
    }
    refusal("{item: {envelope: true}}", Some("url"));
}

/// Control: POST keeps its body.
#[test]
fn get_and_head_requests_cannot_have_a_body() {
    for method in ["", "method: GET, ", "method: HEAD, envelope: true, "] {
        let error = refusal(
            &format!("{{item: {{url: '{URL}', {method}body: {{text: abc}}}}}}"),
            Some("body"),
        );
        assert!(error.message.ends_with("cannot have a body"), "{error:?}");
    }
    let parsed = template(&format!(
        "{{item: {{url: '{URL}', method: POST, body: {{text: abc}}}}}}"
    ));
    assert_eq!(
        (parsed.method, parsed.has_body),
        (DeclaredMethod::Post, true)
    );
}

/// A HEAD response has data only in the envelope. Control: HEAD with the envelope.
#[test]
fn head_requires_the_envelope() {
    for raw in [
        format!("{{item: {{url: '{URL}', method: HEAD}}}}"),
        format!("{{item: {{url: '{URL}', method: HEAD, envelope: false}}}}"),
    ] {
        refusal(&raw, Some("envelope"));
    }
    let parsed = template(&format!(
        "{{item: {{url: '{URL}', method: HEAD, envelope: true}}}}"
    ));
    assert_eq!(
        (parsed.method, parsed.envelope),
        (DeclaredMethod::Head, true)
    );
}

/// Credential headers take the secret form only, in any letter case and also when the
/// text holds a reference. The message does not show the value. Controls: the secret
/// form of each name, and a literal value for another header name.
#[test]
fn credential_headers_take_the_secret_form_only() {
    for name in [
        "Authorization",
        "authorization",
        "PROXY-AUTHORIZATION",
        "Cookie",
        "X-Api-Key",
        "x-API-key",
    ] {
        for value in ["'Bearer value_marker'", "'Bearer {{value_marker}}'"] {
            let raw = format!("{{item: {{url: '{URL}', headers: {{{name}: {value}}}}}}}");
            let error = refusal(&raw, Some(name));
            assert!(
                error
                    .message
                    .ends_with(": use the secret form for this header"),
                "{error:?}"
            );
            assert!(!error.message.contains("value_marker"), "{error:?}");
        }
        let secret = format!(
            "{{item: {{url: '{URL}', headers: {{{name}: {{secret: token, prefix: 'Bearer '}}}}}}}}"
        );
        assert_eq!(
            template(&secret).headers[0].value,
            HeaderSource::Secret {
                name: "token".into(),
                prefix: "Bearer ".into()
            }
        );
    }
    let parsed = template(&format!(
        "{{item: {{url: '{URL}', headers: {{X-Trace: 'Bearer {{{{id}}}}'}}}}}}"
    ));
    assert_eq!(
        parsed.headers,
        [HeaderTemplate {
            name: "X-Trace".into(),
            value: HeaderSource::Text("Bearer {{id}}".into())
        }]
    );
}

/// The prefix of a secret header is header text. Control: a visible ASCII prefix.
#[test]
fn secret_prefix_is_header_text() {
    let header = |prefix: &str| {
        format!("{{item: {{url: '{URL}', headers: {{X-Auth: {{secret: token, prefix: '{prefix}'}}}}}}}}")
    };
    refusal(&header("Beärer "), Some("X-Auth"));
    assert!(parse(&header("Bearer ")).is_ok());
}

/// The literal URL goes through the strict URL parser, each reference as the shortest
/// text that it can expand to, in both forms. An IPv4 short form, an upper-case host or
/// a bad port fails before any request exists. Controls: canonical forms with a port.
#[test]
fn literal_url_goes_through_the_strict_parser() {
    for url in [
        "https://127.1/{{name}}",
        "https://2130706433/path",
        "https://0x7f000001/path",
        "https://127.0.0.01/path",
        "https://Example.test/path",
        "https://example.test:99999/path",
        "https://example.test:0/path",
        "https://[::1/path",
        "https://[fe80::1%25en0]/path",
        "https://example.test/pa%th",
        "https://example.test/é",
    ] {
        refusal(&format!("{{item: '{url}'}}"), Some("url"));
        refusal(&format!("{{item: {{url: '{url}'}}}}"), Some("url"));
    }
    refusal("{item: {url: 'http://127.1:8080/'}}", Some("url"));
    for url in [
        "https://127.0.0.1:8080/{{name}}?q={{query}}",
        "https://[::1]:8080/path",
        "https://example.test",
    ] {
        assert!(parse(&format!("{{item: '{url}'}}")).is_ok(), "{url}");
    }
    assert!(parse("{item: {url: 'http://127.0.0.1:8080/'}}").is_ok());
}

/// A query reference can expand to empty text, so it counts no byte toward the URL cap
/// at declaration; a path reference counts one. Past the cap is a resource limit.
#[test]
fn literal_url_at_the_length_cap_passes_and_one_byte_over_fails() {
    let base = "https://example.test/";
    let url = |extra: usize| {
        let fill = crate::limits::MAX_URL_BYTES - base.len() - "x?q=".len() + extra;
        format!("{base}{{{{a}}}}{}?q={{{{b}}}}", "p".repeat(fill))
    };
    assert!(parse(&format!("{{item: '{}'}}", url(0))).is_ok());
    let error = parse(&format!("{{item: '{}'}}", url(1)))
        .unwrap_err()
        .serialize();
    assert_eq!(error.code, "mds::resource_limit", "{error:?}");
    assert!(
        error.message.contains("entry \"item\", field \"url\"")
            && error.message.contains("8192 bytes"),
        "{error:?}"
    );
}

/// Declaration analysis counts the header map before it keeps a header: 32 headers pass,
/// and one more is a resource limit that names the entry.
#[test]
fn header_count_cap_admits_exactly_the_cap() {
    let entry = |count: usize| {
        let headers: Vec<String> = (0..count).map(|i| format!("x-h{i}: v")).collect();
        format!(
            "{{item: {{url: '{URL}', headers: {{{}}}}}}}",
            headers.join(", ")
        )
    };
    let cap = crate::limits::MAX_REQUEST_HEADERS;
    assert_eq!(template(&entry(cap)).headers.len(), cap);
    let error = parse(&entry(cap + 1)).unwrap_err().serialize();
    assert_eq!(error.code, "mds::resource_limit", "{error:?}");
    assert_eq!(
        error.message,
        format!(
            "resource limit exceeded: \"sub/main.mds\", entry \"item\", field \"headers\": \
             too many headers (max {cap} per request)"
        )
    );
}
