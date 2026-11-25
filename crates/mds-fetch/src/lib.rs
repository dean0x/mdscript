//! The native sender of URL data entries.
//!
//! [`Fetcher`] sends each [`mds::Request`] of a step compile and gives the
//! [`mds::Response`] that the host adds to the [`mds::FetchState`] of the next step.
//! This crate holds the HTTP, TLS and root list crates, so `mds-core` and `mds-wasm`
//! depend on none of them.
//!
//! The sender keeps these rules:
//! - It follows no redirect. A redirect answer goes back to the core, which checks
//!   each hop.
//! - It uses no proxy. The agent has no proxy setting, and its connector chain has no
//!   proxy connector, so a proxy environment variable has no effect.
//! - It looks up a host name once per request, checks every address of the answer with
//!   [`mds::NetworkPolicy::check_address`] and connects only to a checked address. One
//!   refused address refuses the request.
//! - It uses TLS 1.2 or 1.3 with the ring provider, and the certificate check is always
//!   on. The roots are the Mozilla list and the roots that the host gives, never roots
//!   of the environment or of the system.
//! - It reads at most the body cap plus one byte, and one deadline binds the request
//!   from the name lookup to the end of the body.
//! - It keeps no connection after a request.
//!
//! The sender installs no logger. ureq writes the request at the `debug` level and the
//! bytes on the wire at the `trace` level of the `log` crate, so a host that installs a
//! logger must keep those levels off for ureq.

use std::error::Error;
use std::fmt;

use mds::RunKey;

mod resolve;
mod send;
mod tls;

pub use resolve::{Lookup, SystemLookup};
pub use rustls::pki_types::CertificateDer;
pub use send::Fetcher;
pub use tls::parse_pem_roots;

/// Why the sender can not start. The message is fixed text and holds no input.
///
/// This enum is `#[non_exhaustive]`: new errors may be added in minor releases.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// The random source of the system failed.
    Random,
    /// A root file is not valid PEM.
    RootFile,
    /// A root file holds no certificate.
    NoRoots,
    /// The host root at this index of the list is not a valid trust anchor.
    Root { index: usize },
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupError::Random => f.write_str("the random source of the system failed"),
            SetupError::RootFile => f.write_str("the root file is not valid PEM"),
            SetupError::NoRoots => f.write_str("the root file holds no certificate"),
            SetupError::Root { index } => {
                write!(f, "root {index} of the host is not a valid trust anchor")
            }
        }
    }
}

impl Error for SetupError {}

/// A new per-run key for the request identities of one run, from the random source of
/// the system (through the ring provider of the sender).
///
/// # Errors
///
/// [`SetupError::Random`] when the random source fails.
pub fn run_key() -> Result<RunKey, SetupError> {
    let mut bytes = [0u8; 32];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes)
        .map_err(|_| SetupError::Random)?;
    Ok(RunKey::from_bytes(bytes))
}
