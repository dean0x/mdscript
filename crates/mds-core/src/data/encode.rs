//! Placement encoders and value-to-text of network data requests.
//!
//! [`expand`] is the one path from a template and its reference values to request text,
//! for every value source. The place of each reference comes from the literal template
//! text, never from the expanded text, and each value is inserted once and never scanned
//! again.

use std::borrow::Cow;

use super::refs::{pieces, Piece};
use crate::error::{data_location, MdsError};
use crate::limits::{MAX_HEADER_VALUE_BYTES, MAX_URL_BYTES};
use crate::value::Value;

/// The largest integer up to which an `f64` holds every integer exactly: 2^53.
const MAX_EXACT_INTEGER: f64 = 9_007_199_254_740_992.0;

/// Where a reference value goes in a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// One URL path segment, before the first literal `?`.
    PathSegment,
    /// A query key or value, after the first literal `?`.
    Query,
    HeaderValue,
}

impl Placement {
    fn noun(self) -> &'static str {
        match self {
            Placement::PathSegment => "a URL path segment",
            Placement::Query => "a URL query",
            Placement::HeaderValue => "a header value",
        }
    }
}

/// The request text that a template fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Url,
    Header,
}

/// The declaration site of a template, for messages.
#[derive(Clone, Copy)]
pub(crate) struct FieldLabel<'a> {
    /// Project-relative label of the declaring module.
    pub(crate) module: &'a str,
    pub(crate) entry: &'a str,
    /// The request field: `url` or a header name.
    pub(crate) field: &'a str,
}

/// Why an encoder refused a value. The reason is fixed text and never holds the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EncodeRefusal {
    EmptySegment,
    DotSegment,
    HeaderByte,
}

impl EncodeRefusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            EncodeRefusal::EmptySegment => {
                "a reference cannot fill a URL path segment with empty text"
            }
            EncodeRefusal::DotSegment => {
                "a reference cannot fill a URL path segment with `.` or `..`"
            }
            EncodeRefusal::HeaderByte => {
                "a header value can contain only visible ASCII characters and spaces"
            }
        }
    }
}

/// The text of a reference value: a string as it is, `true` or `false`, or an integer
/// with no fraction and an absolute value of at most 2^53. Every other value fails with
/// `mds::type_error`.
#[must_use = "a value can have a type that the request does not take"]
pub(crate) fn scalar_text<'v>(
    value: &'v Value,
    placement: Placement,
    label: FieldLabel<'_>,
) -> Result<Cow<'v, str>, MdsError> {
    let got = match value {
        Value::String(text) => return Ok(Cow::Borrowed(text)),
        Value::Boolean(true) => return Ok(Cow::Borrowed("true")),
        Value::Boolean(false) => return Ok(Cow::Borrowed("false")),
        Value::Number(number) if is_exact_integer(*number) => {
            // An exact integer of at most 2^53 converts to `i64` with no loss.
            return Ok(Cow::Owned((*number as i64).to_string()));
        }
        Value::Number(_) => "a number that is not such an integer",
        Value::Null => "null",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    };
    Err(MdsError::data_type_error(
        label.module,
        label.entry,
        label.field,
        &format!(
            "{} takes a string, a boolean or an exact integer up to 2^53, not {got}",
            placement.noun()
        ),
    ))
}

fn is_exact_integer(number: f64) -> bool {
    number.is_finite() && number.fract() == 0.0 && number.abs() <= MAX_EXACT_INTEGER
}

/// Encode a value for one URL path segment. Every byte outside the RFC 3986 unreserved
/// set is percent-encoded, so a value can not add a segment. Empty text, `.` and `..`
/// are refused.
#[must_use = "a path segment value can be refused"]
pub(crate) fn encode_path_segment(text: &str) -> Result<String, EncodeRefusal> {
    match text {
        "" => Err(EncodeRefusal::EmptySegment),
        "." | ".." => Err(EncodeRefusal::DotSegment),
        _ => Ok(percent_encode(text)),
    }
}

/// Encode a value for a query key or value: the byte rule of [`encode_path_segment`], so
/// `&`, `=`, `+` and `#` can not add or cut parameters.
#[must_use]
pub(crate) fn encode_query(text: &str) -> String {
    percent_encode(text)
}

/// Check header text: bytes 0x20 to 0x7E only. Other text is refused, never encoded.
#[must_use = "header text can be refused"]
pub(crate) fn check_header_value(text: &str) -> Result<(), EncodeRefusal> {
    if text.bytes().all(|byte| matches!(byte, 0x20..=0x7e)) {
        Ok(())
    } else {
        Err(EncodeRefusal::HeaderByte)
    }
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0f));
        }
    }
    out
}

/// The upper-case hex digit of a value from 0 to 15.
fn hex_digit(nibble: u8) -> char {
    char::from(if nibble < 10 {
        b'0' + nibble
    } else {
        b'A' + (nibble - 10)
    })
}

/// Expand `template` for `target` in one pass over its original text. `lookup` gives the
/// value of each reference path, whatever its source. A URL that would grow past
/// `MAX_URL_BYTES`, or a header value past `MAX_HEADER_VALUE_BYTES`, fails with
/// `mds::resource_limit` before the append.
#[must_use = "expansion can fail"]
pub(crate) fn expand<'v>(
    template: &str,
    target: Target,
    label: FieldLabel<'_>,
    mut lookup: impl FnMut(&str) -> Result<&'v Value, MdsError>,
) -> Result<String, MdsError> {
    let mut writer = Writer::new(target);
    for piece in pieces(template) {
        let piece = piece.map_err(|reason| label.data_error(reason))?;
        let written = match piece {
            Piece::Literal(text) => writer.literal(text),
            Piece::Reference(path) => {
                let text = scalar_text(lookup(path)?, writer.placement, label)?;
                writer.value(&text)
            }
        };
        written.map_err(|fault| label.fault(fault))?;
    }
    Ok(writer.finish())
}

impl FieldLabel<'_> {
    fn data_error(self, reason: &str) -> MdsError {
        MdsError::data(
            self.module,
            Some(self.entry),
            Some(self.field),
            reason,
            None,
        )
    }

    fn fault(self, fault: Fault) -> MdsError {
        match fault {
            Fault::Refused(refusal) => self.data_error(refusal.reason()),
            Fault::UrlTooLong => self.limit("URL", MAX_URL_BYTES),
            Fault::HeaderTooLong => self.limit("header value", MAX_HEADER_VALUE_BYTES),
        }
    }

    pub(crate) fn limit(self, what: &str, max: usize) -> MdsError {
        MdsError::resource_limit(format!(
            "{}: {what} exceeds the maximum length of {max} bytes",
            data_location(self.module, Some(self.entry), Some(self.field))
        ))
    }
}

enum Fault {
    Refused(EncodeRefusal),
    UrlTooLong,
    HeaderTooLong,
}

impl From<EncodeRefusal> for Fault {
    fn from(refusal: EncodeRefusal) -> Self {
        Fault::Refused(refusal)
    }
}

struct Writer {
    out: String,
    /// The placement of the next reference.
    placement: Placement,
}

impl Writer {
    fn new(target: Target) -> Self {
        Self {
            out: String::new(),
            placement: match target {
                Target::Url => Placement::PathSegment,
                Target::Header => Placement::HeaderValue,
            },
        }
    }

    /// Write literal template text. `\{{` writes `{{`. In a URL, the first literal `?`
    /// starts the query.
    fn literal(&mut self, text: &str) -> Result<(), Fault> {
        let text = if text.contains("\\{{") {
            Cow::Owned(text.replace("\\{{", "{{"))
        } else {
            Cow::Borrowed(text)
        };
        match self.placement {
            Placement::HeaderValue => check_header_value(&text)?,
            Placement::PathSegment if text.contains('?') => self.placement = Placement::Query,
            Placement::PathSegment | Placement::Query => {}
        }
        self.push(&text)
    }

    /// Write the text of one reference value with the encoder of its placement.
    fn value(&mut self, text: &str) -> Result<(), Fault> {
        match self.placement {
            Placement::PathSegment => {
                // Encoding never makes text shorter: check the raw length before the work.
                self.room_for(text.len())?;
                self.push(&encode_path_segment(text)?)
            }
            Placement::Query => {
                self.room_for(text.len())?;
                self.push(&encode_query(text))
            }
            Placement::HeaderValue => {
                check_header_value(text)?;
                self.push(text)
            }
        }
    }

    fn finish(self) -> String {
        self.out
    }

    fn push(&mut self, text: &str) -> Result<(), Fault> {
        self.room_for(text.len())?;
        self.out.push_str(text);
        Ok(())
    }

    /// A URL holds at most `MAX_URL_BYTES`, and a header value `MAX_HEADER_VALUE_BYTES`.
    fn room_for(&self, bytes: usize) -> Result<(), Fault> {
        let (max, fault) = match self.placement {
            Placement::HeaderValue => (MAX_HEADER_VALUE_BYTES, Fault::HeaderTooLong),
            Placement::PathSegment | Placement::Query => (MAX_URL_BYTES, Fault::UrlTooLong),
        };
        if self.out.len().saturating_add(bytes) > max {
            Err(fault)
        } else {
            Ok(())
        }
    }
}
