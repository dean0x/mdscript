//! The sender: one ureq agent per request with a fixed configuration.
//!
//! Each request gets its own agent, resolver and connection pool, so no connection or
//! address stays from an earlier request. The agent has no proxy, and its connector
//! chain is TCP then rustls only: no proxy connector is in the chain.

use std::fmt;
use std::io::{self, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mds::{
    HeaderValue, Method, NetworkPolicy, Origin, Request, Response, TransportFailure,
    MAX_RESPONSE_BYTES, MAX_RESPONSE_HEADER_BYTES, REQUEST_TIMEOUT,
};
use rustls::pki_types::CertificateDer;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::tls::TlsConfig;
use ureq::unversioned::transport::{Connector, RustlsConnector, TcpConnector};
use ureq::{Agent, Body};

use crate::resolve::{AddressRefused, CheckedResolver, Lookup, SystemLookup, Target};
use crate::tls::client_config;
use crate::SetupError;

/// The `User-Agent` of every request.
const USER_AGENT: &str = concat!("mds-fetch/", env!("CARGO_PKG_VERSION"));
/// The `Accept` of every request: the data formats that the core reads.
const ACCEPT: &str = "application/json, application/yaml, text/plain";
/// The `Accept-Encoding` of every request. The sender decodes no content coding, so the
/// body cap counts the bytes that the core reads.
const ACCEPT_ENCODING: &str = "identity";

/// The sender of the requests of step compiles.
///
/// It follows no redirect, uses no proxy, checks every address of a lookup with
/// [`NetworkPolicy::check_address`] before it connects, keeps the certificate check on,
/// reads at most the body cap plus one byte and binds each request with a deadline.
///
/// This type is `#[non_exhaustive]`: obtain values from [`Fetcher::new`].
#[non_exhaustive]
#[derive(Clone)]
pub struct Fetcher {
    policy: Arc<NetworkPolicy>,
    tls: TlsConfig,
    lookup: Arc<dyn Lookup>,
    deadline: Duration,
    body_cap: u64,
}

impl fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetcher")
            .field("deadline", &self.deadline)
            .field("body_cap", &self.body_cap)
            .finish_non_exhaustive()
    }
}

impl Fetcher {
    /// A sender for the requests of step compiles. `host_roots` join the Mozilla roots;
    /// no root comes from the environment or the system.
    ///
    /// The sender does not examine the allow list again: it sends each request that the
    /// core gives it. It uses `policy` only for the address check and to bind the secret
    /// headers. Thus give the sender the same `NetworkPolicy` as the
    /// [`mds::FetchContext`] of the step compiles.
    ///
    /// # Errors
    ///
    /// [`SetupError::Root`] when a host root is not a valid trust anchor.
    pub fn new(
        policy: Arc<NetworkPolicy>,
        host_roots: &[CertificateDer<'_>],
    ) -> Result<Self, SetupError> {
        Ok(Self {
            policy,
            tls: client_config(host_roots)?,
            lookup: Arc::new(SystemLookup::new()),
            deadline: REQUEST_TIMEOUT,
            body_cap: MAX_RESPONSE_BYTES,
        })
    }

    /// The sender with `lookup` for host names.
    #[must_use]
    pub fn with_lookup(self, lookup: impl Lookup + 'static) -> Self {
        Self {
            lookup: Arc::new(lookup),
            ..self
        }
    }

    /// The sender with a request deadline of at most `deadline`. It can only narrow the
    /// core deadline of 10 s.
    #[must_use]
    pub fn with_deadline(self, deadline: Duration) -> Self {
        Self {
            deadline: deadline.min(REQUEST_TIMEOUT),
            ..self
        }
    }

    /// The sender with a body cap of at most `cap` bytes. It can only narrow the core
    /// cap of 10 MiB.
    #[must_use]
    pub fn with_body_cap(self, cap: u64) -> Self {
        Self {
            body_cap: cap.min(MAX_RESPONSE_BYTES),
            ..self
        }
    }

    /// Send `request` and give its response for the next step. The request ends before
    /// `deadline` and before the request deadline of the sender. A redirect answer comes
    /// back as it is: the core checks each hop.
    ///
    /// The sender binds each secret header again for the request origin before it
    /// writes the header; a header that does not bind sends nothing. Every failure is a
    /// fixed [`TransportFailure`]: no error text of a lower layer is in the response.
    #[must_use]
    pub fn send(&self, request: &Request, deadline: Instant) -> Response {
        match self.try_send(request, deadline) {
            Ok(response) => response,
            Err(failure) => Response::failed(failure),
        }
    }

    fn try_send(&self, request: &Request, deadline: Instant) -> Result<Response, TransportFailure> {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(TransportFailure::Timeout)?;
        let url = request.url();
        let origin = url.origin();
        let uri: Uri = url
            .to_text()
            .parse()
            .map_err(|_| TransportFailure::Connect)?;
        // The URI that ureq reads must name the origin that the core checked.
        let target = Target::of(&uri)
            .filter(|target| names_origin(target, &origin))
            .ok_or(TransportFailure::Connect)?;
        let agent = self.agent(left.min(self.deadline), target);
        let mut builder = match request.method() {
            Method::Get => agent.get(uri),
            Method::Head => agent.head(uri),
            _ => return Err(TransportFailure::Connect),
        };
        for header in request.headers() {
            let value = header_text(&self.policy, header.value(), &origin)?;
            builder = builder.header(header.name(), value);
        }
        let response = builder.call().map_err(failure_of)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect();
        let body = read_body(response.into_body(), self.body_cap)?;
        Ok(Response::http(status, headers, body))
    }

    /// The agent of one request: no proxy, no redirect, no idle connection, the header
    /// cap, the deadline, fixed request headers and a resolver for `target` only.
    fn agent(&self, timeout: Duration, target: Target) -> Agent {
        let config = Config::builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .max_response_header_size(MAX_RESPONSE_HEADER_BYTES)
            .timeout_global(Some(timeout))
            .user_agent(USER_AGENT)
            .accept(ACCEPT)
            .accept_encoding(ACCEPT_ENCODING)
            .tls_config(self.tls.clone())
            .build();
        let connector = ().chain(TcpConnector::default()).chain(RustlsConnector::default());
        let resolver = CheckedResolver {
            policy: Arc::clone(&self.policy),
            lookup: Arc::clone(&self.lookup),
            target,
        };
        Agent::with_parts(config, connector, resolver)
    }
}

/// The target is the host and the port of `origin`.
fn names_origin(target: &Target, origin: &Origin) -> bool {
    let host = origin.host().to_string();
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(&host);
    target.host == host && target.port == origin.port()
}

/// The text that a header sends: a text value as it is, or the prefix and then the
/// value of the host secret that binds to `origin`.
fn header_text(
    policy: &NetworkPolicy,
    value: &HeaderValue,
    origin: &Origin,
) -> Result<String, TransportFailure> {
    match value {
        HeaderValue::Text(text) => Ok(text.clone()),
        HeaderValue::Secret { name, prefix } => {
            let secret = policy
                .bind_secret(name, origin)
                .map_err(|_| TransportFailure::Connect)?;
            Ok(format!("{prefix}{}", secret.expose_for_send()))
        }
        _ => Err(TransportFailure::Connect),
    }
}

/// The body, read to at most `cap` plus one byte. A longer body fails the request.
fn read_body(body: Body, cap: u64) -> Result<Vec<u8>, TransportFailure> {
    let mut bytes = Vec::new();
    body.into_reader()
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| failure_of(ureq::Error::from(error)))?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > cap) {
        return Err(TransportFailure::BodyTooLarge);
    }
    Ok(bytes)
}

/// The fixed failure of a ureq error. No error text crosses.
fn failure_of(error: ureq::Error) -> TransportFailure {
    match error {
        ureq::Error::Timeout(_) | ureq::Error::BodyStalled => TransportFailure::Timeout,
        ureq::Error::Other(inner) if inner.is::<AddressRefused>() => {
            TransportFailure::AddressRefused
        }
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Other(_) => {
            TransportFailure::Connect
        }
        ureq::Error::Tls(_)
        | ureq::Error::Pem(_)
        | ureq::Error::Rustls(_)
        | ureq::Error::TlsRequired => TransportFailure::Tls,
        ureq::Error::LargeResponseHeader(..) => TransportFailure::HeadersTooLarge,
        ureq::Error::BodyExceedsLimit(_) => TransportFailure::BodyTooLarge,
        ureq::Error::Io(error) => io_failure(&error),
        _ => TransportFailure::Protocol,
    }
}

/// The failure of an I/O error. A TLS error of the handshake comes as an I/O error that
/// holds the rustls error.
fn io_failure(error: &io::Error) -> TransportFailure {
    if error
        .get_ref()
        .is_some_and(|inner| inner.is::<rustls::Error>())
    {
        return TransportFailure::Tls;
    }
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => TransportFailure::Timeout,
        io::ErrorKind::InvalidData => TransportFailure::Protocol,
        _ => TransportFailure::Connect,
    }
}
