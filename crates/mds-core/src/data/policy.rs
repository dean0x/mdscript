//! The network policy of data requests: the host allow list and its narrowing, the import
//! opt-in, the host secrets, the address table and the redirect rules.
//!
//! The network is off until a host builds a [`NetworkPolicy`]. The core checks every
//! request against it, and a policy can only narrow. A refusal is a fixed [`Refusal`]
//! that names its check and never holds input text.

use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use super::decl::{is_credential_header, DeclaredMethod};
use super::request::Method;
use super::url::{self, CheckedUrl, Host, Origin, Scheme};
use crate::limits::{MAX_ALLOW_ITEMS, MAX_REDIRECTS, MAX_SECRETS};
use crate::parser::is_valid_identifier;

/// The check that refused a request.
///
/// This enum is `#[non_exhaustive]`: new checks may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// The origin is not in the allow list.
    AllowList,
    /// A network entry of an imported module needs the import opt-in.
    ImportOptIn,
    /// The strict URL parser refused an expanded URL or a redirect location.
    UrlParser,
    /// A redirect hop breaks a hop rule.
    RedirectCheck,
    /// A secret header does not match a host secret.
    SecretBinding,
    /// A name lookup gave an address in a refused range.
    TransportAddress,
    /// A response holds a secret value.
    ResponseCheck,
    /// The request method is not one that this release sends.
    Method,
}

impl Layer {
    /// The check's name: `allow-list`, `import-opt-in`, `url-parser`, `redirect-check`,
    /// `secret-binding`, `transport-address`, `response-check` or `method`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Layer::AllowList => "allow-list",
            Layer::ImportOptIn => "import-opt-in",
            Layer::UrlParser => "url-parser",
            Layer::RedirectCheck => "redirect-check",
            Layer::SecretBinding => "secret-binding",
            Layer::TransportAddress => "transport-address",
            Layer::ResponseCheck => "response-check",
            Layer::Method => "method",
        }
    }
}

/// A policy refusal: the check and a fixed reason that never holds input text.
///
/// Obtain refusals from the checks of [`NetworkPolicy`]. This type is
/// `#[non_exhaustive]` and cannot be built outside the core.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    layer: Layer,
    reason: &'static str,
}

/// The refusal of an address in a refused range, for the address check and for a sender
/// that reports it.
pub(crate) const ADDRESS_REFUSED: Refusal =
    Refusal::new(Layer::TransportAddress, "the address is in a refused range");

impl Refusal {
    pub(crate) const fn new(layer: Layer, reason: &'static str) -> Self {
        Self { layer, reason }
    }

    #[must_use]
    pub fn layer(&self) -> Layer {
        self.layer
    }

    #[must_use]
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

/// Why the host input of a policy is refused. The message is fixed text and never holds
/// the input.
///
/// This enum is `#[non_exhaustive]`: new errors may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// An allow item is not in the item grammar. The value is the fixed reason.
    AllowItem(&'static str),
    /// An allow list has no item.
    NoItems,
    /// An allow list has more than 64 items.
    TooManyItems,
    /// A secret name is not an identifier.
    SecretName,
    /// A secret value is empty or holds a character that is not visible ASCII.
    SecretValue,
    /// A secret origin does not use `https`.
    SecretScheme,
    /// A policy has more than 16 secrets.
    TooManySecrets,
    /// Two secrets have the same name.
    DuplicateSecret,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyError::AllowItem(reason) => f.write_str(reason),
            PolicyError::NoItems => f.write_str("an allow list needs at least one item"),
            PolicyError::TooManyItems => {
                write!(f, "an allow list has at most {MAX_ALLOW_ITEMS} items")
            }
            PolicyError::SecretName => f.write_str("a secret name must be an identifier"),
            PolicyError::SecretValue => {
                f.write_str("a secret value must be one or more visible ASCII characters")
            }
            PolicyError::SecretScheme => f.write_str("a secret origin must use https"),
            PolicyError::TooManySecrets => write!(f, "a policy has at most {MAX_SECRETS} secrets"),
            PolicyError::DuplicateSecret => f.write_str("two secrets have the same name"),
        }
    }
}

impl Error for PolicyError {}

/// Why [`NetworkPolicy::narrow`] refused a narrower policy.
///
/// This enum is `#[non_exhaustive]`: new errors may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NarrowError {
    /// The item at `index` of the narrower list permits a request that the wider
    /// policy refuses.
    ItemNotAllowed { index: usize },
    /// The narrower policy allows imports, and the wider policy does not.
    ImportsNotAllowed,
    /// The narrower list breaks a list rule.
    Policy(PolicyError),
}

impl fmt::Display for NarrowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NarrowError::ItemNotAllowed { index } => write!(
                f,
                "allow item {index} permits a request that the wider allow list refuses"
            ),
            NarrowError::ImportsNotAllowed => {
                f.write_str("the import opt-in is not in the wider policy")
            }
            NarrowError::Policy(error) => error.fmt(f),
        }
    }
}

impl Error for NarrowError {}

/// One item of a host allow list: an exact scheme, host and port.
///
/// The grammar is `host`, `host:port`, `https://host[:port]` or `http://host:port`. The
/// host rules are those of a request URL: a lower-case name or a canonical IP address.
/// An item has no path, no wildcard, no trailing dot and no upper-case letter. `http`
/// needs its scheme and a port.
///
/// This type is `#[non_exhaustive]`: obtain items from [`AllowItem::parse`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowItem {
    scheme: Scheme,
    host: Host,
    /// The port as written. `None` is the default port of the scheme.
    port: Option<u16>,
}

impl AllowItem {
    /// Parse one allow item.
    #[must_use = "an allow item can be refused"]
    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        let (scheme, authority) = if let Some(rest) = text.strip_prefix("https://") {
            (Scheme::Https, rest)
        } else if let Some(rest) = text.strip_prefix("http://") {
            (Scheme::Http, rest)
        } else if text.contains("://") {
            return Err(PolicyError::AllowItem(
                "expected the scheme https:// or http://, or no scheme",
            ));
        } else {
            (Scheme::Https, text)
        };
        if authority.contains(['/', '?', '#']) {
            return Err(PolicyError::AllowItem(
                "an allow item cannot have a path, a query or a fragment",
            ));
        }
        // One strict parser reads every host, so the item goes through the URL parser.
        let parsed = url::parse_absolute(&format!("{}://{authority}/", scheme.as_str()))
            .map_err(|refusal| PolicyError::AllowItem(refusal.reason()))?;
        if scheme == Scheme::Http && parsed.port.is_none() {
            return Err(PolicyError::AllowItem(
                "an http allow item needs the http:// scheme and a port",
            ));
        }
        Ok(Self {
            scheme,
            host: parsed.host,
            port: parsed.port,
        })
    }

    /// The origin that the item allows. An item with no port has the default port.
    #[must_use]
    pub fn origin(&self) -> Origin {
        Origin {
            scheme: self.scheme,
            host: self.host.clone(),
            port: self.effective_port(),
        }
    }

    /// The item allows requests to `origin`: the same scheme and host, and the same port,
    /// where the default port and no port are equal.
    pub(crate) fn matches(&self, origin: &Origin) -> bool {
        self.scheme == origin.scheme
            && self.host == origin.host
            && self.effective_port() == origin.port
    }

    fn effective_port(&self) -> u16 {
        self.port.unwrap_or(self.scheme.default_port())
    }

    /// The item is the IP address of `address` with its port, written in the item.
    fn opens_address(&self, address: SocketAddr) -> bool {
        let same_ip = match (&self.host, address.ip()) {
            (Host::V4(item), IpAddr::V4(ip)) => *item == ip,
            (Host::V6(item), IpAddr::V6(ip)) => *item == ip,
            _ => false,
        };
        same_ip && self.port == Some(address.port())
    }

    /// The item permits no request that `wider` refuses: the same origin, and for an IP
    /// address a written port only when `wider` has one, because such a port can open the
    /// address.
    fn within(&self, wider: &AllowItem) -> bool {
        let opens = self.port.is_some() && !matches!(self.host, Host::Name(_));
        wider.matches(&self.origin()) && (!opens || wider.port.is_some())
    }
}

/// The value of a host secret. Its `Debug` shows no value. It has no `Display` and no
/// `Serialize`, and [`SecretValue::expose_for_send`] is its one accessor.
///
/// This type is `#[non_exhaustive]`: obtain values from [`SecretValue::new`].
#[non_exhaustive]
#[derive(Clone)]
pub struct SecretValue(String);

impl SecretValue {
    /// A secret value: one or more visible ASCII characters.
    #[must_use = "a secret value can be refused"]
    pub fn new(value: String) -> Result<Self, PolicyError> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(PolicyError::SecretValue);
        }
        Ok(Self(value))
    }

    /// The value, for the request header of a send only. Never write it to a message, a
    /// log or an output.
    #[must_use]
    pub fn expose_for_send(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(..)")
    }
}

/// A host secret: a name in its own name space, a value and the one origin that can
/// receive it.
///
/// This type is `#[non_exhaustive]`: obtain secrets from [`Secret::new`].
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Secret {
    name: String,
    value: SecretValue,
    origin: Origin,
}

impl Secret {
    /// A secret named `name` (an identifier) that only requests to `origin` (an `https`
    /// origin) can carry.
    #[must_use = "a secret can be refused"]
    pub fn new(name: &str, value: SecretValue, origin: Origin) -> Result<Self, PolicyError> {
        if !is_valid_identifier(name) {
            return Err(PolicyError::SecretName);
        }
        if origin.scheme != Scheme::Https {
            return Err(PolicyError::SecretScheme);
        }
        Ok(Self {
            name: name.to_owned(),
            value,
            origin,
        })
    }
}

/// The network grant of a host: the allow list, the import opt-in and the host secrets.
///
/// This type is `#[non_exhaustive]`: obtain policies from [`NetworkPolicy::new`].
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct NetworkPolicy {
    items: Vec<AllowItem>,
    imports: bool,
    secrets: Vec<Secret>,
}

impl NetworkPolicy {
    /// A policy that allows the origins of `items` (1 to 64 items), with no import opt-in
    /// and no secret.
    #[must_use = "a policy can be refused"]
    pub fn new(items: Vec<AllowItem>) -> Result<Self, PolicyError> {
        check_item_count(items.len())?;
        Ok(Self {
            items,
            imports: false,
            secrets: Vec::new(),
        })
    }

    /// Set the import opt-in: a network entry of an imported module can then send.
    #[must_use]
    pub fn with_imports(self, allow: bool) -> Self {
        Self {
            imports: allow,
            ..self
        }
    }

    /// Add a host secret. A policy has at most 16 secrets, each with its own name.
    #[must_use = "a secret can be refused"]
    pub fn with_secret(mut self, secret: Secret) -> Result<Self, PolicyError> {
        if self.secrets.iter().any(|known| known.name == secret.name) {
            return Err(PolicyError::DuplicateSecret);
        }
        if self.secrets.len() >= MAX_SECRETS {
            return Err(PolicyError::TooManySecrets);
        }
        self.secrets.push(secret);
        Ok(self)
    }

    /// A policy with only the grants of `items` and `imports`. Each item must permit no
    /// request that this policy refuses, and `imports` can be true only when this policy
    /// allows imports. The secrets stay the same. This is the one narrowing rule of every
    /// host surface.
    #[must_use = "a narrower policy can be refused"]
    pub fn narrow(&self, items: Vec<AllowItem>, imports: bool) -> Result<Self, NarrowError> {
        check_item_count(items.len()).map_err(NarrowError::Policy)?;
        let wider = |item: &AllowItem| !self.items.iter().any(|known| item.within(known));
        if let Some(index) = items.iter().position(wider) {
            return Err(NarrowError::ItemNotAllowed { index });
        }
        if imports && !self.imports {
            return Err(NarrowError::ImportsNotAllowed);
        }
        Ok(Self {
            items,
            imports,
            secrets: self.secrets.clone(),
        })
    }

    /// Check one address that a name lookup gave, before a connection to it. An address
    /// in a refused range passes only when an allow item is that exact IP address with
    /// that exact port, written in the item. So a name can not open such an address that
    /// the host did not list. An IPv4-mapped IPv6 address never equals an IPv4 item.
    #[must_use = "an address can be refused"]
    pub fn check_address(&self, address: SocketAddr) -> Result<(), Refusal> {
        if !is_refused_address(address.ip())
            || self.items.iter().any(|item| item.opens_address(address))
        {
            Ok(())
        } else {
            Err(ADDRESS_REFUSED)
        }
    }
}

impl NetworkPolicy {
    /// Check the origin of a request: the allow list first, then the import opt-in for an
    /// entry that an imported module declares.
    pub(crate) fn check_origin(
        &self,
        origin: &Origin,
        from_entry_module: bool,
    ) -> Result<(), Refusal> {
        if !self.allows(origin) {
            return Err(Refusal::new(
                Layer::AllowList,
                "the origin is not in the allow list",
            ));
        }
        if !from_entry_module && !self.imports {
            return Err(Refusal::new(
                Layer::ImportOptIn,
                "a network entry of an imported module needs the import opt-in",
            ));
        }
        Ok(())
    }

    /// Check redirect hop number `hop` (the first redirect is hop 1) from `from` to `to`.
    /// The hop count binds first, then the scheme, then the allow list.
    pub(crate) fn check_hop(
        &self,
        from: &CheckedUrl,
        to: &CheckedUrl,
        hop: usize,
    ) -> Result<Hop, HopError> {
        if hop > MAX_REDIRECTS {
            return Err(HopError::TooMany);
        }
        if from.scheme == Scheme::Https && to.scheme == Scheme::Http {
            return Err(HopError::Refused(Refusal::new(
                Layer::RedirectCheck,
                "a redirect from https to http is refused",
            )));
        }
        let target = to.origin();
        if !self.allows(&target) {
            return Err(HopError::Refused(Refusal::new(
                Layer::RedirectCheck,
                "the redirect target is not in the allow list",
            )));
        }
        Ok(Hop {
            same_origin: from.origin() == target,
        })
    }

    /// The value of the host secret `name` for a request to `origin`. The secret must
    /// exist and name that exact origin, which is always an `https` origin. The core
    /// binds each secret header with it, and a sender binds it again before it writes
    /// the header.
    #[must_use = "a secret binding can be refused"]
    pub fn bind_secret(&self, name: &str, origin: &Origin) -> Result<&SecretValue, Refusal> {
        let Some(secret) = self.secrets.iter().find(|secret| secret.name == name) else {
            return Err(Refusal::new(
                Layer::SecretBinding,
                "the host gives no secret of this name",
            ));
        };
        if secret.origin != *origin {
            return Err(Refusal::new(
                Layer::SecretBinding,
                "the request origin is not the origin of the secret",
            ));
        }
        Ok(&secret.value)
    }

    /// The method that a request sends. This release sends GET and HEAD only.
    pub(crate) fn check_method(&self, method: DeclaredMethod) -> Result<Method, Refusal> {
        match method {
            DeclaredMethod::Get => Ok(Method::Get),
            DeclaredMethod::Head => Ok(Method::Head),
            DeclaredMethod::Post
            | DeclaredMethod::Put
            | DeclaredMethod::Patch
            | DeclaredMethod::Delete
            | DeclaredMethod::Options => Err(Refusal::new(
                Layer::Method,
                "this release sends GET and HEAD requests only",
            )),
        }
    }

    /// The name and the value of each host secret, for the response check only.
    pub(crate) fn secrets(&self) -> impl Iterator<Item = (&str, &SecretValue)> {
        self.secrets
            .iter()
            .map(|secret| (secret.name.as_str(), &secret.value))
    }

    fn allows(&self, origin: &Origin) -> bool {
        self.items.iter().any(|item| item.matches(origin))
    }
}

fn check_item_count(count: usize) -> Result<(), PolicyError> {
    match count {
        0 => Err(PolicyError::NoItems),
        1..=MAX_ALLOW_ITEMS => Ok(()),
        _ => Err(PolicyError::TooManyItems),
    }
}

/// A redirect hop that the policy allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hop {
    same_origin: bool,
}

impl Hop {
    /// A hop to another origin drops every secret header and each credential header.
    pub(crate) fn keeps_header(self, name: &str, secret: bool) -> bool {
        self.same_origin || !(secret || is_credential_header(name))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HopError {
    Refused(Refusal),
    /// The chain has more than `MAX_REDIRECTS` hops: a resource limit.
    TooMany,
}

/// The refused IPv4 ranges: the first address and the prefix length.
const REFUSED_V4: [(Ipv4Addr, u32); 12] = [
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
    (Ipv4Addr::new(255, 255, 255, 255), 32),
];

/// The refused IPv6 ranges: the first address and the prefix length. `::/96` holds the
/// unspecified and the loopback address. Each range that embeds an IPv4 address
/// (IPv4-compatible, IPv4-mapped, IPv4-translated, the two NAT64 prefixes, Teredo and 6to4)
/// is refused whole, so each IPv4 address that it embeds is too.
const REFUSED_V6: [(Ipv6Addr, u32); 11] = [
    (Ipv6Addr::UNSPECIFIED, 96),
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96),
    (Ipv6Addr::new(0, 0, 0, 0, 0xffff, 0, 0, 0), 96),
    (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96),
    (Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, 0, 0), 48),
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 32),
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
    (Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 0), 10),
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
];

// Each range is a valid prefix: its length fits the address, and its first address has
// no bit set past the prefix.
const _: () = {
    let mut at = 0;
    while at < REFUSED_V4.len() {
        let (first, length) = REFUSED_V4[at];
        assert!(length <= u32::BITS);
        assert!(u32::from_be_bytes(first.octets()) & !v4_mask(length) == 0);
        at += 1;
    }
    let mut at = 0;
    while at < REFUSED_V6.len() {
        let (first, length) = REFUSED_V6[at];
        assert!(length <= u128::BITS);
        assert!(u128::from_be_bytes(first.octets()) & !v6_mask(length) == 0);
        at += 1;
    }
};

const fn v4_mask(length: u32) -> u32 {
    match u32::MAX.checked_shl(u32::BITS - length) {
        Some(mask) => mask,
        None => 0,
    }
}

const fn v6_mask(length: u32) -> u128 {
    match u128::MAX.checked_shl(u128::BITS - length) {
        Some(mask) => mask,
        None => 0,
    }
}

fn is_refused_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let bits = u32::from(ip);
            REFUSED_V4
                .iter()
                .any(|&(first, length)| bits & v4_mask(length) == u32::from(first))
        }
        IpAddr::V6(ip) => {
            let bits = u128::from(ip);
            REFUSED_V6
                .iter()
                .any(|&(first, length)| bits & v6_mask(length) == u128::from(first))
        }
    }
}
