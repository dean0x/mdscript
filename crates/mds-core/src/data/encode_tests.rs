use std::cell::Cell;
use std::collections::HashMap;

use super::encode::{
    check_header_value, encode_path_segment, encode_query, expand, scalar_text, EncodeRefusal,
    FieldLabel, Placement, Target,
};
use crate::error::MdsError;
use crate::limits::{MAX_HEADER_VALUE_BYTES, MAX_URL_BYTES};
use crate::value::Value;

const LABEL: FieldLabel<'static> = FieldLabel {
    module: "entry.mds",
    entry: "users",
    field: "url",
};

const EXACT: f64 = 9_007_199_254_740_992.0;

/// A value, then the exact bytes it gives in a path segment, a query part and a header
/// value, or the refusal. `None` for a header means the header refusal.
pub(super) type Vector = (
    String,
    Result<&'static str, EncodeRefusal>,
    &'static str,
    Option<&'static str>,
);

/// One value from each row of the shared vector table. `abc` is the control.
pub(super) fn vectors() -> Vec<Vector> {
    let refuse_header = None;
    vec![
        ("abc".into(), Ok("abc"), "abc", Some("abc")),
        ("a/b".into(), Ok("a%2Fb"), "a%2Fb", Some("a/b")),
        (
            "..".into(),
            Err(EncodeRefusal::DotSegment),
            "..",
            Some(".."),
        ),
        (".".into(), Err(EncodeRefusal::DotSegment), ".", Some(".")),
        (
            String::new(),
            Err(EncodeRefusal::EmptySegment),
            "",
            Some(""),
        ),
        ("x?y".into(), Ok("x%3Fy"), "x%3Fy", Some("x?y")),
        ("x#y".into(), Ok("x%23y"), "x%23y", Some("x#y")),
        ("a&b=c".into(), Ok("a%26b%3Dc"), "a%26b%3Dc", Some("a&b=c")),
        ("%2F".into(), Ok("%252F"), "%252F", Some("%2F")),
        (char::from(92).to_string(), Ok("%5C"), "%5C", Some("\\")),
        ("\r\n".into(), Ok("%0D%0A"), "%0D%0A", refuse_header),
        (
            ['\u{10d}', '\u{10a}'].iter().collect(),
            Ok("%C4%8D%C4%8A"),
            "%C4%8D%C4%8A",
            refuse_header,
        ),
    ]
}

fn one(name: &str, value: &str) -> HashMap<String, Value> {
    HashMap::from([(name.to_owned(), Value::String(value.to_owned()))])
}

fn expand_with(
    template: &str,
    target: Target,
    values: &HashMap<String, Value>,
) -> Result<String, MdsError> {
    expand(template, target, LABEL, |path| {
        values
            .get(path)
            .ok_or_else(|| MdsError::undefined_var(path))
    })
}

fn data_reason(result: Result<String, MdsError>) -> String {
    match result {
        Err(err @ MdsError::Data { .. }) => err.to_string(),
        other => panic!("expected mds::data, got {other:?}"),
    }
}

#[test]
fn each_placement_encoder_gives_the_exact_bytes_or_its_refusal() {
    for (value, path, query, header) in vectors() {
        assert_eq!(
            encode_path_segment(&value).as_deref(),
            path.as_deref(),
            "{value:?}"
        );
        assert_eq!(encode_query(&value), query, "{value:?}");
        match header {
            Some(_) => assert_eq!(check_header_value(&value), Ok(()), "{value:?}"),
            None => assert_eq!(
                check_header_value(&value),
                Err(EncodeRefusal::HeaderByte),
                "{value:?}"
            ),
        }
    }
}

#[test]
fn every_vector_goes_through_expansion_into_path_query_and_header() {
    for (value, path, query, header) in vectors() {
        let values = one("v", &value);
        let url = expand_with("https://a.example/items/{{v}}", Target::Url, &values);
        match path {
            Ok(bytes) => assert_eq!(url.unwrap(), format!("https://a.example/items/{bytes}")),
            Err(refusal) => assert!(data_reason(url).ends_with(refusal.reason()), "{value:?}"),
        }
        let url = expand_with("https://a.example/items?q={{v}}&n=1", Target::Url, &values);
        assert_eq!(
            url.unwrap(),
            format!("https://a.example/items?q={query}&n=1"),
            "{value:?}"
        );
        let header_text = expand_with("Bearer {{v}}", Target::Header, &values);
        match header {
            Some(bytes) => assert_eq!(header_text.unwrap(), format!("Bearer {bytes}")),
            None => assert!(
                data_reason(header_text).ends_with(EncodeRefusal::HeaderByte.reason()),
                "{value:?}"
            ),
        }
    }
}

#[test]
fn an_encoder_refusal_names_the_module_entry_and_field() {
    let message = data_reason(expand_with(
        "https://a.example/{{v}}",
        Target::Url,
        &one("v", ".."),
    ));
    assert!(
        message.starts_with(r#"data error in "entry.mds", entry "users", field "url": "#),
        "{message}"
    );
}

/// The placement comes from the template text: a `?` in a value does not move the next
/// reference into the query.
#[test]
fn placement_comes_from_the_literal_template_never_from_a_value() {
    let mut values = one("a", "x?y");
    values.insert("b".into(), Value::String("..".into()));
    let refused = data_reason(expand_with(
        "https://a.example/{{a}}/{{b}}",
        Target::Url,
        &values,
    ));
    assert!(
        refused.ends_with(EncodeRefusal::DotSegment.reason()),
        "{refused}"
    );

    values.insert("b".into(), Value::String("abc".into()));
    assert_eq!(
        expand_with("https://a.example/{{a}}/{{b}}", Target::Url, &values).unwrap(),
        "https://a.example/x%3Fy/abc"
    );
    // After the first literal `?`, a dot value is query text.
    values.insert("b".into(), Value::String("..".into()));
    assert_eq!(
        expand_with("https://a.example/{{a}}?k={{b}}", Target::Url, &values).unwrap(),
        "https://a.example/x%3Fy?k=.."
    );
}

/// Inserted text is never scanned for references: one lookup per reference in the template.
#[test]
fn each_value_is_inserted_once_and_never_scanned_again() {
    let values = HashMap::from([
        ("a".to_owned(), Value::String("{{b}}".into())),
        ("b".to_owned(), Value::String("other".into())),
    ]);
    for (template, target, expected) in [
        (
            "https://a.example/{{a}}",
            Target::Url,
            "https://a.example/%7B%7Bb%7D%7D",
        ),
        ("x {{a}}", Target::Header, "x {{b}}"),
    ] {
        let lookups = Cell::new(0);
        let result = expand(template, target, LABEL, |path| {
            lookups.set(lookups.get() + 1);
            Ok(&values[path])
        });
        assert_eq!(result.unwrap(), expected);
        assert_eq!(lookups.get(), 1, "{template}");
    }
}

#[test]
fn an_escaped_reference_writes_literal_braces_and_needs_no_lookup() {
    let lookups = Cell::new(0);
    let result = expand("x \\{{a}} y", Target::Header, LABEL, |_| {
        lookups.set(lookups.get() + 1);
        Err(MdsError::internal())
    });
    assert_eq!(result.unwrap(), "x {{a}} y");
    assert_eq!(lookups.get(), 0);
}

#[test]
fn literal_header_text_follows_the_header_byte_rule() {
    let refused = data_reason(expand_with("a\r\n{{v}}", Target::Header, &one("v", "b")));
    assert!(
        refused.ends_with(EncodeRefusal::HeaderByte.reason()),
        "{refused}"
    );
    assert_eq!(
        expand_with("a {{v}}", Target::Header, &one("v", "b")).unwrap(),
        "a b"
    );
}

#[test]
fn the_url_cap_counts_encoded_bytes_and_admits_exactly_the_cap() {
    let prefix = "https://a.example/";
    let fits = "a".repeat(MAX_URL_BYTES - prefix.len());
    let template = format!("{prefix}{{{{v}}}}");
    let url = expand_with(&template, Target::Url, &one("v", &fits)).unwrap();
    assert_eq!(url.len(), MAX_URL_BYTES);

    let over = format!("{fits}a");
    for value in [over, "/".repeat((MAX_URL_BYTES - prefix.len()) / 3 + 1)] {
        match expand_with(&template, Target::Url, &one("v", &value)) {
            Err(err @ MdsError::ResourceLimit { .. }) => {
                let message = err.to_string();
                assert!(message.contains(r#""entry.mds", entry "users", field "url""#));
                assert!(message.contains(&MAX_URL_BYTES.to_string()), "{message}");
            }
            other => panic!("expected mds::resource_limit, got {other:?}"),
        }
    }
    let slashes = "/".repeat((MAX_URL_BYTES - prefix.len()) / 3);
    assert!(expand_with(&template, Target::Url, &one("v", &slashes)).is_ok());
}

#[test]
fn reference_and_lookup_errors_stop_expansion() {
    let unclosed = data_reason(expand_with(
        "https://a.example/{{v",
        Target::Url,
        &one("v", "a"),
    ));
    assert!(unclosed.ends_with("unclosed `{{` reference"), "{unclosed}");
    let missing = expand_with("https://a.example/{{w}}", Target::Url, &one("v", "a"));
    assert!(
        matches!(missing, Err(MdsError::UndefinedVariable { .. })),
        "{missing:?}"
    );
}

#[test]
fn scalars_become_text_only_as_strings_booleans_and_exact_integers() {
    for (value, text) in [
        (Value::String("a b".into()), "a b"),
        (Value::Boolean(true), "true"),
        (Value::Boolean(false), "false"),
        (Value::Number(EXACT), "9007199254740992"),
        (Value::Number(-EXACT), "-9007199254740992"),
        (Value::Number(3.0), "3"),
        (Value::Number(0.0), "0"),
    ] {
        for placement in [
            Placement::PathSegment,
            Placement::Query,
            Placement::HeaderValue,
        ] {
            assert_eq!(scalar_text(&value, placement, LABEL).unwrap(), text);
        }
    }
}

#[test]
fn other_numbers_null_arrays_and_objects_fail_with_the_type_error_code() {
    for value in [
        Value::Number(EXACT + 2.0),
        Value::Number(-(EXACT + 2.0)),
        Value::Number(1.5),
        Value::Number(f64::INFINITY),
        Value::Number(f64::NAN),
        Value::Null,
        Value::Array(vec![Value::String("a".into())]),
        Value::Object(HashMap::new()),
    ] {
        for placement in [
            Placement::PathSegment,
            Placement::Query,
            Placement::HeaderValue,
        ] {
            let err = scalar_text(&value, placement, LABEL).unwrap_err();
            assert_eq!(err.serialize().code, "mds::type_error", "{value:?}");
            let message = err.to_string();
            assert!(
                message.starts_with(r#"type error in "entry.mds", entry "users", field "url": "#),
                "{message}"
            );
        }
    }
    let through_expand = expand("https://a.example/{{v}}", Target::Url, LABEL, |_| {
        Ok(&Value::Null)
    });
    assert_eq!(
        through_expand.unwrap_err().serialize().code,
        "mds::type_error"
    );
}

/// A header value holds at most 8 KiB: the cap counts literal and inserted text, admits
/// exactly the cap and refuses one more byte before the append.
#[test]
fn the_header_value_cap_admits_exactly_the_cap() {
    let label = FieldLabel {
        field: "x-trace",
        ..LABEL
    };
    let header = |value: &str| {
        let values = one("v", value);
        expand("Bearer {{v}}", Target::Header, label, |path| {
            Ok(&values[path])
        })
    };
    let fits = "a".repeat(MAX_HEADER_VALUE_BYTES - "Bearer ".len());
    assert_eq!(header(&fits).unwrap().len(), MAX_HEADER_VALUE_BYTES);
    match header(&format!("{fits}a")) {
        Err(err @ MdsError::ResourceLimit { .. }) => assert_eq!(
            err.to_string(),
            format!(
                "resource limit exceeded: \"entry.mds\", entry \"users\", field \"x-trace\": \
                 header value exceeds the maximum length of {MAX_HEADER_VALUE_BYTES} bytes"
            )
        ),
        other => panic!("expected mds::resource_limit, got {other:?}"),
    }
    let literal = format!("{}{{{{v}}}}", "a".repeat(MAX_HEADER_VALUE_BYTES + 1));
    let values = one("v", "");
    let refused = expand(&literal, Target::Header, label, |path| Ok(&values[path]));
    assert!(
        matches!(refused, Err(MdsError::ResourceLimit { .. })),
        "{refused:?}"
    );
}
