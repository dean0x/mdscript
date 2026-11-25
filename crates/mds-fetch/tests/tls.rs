//! The TLS sender: the certificate check, the host roots and the secret header. Each
//! test makes its CA, certificate and key at test time, so the repository holds no key.
//! The server is a rustls thread on `127.0.0.1:0` in this process.

mod common;

use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::{
    answer, entry, failure, http, read_head, reply_on, request, soon, Reply, Seen, Server,
};
use mds::{AllowItem, NetworkPolicy, Secret, SecretValue, TransportFailure};
use mds_fetch::{parse_pem_roots, CertificateDer, Fetcher, SetupError};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

const OK: &[u8] = b"{\"n\": 1}";

/// A test CA, and a server certificate and key that the CA signed.
struct Pki {
    ca: CertificateDer<'static>,
    ca_pem: String,
    server: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

/// A CA and a server certificate for the IP address `server_ip`, made now.
fn pki(server_ip: &str) -> Pki {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let server = CertificateParams::new(vec![server_ip.to_owned()])
        .unwrap()
        .signed_by(&key, &ca)
        .unwrap();
    Pki {
        ca: ca.der().clone(),
        ca_pem: ca.pem(),
        server: server.der().clone(),
        key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    }
}

/// A TLS listener on `127.0.0.1:0` that presents the server certificate of `pki` and
/// answers each request with 200 and [`OK`].
fn tls_server(pki: &Pki) -> Server {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![pki.server.clone()], pki.key.clone_key())
        .unwrap();
    let config = Arc::new(config);
    let reply = Reply::bytes(answer("200 OK", "", OK));
    Server::start_with(move |stream: TcpStream, stop: &AtomicBool| {
        let connection = ServerConnection::new(Arc::clone(&config)).ok()?;
        let mut tls = StreamOwned::new(connection, stream);
        // A failed handshake ends the read: the client refused the certificate.
        let head = read_head(&mut tls).ok().filter(|head| !head.is_empty())?;
        reply_on(&mut tls, &reply, stop);
        Some(head)
    })
}

/// A policy for `https://127.0.0.1:<port>`, with the secret `token` for that origin.
fn tls_policy(port: u16) -> Arc<NetworkPolicy> {
    let item = AllowItem::parse(&format!("https://127.0.0.1:{port}")).unwrap();
    let value = SecretValue::new("planted-value".into()).unwrap();
    let secret = Secret::new("token", value, item.origin()).unwrap();
    Arc::new(
        NetworkPolicy::new(vec![item])
            .unwrap()
            .with_secret(secret)
            .unwrap(),
    )
}

/// Send one GET to `server` with `roots` as the host roots.
fn send_with_roots(server: &Server, roots: &[CertificateDer<'_>]) -> mds::Response {
    let port = server.port();
    let policy = tls_policy(port);
    let request = request(
        &entry(&format!("url: 'https://127.0.0.1:{port}/x'")),
        &policy,
    );
    Fetcher::new(policy, roots).unwrap().send(&request, soon())
}

/// TP-98: a server whose certificate chains to a CA that the sender does not know fails
/// with the TLS failure, and no request reaches it. Control: with the CA as a host
/// root, the body arrives.
#[test]
fn an_unknown_root_fails_and_a_host_root_passes() {
    let pki = pki("127.0.0.1");
    let server = tls_server(&pki);
    let response = send_with_roots(&server, &[]);
    assert_eq!(failure(&response), Some(TransportFailure::Tls));
    let seen: Seen = server.stop();
    assert_eq!((seen.connections, seen.heads.len()), (1, 0));

    let server = tls_server(&pki);
    let response = send_with_roots(&server, std::slice::from_ref(&pki.ca));
    assert_eq!(http(&response), Some((200, OK)));
    assert_eq!(server.stop().heads.len(), 1);
}

/// A certificate for another address fails the host check, also with its CA as a host
/// root. Control: the certificate for the request address passes.
#[test]
fn a_certificate_for_another_address_fails() {
    for (server_ip, passes) in [("127.0.0.2", false), ("127.0.0.1", true)] {
        let pki = pki(server_ip);
        let server = tls_server(&pki);
        let response = send_with_roots(&server, std::slice::from_ref(&pki.ca));
        if passes {
            assert_eq!(http(&response), Some((200, OK)));
        } else {
            assert_eq!(failure(&response), Some(TransportFailure::Tls));
        }
        server.stop();
    }
}

/// A secret header sends its prefix and then the value of the host secret for the
/// request origin, over TLS only.
#[test]
fn a_secret_header_sends_the_prefix_and_the_value() {
    let pki = pki("127.0.0.1");
    let server = tls_server(&pki);
    let port = server.port();
    let policy = tls_policy(port);
    let fields = format!(
        "url: 'https://127.0.0.1:{port}/x', \
         headers: {{Authorization: {{secret: token, prefix: 'Bearer '}}}}"
    );
    let request = request(&entry(&fields), &policy);
    assert!(!format!("{request:?}").contains("planted-value"));
    let response = Fetcher::new(policy, std::slice::from_ref(&pki.ca))
        .unwrap()
        .send(&request, soon());
    assert_eq!(http(&response), Some((200, OK)));
    let head = server.stop().heads.remove(0).to_ascii_lowercase();
    assert!(
        head.contains("\r\nauthorization: bearer planted-value\r\n"),
        "{head}"
    );
}

/// A sender with a policy that has no secret for the request origin sends nothing for
/// a secret header. Control: the policy of the step compile sends it.
#[test]
fn a_secret_header_that_does_not_bind_sends_nothing() {
    let pki = pki("127.0.0.1");
    let server = tls_server(&pki);
    let port = server.port();
    let policy = tls_policy(port);
    let fields = format!(
        "url: 'https://127.0.0.1:{port}/x', \
         headers: {{Authorization: {{secret: token, prefix: 'Bearer '}}}}"
    );
    let request = request(&entry(&fields), &policy);
    let other = Arc::new(
        NetworkPolicy::new(vec![
            AllowItem::parse(&format!("https://127.0.0.1:{port}")).unwrap()
        ])
        .unwrap(),
    );
    let response = Fetcher::new(other, std::slice::from_ref(&pki.ca))
        .unwrap()
        .send(&request, soon());
    assert_eq!(failure(&response), Some(TransportFailure::Connect));
    let response = Fetcher::new(policy, std::slice::from_ref(&pki.ca))
        .unwrap()
        .send(&request, soon());
    assert_eq!(http(&response), Some((200, OK)));
    assert_eq!(server.stop().connections, 1);
}

/// A host root must be a valid trust anchor, and a root file must be PEM with at least
/// one certificate. Control: the PEM of the test CA gives that one root.
#[test]
fn host_roots_are_checked() {
    let policy = tls_policy(443);
    let bad = CertificateDer::from(vec![1, 2, 3]);
    let error = Fetcher::new(Arc::clone(&policy), &[bad]).unwrap_err();
    assert_eq!(error, SetupError::Root { index: 0 });
    assert_eq!(
        error.to_string(),
        "root 0 of the host is not a valid trust anchor"
    );
    assert_eq!(
        parse_pem_roots(b"no roots here\n"),
        Err(SetupError::NoRoots)
    );
    let broken = b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
    assert_eq!(parse_pem_roots(broken), Err(SetupError::RootFile));

    let pki = pki("127.0.0.1");
    let roots = parse_pem_roots(pki.ca_pem.as_bytes()).unwrap();
    assert_eq!(roots, vec![pki.ca.clone()]);
    assert!(Fetcher::new(policy, &roots).is_ok());
}
