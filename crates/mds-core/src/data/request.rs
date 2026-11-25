//! The requests of network data entries and their identity.
//!
//! A request carries a descriptor of each secret header, never the secret value. The
//! request ID is the SHA-256 digest of the exact request. A secret header enters the
//! digest as its name, its prefix and the HMAC-SHA-256 of its value under the per-run
//! key, so two requests that differ in one header value have two IDs, and an ID permits
//! no offline guess of a short secret.

use std::error::Error;
use std::fmt;

use super::digest::{hex, hmac_sha256, sha256};
use super::policy::{NetworkPolicy, Refusal, SecretValue};
use super::url::CheckedUrl;
use super::Accept;

/// The per-run key of the keyed hash of secret header values. The host makes it from a
/// random source once per run. Its `Debug` shows no key.
///
/// This type is `#[non_exhaustive]`: obtain keys from [`RunKey::from_bytes`].
#[non_exhaustive]
pub struct RunKey([u8; 32]);

impl RunKey {
    /// A key of 32 bytes from a random source.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for RunKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RunKey(..)")
    }
}

/// The method of a request. This release sends GET and HEAD only.
///
/// This enum is `#[non_exhaustive]`: new methods may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Head,
}

impl Method {
    /// The method name: `GET` or `HEAD`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
        }
    }
}

/// The identity of a request: a SHA-256 digest of the exact request.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId([u8; 32]);

impl RequestId {
    /// The ID as 64 lower-case hex digits.
    #[must_use]
    pub fn as_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parse an ID from exactly 64 lower-case hex digits.
    #[must_use = "an ID can be refused"]
    pub fn parse_hex(text: &str) -> Result<Self, RequestIdError> {
        let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
            return Err(RequestIdError);
        };
        let mut id = [0u8; 32];
        if pairs.len() != id.len() {
            return Err(RequestIdError);
        }
        for (byte, &[high, low]) in id.iter_mut().zip(pairs) {
            *byte = (lower_hex_digit(high).ok_or(RequestIdError)? << 4)
                | lower_hex_digit(low).ok_or(RequestIdError)?;
        }
        Ok(Self(id))
    }
}

impl fmt::Debug for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RequestId({})", self.as_hex())
    }
}

/// Text that is not 64 lower-case hex digits.
///
/// This type is `#[non_exhaustive]` and cannot be built outside the core.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestIdError;

impl fmt::Display for RequestIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a request ID is 64 lower-case hex digits")
    }
}

impl Error for RequestIdError {}

/// The value of a request header.
///
/// This enum is `#[non_exhaustive]`: new value kinds may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderValue {
    /// The final header text.
    Text(String),
    /// A host secret: the secret name and the literal prefix, never the value. The
    /// sender writes the prefix and then the value of the bound secret.
    Secret { name: String, prefix: String },
}

/// One header of a request.
///
/// This type is `#[non_exhaustive]`: obtain headers from [`Request::headers`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHeader {
    name: String,
    value: HeaderValue,
}

impl RequestHeader {
    /// The header name as declared.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &HeaderValue {
        &self.value
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the step engine is the first caller")
)]
impl RequestHeader {
    pub(crate) fn new(name: String, value: HeaderValue) -> Self {
        Self { name, value }
    }
}

/// A request that the core needs and that passed every policy check.
///
/// This type is `#[non_exhaustive]`: obtain requests from the core.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    id: RequestId,
    method: Method,
    url: CheckedUrl,
    accept: Accept,
    headers: Vec<RequestHeader>,
}

impl Request {
    #[must_use]
    pub fn id(&self) -> RequestId {
        self.id
    }

    #[must_use]
    pub fn method(&self) -> Method {
        self.method
    }

    #[must_use]
    pub fn url(&self) -> &CheckedUrl {
        &self.url
    }

    /// The headers in declaration order.
    #[must_use]
    pub fn headers(&self) -> &[RequestHeader] {
        &self.headers
    }
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the step engine is the first caller")
)]
impl Request {
    /// A request with its ID. Each secret header must name a host secret that is bound
    /// to the origin of `url`; else the request fails the secret binding.
    pub(crate) fn new(
        method: Method,
        url: CheckedUrl,
        accept: Accept,
        headers: Vec<RequestHeader>,
        key: &RunKey,
        policy: &NetworkPolicy,
    ) -> Result<Self, Refusal> {
        let origin = url.origin();
        let mut keys = Vec::with_capacity(headers.len());
        for header in &headers {
            let value = match &header.value {
                HeaderValue::Text(text) => Part::Text(text),
                HeaderValue::Secret { name, prefix } => Part::Secret {
                    name,
                    prefix,
                    value: policy.bind_secret(name, &origin)?,
                },
            };
            keys.push(header_key(&header.name, value, key));
        }
        Ok(Self {
            id: identity(method, &url, accept, keys),
            method,
            url,
            accept,
            headers,
        })
    }
}

/// The version tag of the canonical text. Change it with any change of the text.
const IDENTITY_VERSION: &str = "mds-request-1";

/// The SHA-256 digest of the canonical text of a request. The text is a sequence of
/// fields, each its byte length as a u64 big-endian number and then its bytes: the
/// version tag, the method, the URL text, the format (`json`, `yaml` or `text`), the
/// header count as a u64 big-endian number, each header key in byte order, and the body
/// (empty in this release).
fn identity(method: Method, url: &CheckedUrl, accept: Accept, mut keys: Vec<String>) -> RequestId {
    // The keys are strings, so this sort is the one string sort that the crate has, and
    // the WASM build carries no other sort code for it.
    keys.sort_unstable();
    let mut text = Vec::new();
    let count = u64::try_from(keys.len()).unwrap_or(u64::MAX).to_be_bytes();
    for part in [
        IDENTITY_VERSION.as_bytes(),
        method.as_str().as_bytes(),
        url.to_text().as_bytes(),
        accept_name(accept).as_bytes(),
        &count,
    ] {
        push_field(&mut text, part);
    }
    for header in &keys {
        push_field(&mut text, header.as_bytes());
    }
    push_field(&mut text, b"");
    RequestId(sha256(&[&text]))
}

/// The value of one header as the identity reads it.
enum Part<'a> {
    Text(&'a str),
    Secret {
        name: &'a str,
        prefix: &'a str,
        value: &'a SecretValue,
    },
}

/// The key of one header: the lower-case name and NUL, so that the keys sort in
/// lower-case name order, and then the parts as netstrings (byte length, `:`, bytes).
/// A text header gives `t`, the name and the text. A secret header gives `s`, the name,
/// the secret name, the prefix and the hex HMAC-SHA-256 of the value under the run key.
fn header_key(name: &str, value: Part<'_>, key: &RunKey) -> String {
    let name = name.to_ascii_lowercase();
    let mut text = format!("{name}\0");
    match value {
        Part::Secret {
            name: secret,
            prefix,
            value,
        } => {
            let hmac = hex(&hmac_sha256(&key.0, &[value.expose_for_send().as_bytes()]));
            text.push('s');
            for part in [name.as_str(), secret, prefix, &hmac] {
                push_netstring(&mut text, part);
            }
        }
        Part::Text(value) => {
            text.push('t');
            for part in [name.as_str(), value] {
                push_netstring(&mut text, part);
            }
        }
    }
    text
}

fn push_field(out: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
}

fn push_netstring(out: &mut String, text: &str) {
    out.push_str(&text.len().to_string());
    out.push(':');
    out.push_str(text);
}

fn accept_name(accept: Accept) -> &'static str {
    match accept {
        Accept::Json => "json",
        Accept::Yaml => "yaml",
        Accept::Text => "text",
    }
}

fn lower_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
