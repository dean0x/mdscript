//! End-to-end tests of the network of `mds build` (#405): `--allow-net`, the `mds.json`
//! `network` section that can only narrow it, the import opt-in, host secrets, host
//! roots, the proxy variables, and the commands that never send.
//!
//! Every listener is a thread on `127.0.0.1:0` in this process. Each check that a
//! listener got no connection has a control in the same test that shows the listener
//! taking a connection first.

mod common;
use common::{mds_bin, ChildGuard};

use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, MAIN_SEPARATOR_STR};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// The poll interval of a listener thread.
const POLL: Duration = Duration::from_millis(5);
/// The longest life of a listener thread.
const LIFE: Duration = Duration::from_secs(60);
/// The longest wait for a command that must stop at its argument parse.
const PARSE_WAIT: Duration = Duration::from_secs(10);
/// The longest request head that a listener reads.
const MAX_HEAD: usize = 64 * 1024;
/// The body of a plain answer, and the output of `{{u.n}}` over it.
const BODY: &str = r#"{"n": 1}"#;
const RENDERED: &str = "1\n";
/// The secret value of the secret tests. It must never reach stderr.
const SECRET: &str = "planted-secret-405";
/// The environment variable that holds [`SECRET`] for the child.
const SECRET_VAR: &str = "MDS_TEST_NET_TOKEN";

// ── Listeners ────────────────────────────────────────────────────────────────

/// What a listener saw.
#[derive(Debug, Default)]
struct Seen {
    connections: usize,
    heads: Vec<String>,
}

/// A listener thread on `127.0.0.1:0`. `serve` handles each connection and gives the
/// request head that it read.
struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
    thread: JoinHandle<()>,
}

impl Server {
    fn start(serve: impl Fn(TcpStream) -> Option<String> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (flag, record) = (Arc::clone(&stop), Arc::clone(&seen));
        let thread = thread::spawn(move || {
            let end = Instant::now() + LIFE;
            // Accept until a stop, then take every connection still in the backlog.
            while Instant::now() < end {
                match listener.accept() {
                    Ok((stream, _)) => {
                        record.lock().unwrap().connections += 1;
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        if let Some(head) = serve(stream) {
                            record.lock().unwrap().heads.push(head);
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        if flag.load(Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(POLL);
                    }
                    Err(e) => panic!("accept failed: {e}"),
                }
            }
        });
        Self {
            port,
            stop,
            seen,
            thread,
        }
    }

    /// A plain HTTP listener: `reply` gives the answer to a request head, or `None` to
    /// close with no answer.
    fn plain(reply: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> Self {
        Self::start(move |mut stream| {
            let head = read_head(&mut stream).ok()?;
            if let Some(bytes) = reply(&head) {
                let _ = stream.write_all(&bytes);
                let _ = stream.flush();
            }
            Some(head)
        })
    }

    /// A plain listener that answers every request with 200 and [`BODY`].
    fn ok() -> Self {
        Self::plain(|_| Some(answer("200 OK", "", BODY)))
    }

    /// A TLS listener that presents the server certificate of `pki`.
    fn tls(pki: &Pki, reply: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![pki.server.clone()], pki.key.clone_key())
            .unwrap();
        let config = Arc::new(config);
        Self::start(move |stream| {
            let connection = ServerConnection::new(Arc::clone(&config)).ok()?;
            let mut tls = StreamOwned::new(connection, stream);
            // A failed handshake ends the read: the client refused the certificate.
            let head = read_head(&mut tls).ok().filter(|head| !head.is_empty())?;
            if let Some(bytes) = reply(&head) {
                let _ = tls.write_all(&bytes);
            }
            tls.conn.send_close_notify();
            let _ = tls.flush();
            Some(head)
        })
    }

    /// The connections until now.
    fn connections(&self) -> usize {
        self.seen.lock().unwrap().connections
    }

    /// Stop the listener and give what it saw. It takes every connection that completed
    /// before the stop, so a count with no new connection means that none came.
    fn stop(self) -> Seen {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap();
        Arc::try_unwrap(self.seen).unwrap().into_inner().unwrap()
    }
}

/// Read a request head: the bytes up to the first blank line, at most [`MAX_HEAD`].
fn read_head(stream: &mut impl Read) -> io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < MAX_HEAD && !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// An HTTP/1.1 answer with `status`, the extra header lines `headers` and `body`.
fn answer(status: &str, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// The value of the header `name` in a request head, any letter case.
fn header_value<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// A test CA, its PEM text, and a server certificate and key that the CA signed.
struct Pki {
    ca_pem: String,
    server: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

/// A CA and a certificate for `127.0.0.1`, made now: the repository holds no key.
fn pki() -> Pki {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let server = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .signed_by(&key, &ca)
        .unwrap();
    Pki {
        ca_pem: ca.pem(),
        server: server.der().clone(),
        key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    }
}

// ── Projects and runs ────────────────────────────────────────────────────────

/// A project directory (a `.mdsroot` marker) that holds `files`.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".mdsroot"), "").unwrap();
    for (name, text) in files {
        fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

/// A module with one request-map entry `u` for `url`, that renders `{{u.n}}`.
fn entry_module(url: &str) -> String {
    format!("---\ndata:\n  u:\n    url: \"{url}\"\n---\n{{{{u.n}}}}\n")
}

/// The allow item of a plain listener.
fn http_item(server: &Server) -> String {
    format!("http://127.0.0.1:{}", server.port)
}

/// A plain URL at a listener.
fn http_url(server: &Server, path: &str) -> String {
    format!("http://127.0.0.1:{}{path}", server.port)
}

/// `mds <args>` in `dir`, with no stdin and no proxy variable.
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = mds_bin();
    cmd.current_dir(dir).args(args).stdin(Stdio::null());
    for var in PROXY_VARS {
        cmd.env_remove(var);
    }
    cmd
}

fn run(dir: &Path, args: &[&str]) -> Output {
    command(dir, args).output().unwrap()
}

/// `mds build main.mds -o - --allow-net <items>` in `dir`, with `extra` args.
fn build(dir: &Path, items: &[&str], extra: &[&str]) -> Output {
    let allow = items.join(",");
    let mut args = vec!["build", "main.mds", "-o", "-", "--allow-net", &allow];
    args.extend_from_slice(extra);
    run(dir, &args)
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The exit code and the stdout of `out`.
fn code_and_stdout(out: &Output) -> (Option<i32>, String) {
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// `text` without whitespace or miette's frame marker, so that a message miette wrapped
/// compares equal to the unwrapped one.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{2502}')
        .collect()
}

/// Assert that `out` is a success that printed [`RENDERED`].
fn assert_rendered(out: &Output, what: &str) {
    assert_eq!(
        code_and_stdout(out),
        (Some(0), RENDERED.to_owned()),
        "{what}; stderr: {}",
        stderr_of(out)
    );
}

/// Assert that `out` failed with exit 2, `code` and a message that holds `text`.
fn assert_fails(out: &Output, code: &str, text: &str) {
    let stderr = stderr_of(out);
    assert_eq!(code_and_stdout(out), (Some(2), String::new()), "{stderr}");
    assert!(stderr.contains(code), "expected {code}: {stderr}");
    assert!(
        squash(&stderr).contains(&squash(text)),
        "expected {text:?}: {stderr}"
    );
}

/// Assert that neither stream of `out` holds the planted secret value. The messages
/// are fixed text, so a failure does not show the value.
fn assert_no_secret(out: &Output) {
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(SECRET),
        "stdout must not show the secret value"
    );
    assert!(
        !stderr_of(out).contains(SECRET),
        "stderr must not show the secret value"
    );
}

/// The proxy variables of common HTTP clients, in both letter cases.
const PROXY_VARS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

// ── TP-84, TP-85, TP-99: the flag and mds.json ──────────────────────────────

/// TP-84: with no `--allow-net`, an `mds.json` that allows the listener opens nothing:
/// the build fails with `mds::network_denied`, exit 2, and the listener gets no
/// connection. Control: `--allow-net` with the same origin sends once.
#[test]
fn no_flag_opens_no_connection_whatever_mds_json_allows() {
    let server = Server::ok();
    let config = format!(
        "{{\"network\": {{\"allow\": [\"{}\"]}}}}",
        http_item(&server)
    );
    let dir = project(&[
        ("main.mds", &entry_module(&http_url(&server, "/u"))),
        ("mds.json", &config),
    ]);

    assert_rendered(&build(dir.path(), &[&http_item(&server)], &[]), "control");
    assert_eq!(server.connections(), 1, "control: the flag sends once");

    let out = run(dir.path(), &["build", "main.mds", "-o", "-"]);
    assert_fails(
        &out,
        "mds::network_denied",
        "network access denied for data entries: \"main.mds\", entry \"u\"",
    );
    assert_eq!(server.stop().connections, 1, "no flag: no connection");
}

/// TP-85: an `mds.json` allow list wider than `--allow-net` fails with
/// `mds::invalid_options` and names the item, and `allow_imports: true` without
/// `--allow-net-imports` fails and names the key; a narrower list refuses the origin
/// that it drops. Each sends nothing. Control: equal lists send once.
#[test]
fn mds_json_can_only_narrow_the_flag() {
    let target = Server::ok();
    let other = Server::ok();
    let items = [http_item(&other), http_item(&target)];
    let flags = [items[0].as_str(), items[1].as_str()];
    let dir = project(&[("main.mds", &entry_module(&http_url(&target, "/u")))]);
    let with_config = |config: &str| {
        fs::write(dir.path().join("mds.json"), config).unwrap();
        build(dir.path(), &flags, &[])
    };

    let equal = format!(
        "{{\"network\": {{\"allow\": [\"{}\", \"{}\"]}}}}",
        items[0], items[1]
    );
    assert_rendered(&with_config(&equal), "control: equal lists");
    assert_eq!(target.connections(), 1, "control: equal lists send once");

    let wider = format!(
        "{{\"network\": {{\"allow\": [\"{}\", \"{}\", \"http://127.0.0.1:9\"]}}}}",
        items[0], items[1]
    );
    assert_fails(
        &with_config(&wider),
        "mds::invalid_options",
        &format!(
            ".{MAIN_SEPARATOR_STR}mds.json network.allow[2]: the item permits a request \
             that the --allow-net list refuses"
        ),
    );
    let imports = format!(
        "{{\"network\": {{\"allow\": [\"{}\"], \"allow_imports\": true}}}}",
        items[1]
    );
    assert_fails(
        &with_config(&imports),
        "mds::invalid_options",
        &format!(
            ".{MAIN_SEPARATOR_STR}mds.json network.allow_imports: true needs --allow-net-imports"
        ),
    );
    let narrower = format!("{{\"network\": {{\"allow\": [\"{}\"]}}}}", items[0]);
    assert_fails(
        &with_config(&narrower),
        "mds::network_denied",
        &format!(
            "\"main.mds\", entry \"u\", URL \"{}\", origin \"{}\": allow-list: the origin is not in the allow list",
            http_url(&target, "/u"),
            items[1]
        ),
    );
    assert_eq!(target.stop().connections, 1, "no refused run sends");
    assert_eq!(other.stop().connections, 0);
}

/// TP-99: an unknown key in the `network` section fails config loading and sends
/// nothing. Control: without the section, the request is sent.
#[test]
fn an_unknown_network_key_fails_and_sends_nothing() {
    let server = Server::ok();
    let dir = project(&[("main.mds", &entry_module(&http_url(&server, "/u")))]);
    let item = http_item(&server);

    assert_rendered(&build(dir.path(), &[&item], &[]), "control: no section");
    assert_eq!(server.connections(), 1);

    let config = format!("{{\"network\": {{\"allow\": [\"{item}\"], \"allow_all\": true}}}}");
    fs::write(dir.path().join("mds.json"), config).unwrap();
    let out = build(dir.path(), &[&item], &[]);
    let stderr = stderr_of(&out);
    assert_eq!(code_and_stdout(&out), (Some(1), String::new()), "{stderr}");
    assert!(
        stderr.contains("invalid mds.json") && stderr.contains("unknown field `allow_all`"),
        "{stderr}"
    );
    assert_eq!(
        server.stop().connections,
        1,
        "the refused config sends nothing"
    );
}

// ── TP-87, TP-88, TP-95..TP-98: requests ────────────────────────────────────

/// TP-87: a redirect to an origin that is not in the allow list fails with
/// `mds::network_denied` (`redirect-check`), and its target gets no connection.
/// Control: with the target in the list, the redirect is followed once.
#[test]
fn a_redirect_to_an_unlisted_origin_fails_with_no_connection() {
    let target = Server::ok();
    let location = http_url(&target, "/to");
    let start = Server::plain(move |_| {
        Some(answer(
            "302 Found",
            &format!("Location: {location}\r\n"),
            "",
        ))
    });
    let dir = project(&[("main.mds", &entry_module(&http_url(&start, "/from")))]);

    assert_rendered(
        &build(dir.path(), &[&http_item(&start), &http_item(&target)], &[]),
        "control: a listed target",
    );
    assert_eq!(
        target.connections(),
        1,
        "control: the redirect is followed once"
    );

    assert_fails(
        &build(dir.path(), &[&http_item(&start)], &[]),
        "mds::network_denied",
        "redirect-check: the redirect target is not in the allow list",
    );
    assert_eq!(
        target.stop().connections,
        1,
        "the unlisted target gets nothing"
    );
    assert_eq!(start.stop().connections, 2);
}

/// The source of the secret tests: `a` sends the host secret `TOKEN` to the TLS
/// listener, and `b` puts a value of its response in the URL of the second listener.
fn secret_chain(tls_port: u16, path: &str, next: &Server) -> String {
    format!(
        "---\ndata:\n  a:\n    url: \"https://127.0.0.1:{tls_port}{path}\"\n    headers:\n      Authorization: {{secret: TOKEN, prefix: \"Bearer \"}}\n  b:\n    url: \"{}/{{{{a.x}}}}\"\n---\n{{{{b.n}}}}\n",
        http_url(next, "/b")
    )
}

/// TP-88, TP-98: a listener that echoes the secret header fails the response check
/// (`mds::network_denied`), the second listener of the chain gets no connection, and
/// stderr holds no secret value. A TLS listener with a CA that the sender does not know
/// fails with `mds::network`. Controls: with `--tls-root`, a fixed answer reaches the
/// second listener once, and the bound listener got the secret.
#[test]
fn an_echoed_secret_stops_the_chain_and_tls_needs_the_host_root() {
    let pki = pki();
    let tls = Server::tls(&pki, |head| {
        let x = if head.starts_with("GET /echo ") {
            header_value(head, "authorization").unwrap_or("").to_owned()
        } else {
            "fixed".to_owned()
        };
        Some(answer("200 OK", "", &format!("{{\"x\": \"{x}\"}}")))
    });
    let next = Server::ok();
    let tls_origin = format!("https://127.0.0.1:{}", tls.port);
    let dir = project(&[
        ("echo.mds", &secret_chain(tls.port, "/echo", &next)),
        ("fixed.mds", &secret_chain(tls.port, "/fixed", &next)),
        ("ca.pem", &pki.ca_pem),
    ]);
    let allow = format!("{tls_origin},{}", http_item(&next));
    let secret = format!("TOKEN=env:{SECRET_VAR}@{tls_origin}");
    let send = |source: &str, roots: bool| {
        let mut args = vec!["build", source, "-o", "-", "--allow-net", &allow];
        args.extend_from_slice(&["--secret", &secret]);
        if roots {
            args.extend_from_slice(&["--tls-root", "ca.pem"]);
        }
        command(dir.path(), &args)
            .env(SECRET_VAR, SECRET)
            .output()
            .unwrap()
    };

    // Check for the value first in each run, with a fixed message, so that no failure
    // message below can show it.
    let out = send("fixed.mds", true);
    assert_no_secret(&out);
    assert_rendered(&out, "control: a fixed answer");
    assert_eq!(
        next.connections(),
        1,
        "control: the chain reaches the second listener"
    );

    let out = send("echo.mds", true);
    assert_no_secret(&out);
    assert_fails(
        &out,
        "mds::network_denied",
        "response-check: the response holds the value of a host secret",
    );

    let out = send("fixed.mds", false);
    assert_no_secret(&out);
    assert_fails(&out, "mds::network", "the TLS handshake failed");

    assert_eq!(
        next.stop().connections,
        1,
        "no refused run reaches the second listener"
    );
    // Each head holds the secret header, so these messages are fixed text.
    let seen = tls.stop();
    let bound = format!("Bearer {SECRET}");
    assert_eq!(
        seen.heads.len(),
        2,
        "the bound listener must get two requests"
    );
    assert!(
        seen.heads
            .iter()
            .all(|head| header_value(head, "authorization") == Some(bound.as_str())),
        "the bound listener must get the secret header"
    );
}

/// TP-95: an origin that is not in the allow list fails with `mds::network_denied`
/// (`allow-list`), exit 2, and sends nothing; a listed listener that closes with no
/// answer fails with `mds::network`, exit 2. Control: the listed origin sends once.
#[test]
fn an_unlisted_origin_is_denied_and_no_answer_is_a_network_failure() {
    let server = Server::ok();
    let silent = Server::plain(|_| None);
    let dir = project(&[("main.mds", &entry_module(&http_url(&server, "/u")))]);

    assert_rendered(&build(dir.path(), &[&http_item(&server)], &[]), "control");
    assert_eq!(server.connections(), 1);
    assert_fails(
        &build(dir.path(), &[&http_item(&silent)], &[]),
        "mds::network_denied",
        "allow-list: the origin is not in the allow list",
    );
    assert_eq!(
        server.stop().connections,
        1,
        "the unlisted origin gets nothing"
    );

    fs::write(
        dir.path().join("main.mds"),
        entry_module(&http_url(&silent, "/u")),
    )
    .unwrap();
    let out = build(dir.path(), &[&http_item(&silent)], &[]);
    let stderr = stderr_of(&out);
    assert_eq!(code_and_stdout(&out), (Some(2), String::new()), "{stderr}");
    assert!(stderr.contains("mds::network"), "{stderr}");
    assert!(!stderr.contains("mds::network_denied"), "{stderr}");
    assert_eq!(silent.stop().connections, 1);
}

/// TP-96: a response value in the URL of the next request, which gets 404, is absent
/// from stderr. Control: stderr names the URL template, and the listener got the value.
#[test]
fn a_response_value_in_a_failed_url_never_reaches_stderr() {
    const MARKER: &str = "marker-from-response-405";
    let first = Server::plain(|_| Some(answer("200 OK", "", &format!("{{\"x\": \"{MARKER}\"}}"))));
    let second = Server::plain(|_| Some(answer("404 Not Found", "", "")));
    let template = format!("{}/{{{{a.x}}}}", http_url(&second, "/b"));
    let source = format!(
        "---\ndata:\n  a:\n    url: \"{}\"\n  b:\n    url: \"{template}\"\n---\n{{{{b.n}}}}\n",
        http_url(&first, "/a")
    );
    let dir = project(&[("main.mds", &source)]);

    let out = build(dir.path(), &[&http_item(&first), &http_item(&second)], &[]);
    // Check for the value first, with fixed messages, so that no failure message below
    // can show it. The request head holds the value, so its messages are fixed too.
    assert!(
        !stderr_of(&out).contains(MARKER),
        "stderr must not show the response value"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(MARKER),
        "stdout must not show the response value"
    );
    assert_fails(&out, "mds::network", &format!("URL \"{template}\""));
    assert!(
        squash(&stderr_of(&out)).contains("theserveransweredwithHTTPstatus404"),
        "stderr must name the status of the answer"
    );
    let seen = second.stop();
    assert_eq!(
        seen.heads.len(),
        1,
        "the second listener must get one request"
    );
    assert!(
        seen.heads[0].starts_with(&format!("GET /b/{MARKER} ")),
        "control: the second listener must get the response value"
    );
    first.stop();
}

/// TP-97: a URL entry of an imported module fails without `--allow-net-imports`
/// (`import-opt-in`) and sends nothing. Control: with the opt-in it sends once.
#[test]
fn an_imported_url_entry_needs_the_import_opt_in() {
    let server = Server::ok();
    let dir = project(&[
        ("main.mds", "@import \"./lib.mds\" as lib\n@include lib\n"),
        ("lib.mds", &entry_module(&http_url(&server, "/lib"))),
    ]);
    let item = http_item(&server);

    assert_rendered(
        &build(dir.path(), &[&item], &["--allow-net-imports"]),
        "control: the opt-in",
    );
    assert_eq!(server.connections(), 1);
    assert_fails(
        &build(dir.path(), &[&item], &[]),
        "mds::network_denied",
        "import-opt-in: a network entry of an imported module needs the import opt-in",
    );
    assert_eq!(server.stop().connections, 1, "no opt-in: no connection");
}

/// The proxy variables of the child change no target: the request goes to the listed
/// listener, and the proxy listener gets no connection. Test-side control: a connection
/// to the proxy listener is seen.
#[test]
fn proxy_variables_change_no_target() {
    let target = Server::ok();
    let proxy = Server::plain(|_| Some(answer("200 OK", "", r#"{"n": 2}"#)));
    drop(TcpStream::connect(("127.0.0.1", proxy.port)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while proxy.connections() == 0 && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    assert_eq!(
        proxy.connections(),
        1,
        "control: the proxy listener sees a connection"
    );

    let dir = project(&[("main.mds", &entry_module(&http_url(&target, "/u")))]);
    let allow = http_item(&target);
    let mut cmd = command(
        dir.path(),
        &["build", "main.mds", "-o", "-", "--allow-net", &allow],
    );
    let proxy_url = http_url(&proxy, "");
    for var in PROXY_VARS {
        if !var.eq_ignore_ascii_case("NO_PROXY") {
            cmd.env(var, &proxy_url);
        }
    }
    assert_rendered(&cmd.output().unwrap(), "the target answers");
    assert_eq!(target.stop().connections, 1);
    assert_eq!(proxy.stop().connections, 1, "the proxy gets no request");
}

/// A stdin build and a directory build send too: stdin through the string step
/// compile, a directory build once per file, each file pinned to its walk entry.
#[test]
fn stdin_and_directory_builds_send() {
    let server = Server::ok();
    let module = entry_module(&http_url(&server, "/u"));
    let dir = project(&[("a.mds", &module), ("b.mds", &module)]);
    let item = http_item(&server);

    let mut child = command(dir.path(), &["build", "-", "--allow-net", &item])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(module.as_bytes())
        .unwrap();
    assert_rendered(&child.wait_with_output().unwrap(), "stdin");
    assert_eq!(server.connections(), 1);

    let out = run(dir.path(), &["build", ".", "--allow-net", &item]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    for name in ["a.md", "b.md"] {
        assert_eq!(fs::read_to_string(dir.path().join(name)).unwrap(), RENDERED);
    }
    assert_eq!(server.stop().connections, 3);
}

// ── Commands that never send ─────────────────────────────────────────────────

/// `mds check`, `mds lint` and `mds fmt --check` open no connection, and the parser
/// refuses `--allow-net` on them and on `mds watch`. The other network flags need
/// `--allow-net`. Control: `mds build --allow-net` sends once.
#[test]
fn tooling_never_sends_and_refuses_the_network_flags() {
    let server = Server::ok();
    let dir = project(&[("main.mds", &entry_module(&http_url(&server, "/u")))]);
    let item = http_item(&server);

    assert_rendered(&build(dir.path(), &[&item], &[]), "control");
    assert_eq!(server.connections(), 1);

    for args in [
        &["check", "main.mds"][..],
        &["lint", "main.mds"],
        &["fmt", "--check", "main.mds"],
    ] {
        let out = run(dir.path(), args);
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "{args:?}: {}",
            stderr_of(&out)
        );
        let mut flagged = args.to_vec();
        flagged.extend_from_slice(&["--allow-net", &item]);
        let out = run(dir.path(), &flagged);
        assert_eq!(out.status.code(), Some(2), "{flagged:?}");
        assert!(
            stderr_of(&out).contains("unexpected argument '--allow-net'"),
            "{flagged:?}: {}",
            stderr_of(&out)
        );
    }
    // A watch that takes the flag runs until it is stopped, so the wait has a bound.
    let mut watch = ChildGuard(
        command(dir.path(), &["watch", "main.mds", "--allow-net", &item])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let end = Instant::now() + PARSE_WAIT;
    let mut status = None;
    for _ in 0..PARSE_WAIT.as_millis().div_ceil(POLL.as_millis()) {
        status = watch.0.try_wait().unwrap();
        if status.is_some() || Instant::now() >= end {
            break;
        }
        thread::sleep(POLL);
    }
    let Some(status) = status else {
        watch.kill_and_wait();
        panic!("mds watch must refuse --allow-net at parse time");
    };
    let mut stderr = String::new();
    watch
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("unexpected argument '--allow-net'"));

    for flag in [
        &["--allow-net-imports"][..],
        &["--secret", "TOKEN=env:VAR@api.example"],
        &["--tls-root", "ca.pem"],
    ] {
        let mut args = vec!["build", "main.mds", "-o", "-"];
        args.extend_from_slice(flag);
        let out = run(dir.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{flag:?}");
        assert!(
            stderr_of(&out).contains("--allow-net"),
            "{flag:?}: {}",
            stderr_of(&out)
        );
    }
    assert_eq!(server.stop().connections, 1, "no tooling command sends");
}
