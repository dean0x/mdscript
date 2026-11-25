//! The status envelope of a URL entry: the status, the headers and the body of one
//! response as one value, `{status, headers, body}`.
//!
//! A header name becomes lower case, with each `-` changed to `_`, so that a reference
//! can read it with dot access. A name that is then not an identifier is dropped, and so
//! are `set-cookie` and `set-cookie2`. Two names that differ but give the same key fail.
//! A repeated header joins its values with `, `. Each value passes the character rule of
//! data strings. A message names the entry and the field `headers`, never a header name
//! or value, because both are server text.

use std::collections::HashMap;
use std::mem::size_of;

use super::DataLabel;
use crate::escape_class::is_control_char;
use crate::evaluator::budget::{value_bytes, MemBudget, Part};
use crate::parser::is_valid_identifier;
use crate::value::Value;
use crate::MdsError;

/// The response headers that the envelope never holds: they set client state.
const DROPPED: [&str; 2] = ["set-cookie", "set-cookie2"];

/// The envelope of a response with `status`, `headers` in received order and the
/// converted `body` (`Value::Null` for a HEAD request). The caller has checked the
/// header byte cap. The header values are charged as `Buffers` once the value exists.
pub(crate) fn envelope(
    status: u16,
    headers: &[(String, Vec<u8>)],
    body: Value,
    label: DataLabel<'_>,
    mem: &mut MemBudget,
) -> Result<Value, MdsError> {
    let text_bytes = headers.iter().try_fold(0usize, |sum, (name, value)| {
        sum.checked_add(name.len())?.checked_add(value.len())
    });
    // The lower-case name, the key and the joined value of each header, each at twice
    // its length for buffer growth, and the table slots.
    let slots = headers
        .len()
        .checked_mul(2 * size_of::<(String, (String, String))>());
    mem.charge(
        Part::Temp,
        text_bytes
            .and_then(|bytes| bytes.checked_mul(6))
            .and_then(|bytes| bytes.checked_add(slots?)),
    )?;
    let mut fields: HashMap<String, (String, String)> = HashMap::with_capacity(headers.len());
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if DROPPED.contains(&lower.as_str()) {
            continue;
        }
        let key = lower.replace('-', "_");
        if !is_valid_identifier(&key) {
            continue;
        }
        let text = header_text(value, label)?;
        match fields.get_mut(&key) {
            Some((first, joined)) if *first == lower => {
                joined.push_str(", ");
                joined.push_str(text);
            }
            Some(_) => {
                return Err(header_error(
                    label,
                    "two response header names give the same envelope key",
                ))
            }
            None => {
                fields.insert(key, (lower, text.to_owned()));
            }
        }
    }
    let headers = Value::Object(
        fields
            .into_iter()
            .map(|(key, (_, value))| (key, Value::String(value)))
            .collect(),
    );
    // The body charged its own bytes when it was converted. The outer table has three
    // fields, counted at twice their size.
    mem.charge(
        Part::Buffers,
        value_bytes(&headers).and_then(|bytes| bytes.checked_add(6 * size_of::<(String, Value)>())),
    )?;
    Ok(Value::Object(HashMap::from([
        ("status".to_owned(), Value::Number(f64::from(status))),
        ("headers".to_owned(), headers),
        ("body".to_owned(), body),
    ])))
}

/// The text of one header value: UTF-8 with no character of the escape class of data
/// strings.
fn header_text<'v>(value: &'v [u8], label: DataLabel<'_>) -> Result<&'v str, MdsError> {
    let text = std::str::from_utf8(value)
        .map_err(|_| header_error(label, "invalid UTF-8 in a response header value"))?;
    if text.chars().any(is_control_char) {
        return Err(header_error(
            label,
            "forbidden character in a response header value",
        ));
    }
    Ok(text)
}

/// A data error of the field `headers`, a fixed name.
fn header_error(label: DataLabel<'_>, reason: &str) -> MdsError {
    MdsError::data(
        label.module,
        Some(label.entry),
        Some("headers"),
        reason,
        None,
    )
}
