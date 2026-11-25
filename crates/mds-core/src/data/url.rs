//! The strict URL parser of network data entries.
//!
//! One parser reads every URL that a request can go to: the expanded URL of an entry and
//! each redirect `location`. It accepts a narrow form and refuses all else, so no two
//! readers can disagree about the host of a URL. A refusal is a fixed reason that never
//! holds the input text.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use super::decl::has_scheme;
use crate::limits::MAX_URL_BYTES;

/// The maximum bytes of a host name.
const MAX_HOST_BYTES: usize = 253;
/// The maximum bytes of one host name label.
const MAX_LABEL_BYTES: usize = 63;
/// The maximum digits of a port.
const MAX_PORT_DIGITS: usize = 5;

/// The scheme of a request URL.
///
/// This enum is `#[non_exhaustive]`: new schemes may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    Https,
    Http,
}

impl Scheme {
    /// The scheme name: `https` or `http`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Https => "https",
            Scheme::Http => "http",
        }
    }

    pub(crate) fn default_port(self) -> u16 {
        match self {
            Scheme::Https => 443,
            Scheme::Http => 80,
        }
    }
}

/// The host of a request URL. A URL holds a host only after the strict URL parser
/// checked it.
///
/// This enum is `#[non_exhaustive]`: new host forms may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    /// A lower-case name of letter, digit and hyphen labels.
    Name(String),
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Name(name) => f.write_str(name),
            Host::V4(address) => write!(f, "{address}"),
            Host::V6(address) => write!(f, "[{address}]"),
        }
    }
}

/// A URL that passed the strict parser. It has no `Display`, and its `Debug` shows the
/// [`Origin`] only, so a message can not show its path or query by accident.
///
/// Obtain a URL from [`Request::url`](crate::Request::url). This type is
/// `#[non_exhaustive]` and cannot be built with a struct literal.
#[non_exhaustive]
#[derive(Clone, PartialEq, Eq)]
pub struct CheckedUrl {
    pub(crate) scheme: Scheme,
    pub(crate) host: Host,
    /// The port as written. `None` means the default port of the scheme.
    pub(crate) port: Option<u16>,
    /// The path. It starts with `/`.
    pub(crate) path: String,
    /// The text after the first `?`, if the URL has one.
    pub(crate) query: Option<String>,
}

impl fmt::Debug for CheckedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedUrl")
            .field("origin", &format_args!("{}", self.origin()))
            .finish_non_exhaustive()
    }
}

impl CheckedUrl {
    /// The scheme, host and port. A URL with no port has the default port.
    #[must_use]
    pub fn origin(&self) -> Origin {
        Origin {
            scheme: self.scheme,
            host: self.host.clone(),
            port: self.port.unwrap_or(self.scheme.default_port()),
        }
    }

    /// The path. It starts with `/`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The text after the first `?`, if the URL has one.
    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// The URL text that a request sends: the origin (with no default port), the path
    /// and the query. Never put it in a message: a message shows the origin only.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = format!("{}{}", self.origin(), self.path);
        if let Some(query) = &self.query {
            text.push('?');
            text.push_str(query);
        }
        text
    }
}

/// The scheme, host and port of a URL. The default port and no port are the same origin.
///
/// Obtain an origin from [`AllowItem::origin`](crate::AllowItem::origin). This type is
/// `#[non_exhaustive]` and cannot be built with a struct literal.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub(crate) scheme: Scheme,
    pub(crate) host: Host,
    pub(crate) port: u16,
}

impl Origin {
    #[must_use]
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The port. The default port of the scheme when the URL names none.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme.as_str(), self.host)?;
        if self.port != self.scheme.default_port() {
            write!(f, ":{}", self.port)?;
        }
        Ok(())
    }
}

/// Why the strict parser refused a URL. The reason is fixed text and never holds input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UrlRefusal {
    TooLong,
    NotVisibleAscii,
    ForbiddenChar,
    Percent,
    Scheme,
    EmptyHost,
    HostName,
    TrailingDot,
    HostTooLong,
    Ipv4,
    Ipv6,
    Zone,
    Port,
    Path,
    NotAbsolute,
}

impl UrlRefusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            UrlRefusal::TooLong => "URL exceeds the maximum length of 8192 bytes",
            UrlRefusal::NotVisibleAscii => "URL can contain only visible ASCII characters",
            UrlRefusal::ForbiddenChar => "URL cannot contain `@`, a backslash or `#`",
            UrlRefusal::Percent => "each `%` in a URL needs two hex digits",
            UrlRefusal::Scheme => "expected the scheme https or http",
            UrlRefusal::EmptyHost => "URL has no host",
            UrlRefusal::HostName => {
                "expected a host name of lower-case letter, digit and inner hyphen labels"
            }
            UrlRefusal::TrailingDot => "host name cannot end with a dot",
            UrlRefusal::HostTooLong => "host name exceeds 253 bytes",
            UrlRefusal::Ipv4 => "expected an IPv4 address of four decimal parts from 0 to 255",
            UrlRefusal::Ipv6 => "expected an IPv6 address in brackets",
            UrlRefusal::Zone => "IPv6 address cannot have a zone",
            UrlRefusal::Port => "expected a port from 1 to 65535",
            UrlRefusal::Path => "expected a path that starts with `/`",
            UrlRefusal::NotAbsolute => "expected an absolute URL or an absolute path",
        }
    }
}

/// Parse an absolute `https` or `http` URL.
#[must_use = "a URL can be refused"]
pub(crate) fn parse_absolute(text: &str) -> Result<CheckedUrl, UrlRefusal> {
    if text.len() > MAX_URL_BYTES {
        return Err(UrlRefusal::TooLong);
    }
    // After this check every byte is ASCII, so each split below is on a char boundary.
    if !text.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(UrlRefusal::NotVisibleAscii);
    }
    if text.contains(['@', '\\', '#']) {
        return Err(UrlRefusal::ForbiddenChar);
    }
    let percent_ok = text.split('%').skip(1).all(|rest| {
        rest.as_bytes()
            .first_chunk::<2>()
            .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit))
    });
    if !percent_ok {
        return Err(UrlRefusal::Percent);
    }
    let (scheme, rest) = if let Some(rest) = text.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = text.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        return Err(UrlRefusal::Scheme);
    };
    let (authority, target) = split_before(rest, &['/', '?']);
    let (host, port) = parse_authority(authority)?;
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None if target.is_empty() => ("/", None),
        None => (target, None),
    };
    if !path.starts_with('/') {
        return Err(UrlRefusal::Path);
    }
    Ok(CheckedUrl {
        scheme,
        host,
        port,
        path: path.to_owned(),
        query: query.map(str::to_owned),
    })
}

/// Parse a redirect `location`: an absolute URL, or an absolute path on the origin of
/// `base`. Every other form is refused, and both forms go through [`parse_absolute`].
#[must_use = "a location can be refused"]
pub(crate) fn parse_location(base: &CheckedUrl, text: &str) -> Result<CheckedUrl, UrlRefusal> {
    if text.len() > MAX_URL_BYTES {
        return Err(UrlRefusal::TooLong);
    }
    match text.strip_prefix('/') {
        Some(rest) if !rest.starts_with('/') => parse_absolute(&format!("{}{text}", base.origin())),
        None if has_scheme(text) => parse_absolute(text),
        _ => Err(UrlRefusal::NotAbsolute),
    }
}

/// Split `text` before the first of `marks`, or at its end.
fn split_before<'a>(text: &'a str, marks: &[char]) -> (&'a str, &'a str) {
    let at = text.find(marks).unwrap_or(text.len());
    text.split_at_checked(at).unwrap_or((text, ""))
}

fn parse_authority(authority: &str) -> Result<(Host, Option<u16>), UrlRefusal> {
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (inside, after) = bracketed.split_once(']').ok_or(UrlRefusal::Ipv6)?;
        if inside.contains('%') {
            return Err(UrlRefusal::Zone);
        }
        let address = inside.parse::<Ipv6Addr>().map_err(|_| UrlRefusal::Ipv6)?;
        let port = match after {
            "" => None,
            _ => Some(parse_port(
                after.strip_prefix(':').ok_or(UrlRefusal::Ipv6)?,
            )?),
        };
        return Ok((Host::V6(address), port));
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let host = parse_host(host)?;
    let port = port.map(parse_port).transpose()?;
    Ok((host, port))
}

fn parse_port(text: &str) -> Result<u16, UrlRefusal> {
    if text.is_empty()
        || text.len() > MAX_PORT_DIGITS
        || !text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(UrlRefusal::Port);
    }
    match text.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(UrlRefusal::Port),
    }
}

fn parse_host(text: &str) -> Result<Host, UrlRefusal> {
    if text.is_empty() {
        return Err(UrlRefusal::EmptyHost);
    }
    if text.ends_with('.') {
        return Err(UrlRefusal::TrailingDot);
    }
    if text.len() > MAX_HOST_BYTES {
        return Err(UrlRefusal::HostTooLong);
    }
    // A name whose last label reads as a number is an IPv4 form, never a name: other
    // readers take `127.1` or `0x7f000001` as an address.
    if text.rsplit('.').next().is_some_and(is_numeric_label) {
        return parse_ipv4(text).map(Host::V4).ok_or(UrlRefusal::Ipv4);
    }
    if text.split('.').all(is_name_label) {
        Ok(Host::Name(text.to_owned()))
    } else {
        Err(UrlRefusal::HostName)
    }
}

fn is_numeric_label(label: &str) -> bool {
    let digits = !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_digit());
    let hex = label
        .strip_prefix("0x")
        .is_some_and(|rest| rest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    digits || hex
}

fn is_name_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_BYTES
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Four decimal parts from 0 to 255, with no leading zero.
fn parse_ipv4(text: &str) -> Option<Ipv4Addr> {
    let mut parts = text.split('.');
    let mut octets = [0u8; 4];
    for octet in &mut octets {
        let part = parts.next()?;
        let canonical = matches!(part.len(), 1..=3)
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        if !canonical {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(Ipv4Addr::from(octets))
}
