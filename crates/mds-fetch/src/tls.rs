//! The TLS settings of the sender: the ring provider, the Mozilla roots and the roots of
//! the host. The certificate check is always on.

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

use crate::SetupError;

/// The roots of a PEM file that the host gives: one or more `CERTIFICATE` sections.
/// Other sections are skipped.
///
/// # Errors
///
/// [`SetupError::RootFile`] when the text is not valid PEM, and [`SetupError::NoRoots`]
/// when it has no certificate.
pub fn parse_pem_roots(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, SetupError> {
    let roots = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SetupError::RootFile)?;
    if roots.is_empty() {
        return Err(SetupError::NoRoots);
    }
    Ok(roots)
}

/// The TLS settings of each request: the ring provider, set here so that no default of
/// the process applies, and the Mozilla roots plus `host_roots`. Each host root must be
/// a valid trust anchor: ureq skips a bad root with no error, so a bad root fails here.
pub(crate) fn client_config(host_roots: &[CertificateDer<'_>]) -> Result<TlsConfig, SetupError> {
    let mozilla = webpki_root_certs::TLS_SERVER_ROOT_CERTS;
    let mut certs = Vec::with_capacity(mozilla.len() + host_roots.len());
    certs.extend(
        mozilla
            .iter()
            .map(|root| Certificate::from_der(root.as_ref())),
    );
    let mut store = RootCertStore::empty();
    for (index, root) in host_roots.iter().enumerate() {
        store
            .add(root.clone())
            .map_err(|_| SetupError::Root { index })?;
        certs.push(Certificate::from_der(root.as_ref()).to_owned());
    }
    Ok(TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::Specific(Arc::new(certs)))
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build())
}
