//! Name lookup and the address check: the sender connects only to an address that the
//! policy passed.

use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use mds::{NetworkPolicy, MAX_RESOLVED_ADDRS};
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ArrayVec, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;

/// A name lookup. The sender calls it once per request, for a host name only: an IP
/// address host needs no lookup.
pub trait Lookup: Send + Sync {
    /// The addresses of `host` for `port`, in less than `timeout`. An answer of more
    /// than [`MAX_RESOLVED_ADDRS`] addresses fails the request.
    ///
    /// # Errors
    ///
    /// An I/O error of kind [`io::ErrorKind::TimedOut`] for a lookup past `timeout`,
    /// and any other error when the name has no address.
    fn lookup(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>>;
}

/// The lookup of the operating system. It runs each lookup on its own thread and waits
/// at most the timeout. A late lookup thread ends when the system lookup returns.
///
/// This type is `#[non_exhaustive]`: obtain values from [`SystemLookup::new`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLookup;

impl SystemLookup {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Vec<SocketAddr>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let target = (host.to_owned(), port);
        thread::Builder::new()
            .name("mds-fetch-lookup".to_owned())
            .spawn(move || {
                // One more address than the cap shows that the answer is too long.
                let found = target
                    .to_socket_addrs()
                    .map(|addrs| addrs.take(MAX_RESOLVED_ADDRS + 1).collect());
                // The receiver is gone after a timeout; then the answer is not used.
                let _ = sender.send(found);
            })?;
        match receiver.recv_timeout(timeout) {
            Ok(found) => found,
            Err(RecvTimeoutError::Timeout) => Err(io::ErrorKind::TimedOut.into()),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("the lookup stopped")),
        }
    }
}

/// The resolver failure of a request that an address check refused.
#[derive(Debug)]
pub(crate) struct AddressRefused;

impl fmt::Display for AddressRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an address is in a refused range")
    }
}

impl Error for AddressRefused {}

/// The resolver failure of a lookup with more than [`MAX_RESOLVED_ADDRS`] addresses.
#[derive(Debug)]
pub(crate) struct TooManyAddresses;

impl fmt::Display for TooManyAddresses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the lookup gave too many addresses")
    }
}

impl Error for TooManyAddresses {}

/// The host and the port of the one request that a resolver serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// The host with no IPv6 brackets.
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl Target {
    /// The target of `uri`. An `https` or `http` URI with no port has the default port.
    pub(crate) fn of(uri: &Uri) -> Option<Self> {
        let default_port = match uri.scheme_str()? {
            "https" => 443,
            "http" => 80,
            _ => return None,
        };
        let host = uri.host()?;
        let host = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host);
        Some(Self {
            host: host.to_owned(),
            port: uri.port_u16().unwrap_or(default_port),
        })
    }
}

/// The resolver of one request: one lookup for a host name, none for an IP address, and
/// an address check for every address of the answer before ureq connects. One refused
/// address refuses the request.
pub(crate) struct CheckedResolver {
    pub(crate) policy: Arc<NetworkPolicy>,
    pub(crate) lookup: Arc<dyn Lookup>,
    pub(crate) target: Target,
}

impl fmt::Debug for CheckedResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedResolver").finish_non_exhaustive()
    }
}

impl Resolver for CheckedResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        // The resolver serves one request: any other host or port is refused.
        if Target::of(uri).as_ref() != Some(&self.target) {
            return Err(ureq::Error::ConnectionFailed);
        }
        let Target { host, port } = &self.target;
        let found = match host.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, *port)],
            Err(_) => match self.lookup.lookup(host, *port, *timeout.after) {
                Ok(found) => found,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    return Err(ureq::Error::Timeout(timeout.reason));
                }
                Err(_) => return Err(ureq::Error::HostNotFound),
            },
        };
        let checked = check_answer(&self.policy, &found, *port)?;
        let mut addrs = self.empty();
        let room = capacity(&addrs);
        for address in checked.into_iter().take(room) {
            addrs.push(address);
        }
        Ok(addrs)
    }
}

/// The capacity of a ureq address list. A push past it panics, so the copy takes at
/// most this many addresses.
fn capacity<T, const N: usize>(_: &ArrayVec<T, N>) -> usize {
    N
}

/// The addresses of a lookup answer, each with the request port, after the address
/// check of each one. An empty answer, an answer past the cap or one refused address
/// fails the request.
fn check_answer(
    policy: &NetworkPolicy,
    found: &[SocketAddr],
    port: u16,
) -> Result<Vec<SocketAddr>, ureq::Error> {
    if found.is_empty() {
        return Err(ureq::Error::HostNotFound);
    }
    if found.len() > MAX_RESOLVED_ADDRS {
        return Err(ureq::Error::Other(Box::new(TooManyAddresses)));
    }
    let mut checked = Vec::with_capacity(found.len());
    for address in found {
        let address = SocketAddr::new(address.ip(), port);
        if policy.check_address(address).is_err() {
            return Err(ureq::Error::Other(Box::new(AddressRefused)));
        }
        checked.push(address);
    }
    Ok(checked)
}
