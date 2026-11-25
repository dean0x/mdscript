//! The header rules of the status envelope.

use std::collections::HashMap;

use super::envelope::envelope;
use super::{DataInput, DataLabel};
use crate::evaluator::budget::MemBudget;
use crate::value::MAX_VALUE_DEPTH;
use crate::{MdsError, Value};

const LABEL: DataLabel<'static> = DataLabel {
    module: "main.mds",
    entry: "a",
    input: DataInput::Response,
};

fn build(status: u16, headers: &[(&str, &[u8])], body: Value) -> Result<Value, MdsError> {
    let headers: Vec<(String, Vec<u8>)> = headers
        .iter()
        .map(|&(name, value)| (name.to_owned(), value.to_vec()))
        .collect();
    envelope(status, &headers, body, LABEL, &mut MemBudget::default())
}

fn headers(raw: &[(&str, &[u8])]) -> Result<HashMap<String, Value>, MdsError> {
    let value = build(200, raw, Value::Null)?;
    let Value::Object(mut fields) = value else {
        panic!("expected an object");
    };
    match fields.remove("headers") {
        Some(Value::Object(headers)) => Ok(headers),
        other => panic!("expected a header object, got {other:?}"),
    }
}

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

/// The envelope holds the status as a number, the headers and the body as given.
#[test]
fn the_envelope_holds_the_status_the_headers_and_the_body() {
    let value = build(404, &[("ETag", b"\"v1\"")], text("gone")).unwrap();
    let expected = Value::Object(HashMap::from([
        ("status".to_owned(), Value::Number(404.0)),
        (
            "headers".to_owned(),
            Value::Object(HashMap::from([("etag".to_owned(), text("\"v1\""))])),
        ),
        ("body".to_owned(), text("gone")),
    ]));
    assert_eq!(value, expected);
}

/// The envelope puts the body one level deeper. Thus a body with a value at the depth
/// cap fails with a data error of the field `body`. Control: a body one level less deep
/// builds.
#[test]
fn a_body_at_the_depth_cap_fails_with_a_data_error() {
    let nested =
        |depth: usize| (0..depth).fold(Value::Number(0.0), |inner, _| Value::Array(vec![inner]));
    let err = build(200, &[], nested(MAX_VALUE_DEPTH)).unwrap_err();
    assert_eq!(err.serialize().code, "mds::data");
    assert_eq!(
        err.to_string(),
        r#"data error in "main.mds", entry "a", field "body": value nesting exceeds maximum depth of 64"#
    );
    assert!(build(200, &[], nested(MAX_VALUE_DEPTH - 1)).is_ok());
}

/// A header name becomes lower case with `-` changed to `_`. Control: a name that is
/// already a lower-case identifier stays the same.
#[test]
fn header_names_become_lower_case_with_underscores() {
    let fields = headers(&[
        ("X-Rate-Limit", b"10"),
        ("Content-Type", b"application/json"),
        ("etag", b"x"),
    ])
    .unwrap();
    assert_eq!(
        fields,
        HashMap::from([
            ("x_rate_limit".to_owned(), text("10")),
            ("content_type".to_owned(), text("application/json")),
            ("etag".to_owned(), text("x")),
        ])
    );
}

/// A name that is not an identifier after the change is dropped. Control: an
/// identifier name in the same response stays.
#[test]
fn a_name_that_is_not_an_identifier_is_dropped() {
    let fields = headers(&[
        ("x.dotted", b"1"),
        ("1st", b"1"),
        ("caf\u{e9}", b"1"),
        ("x-kept", b"1"),
    ])
    .unwrap();
    assert_eq!(fields, HashMap::from([("x_kept".to_owned(), text("1"))]));
}

/// `set-cookie` and `set-cookie2` are dropped in any letter case. Control: a longer name
/// that starts with `set-cookie` stays.
#[test]
fn cookie_headers_are_dropped() {
    let fields = headers(&[
        ("Set-Cookie", b"id=1"),
        ("SET-COOKIE2", b"id=2"),
        ("set-cookie-policy", b"strict"),
    ])
    .unwrap();
    assert_eq!(
        fields,
        HashMap::from([("set_cookie_policy".to_owned(), text("strict"))])
    );
}

/// A repeated header joins its values with `, `, in received order, in any letter case.
#[test]
fn a_repeated_header_joins_its_values_in_received_order() {
    let fields = headers(&[("Vary", b"accept"), ("vary", b"origin"), ("VARY", b"x")]).unwrap();
    assert_eq!(
        fields,
        HashMap::from([("vary".to_owned(), text("accept, origin, x"))])
    );
}

/// Two different names that give one key fail, and the message names neither name nor
/// value. Control: the same name twice joins.
#[test]
fn two_names_with_one_key_fail() {
    let err = headers(&[("x-a", b"one"), ("x_a", b"two")]).unwrap_err();
    assert_eq!(err.serialize().code, "mds::data");
    assert_eq!(
        err.to_string(),
        r#"data error in "main.mds", entry "a", field "headers": two response header names give the same envelope key"#
    );
    let fields = headers(&[("x-a", b"one"), ("X-A", b"two")]).unwrap();
    assert_eq!(
        fields,
        HashMap::from([("x_a".to_owned(), text("one, two"))])
    );
}

/// A header value passes the character rule of data strings: an escape-class character
/// or a byte that is not UTF-8 fails, and the message does not show the value. Control:
/// a tab passes.
#[test]
fn a_header_value_passes_the_character_rule() {
    for (value, reason) in [
        (
            &b"a\x1bb"[..],
            "forbidden character in a response header value",
        ),
        (
            "a\u{202e}b".as_bytes(),
            "forbidden character in a response header value",
        ),
        (
            &b"a\rb"[..],
            "forbidden character in a response header value",
        ),
        (&b"a\xffb"[..], "invalid UTF-8 in a response header value"),
    ] {
        let err = headers(&[("x-value", value)]).unwrap_err();
        assert_eq!(err.serialize().code, "mds::data");
        assert_eq!(
            err.to_string(),
            format!(r#"data error in "main.mds", entry "a", field "headers": {reason}"#)
        );
    }
    let fields = headers(&[("x-value", b"a\tb")]).unwrap();
    assert_eq!(
        fields,
        HashMap::from([("x_value".to_owned(), text("a\tb"))])
    );
}
