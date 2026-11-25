//! The sender uses no proxy. The test runs itself again as a child process with every
//! proxy variable set to a second listener; that listener must see no connection from
//! the sender. Controls: a direct connection to the second listener is seen, and a
//! default ureq agent in the same environment does reach it.

mod common;

use std::env;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use common::{answer, entry, http, policy, request, soon, Reply, Server};
use mds_fetch::Fetcher;

/// The variable that tells the child process its part.
const CHILD: &str = "MDS_FETCH_PROXY_CHILD";
/// The name of this test, for the child process.
const NAME: &str = "proxy_variables_do_not_reach_the_sender";
/// Every proxy variable that ureq reads, in both cases.
const PROXY_VARS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];
const OK: &[u8] = b"{\"n\": 1}";

#[test]
fn proxy_variables_do_not_reach_the_sender() {
    if let Ok(part) = env::var(CHILD) {
        return child(&part);
    }
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let url = format!("http://{}", proxy.local_addr().unwrap());

    // Control: the second listener sees a direct connection.
    drop(TcpStream::connect(proxy.local_addr().unwrap()).unwrap());
    assert_eq!(accepted(&proxy), 1);

    run_child("sender", &url);
    assert_eq!(accepted(&proxy), 0, "the sender connected to the proxy");

    // Control: the proxy variables reach a default ureq agent in the child.
    run_child("default", &url);
    assert_eq!(accepted(&proxy), 1, "the control did not reach the proxy");
}

/// Run this test as a child for `part`, with each proxy variable set to `url` and no
/// proxy exception, and check that the child ran its part.
fn run_child(part: &str, url: &str) {
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, part)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
    for var in PROXY_VARS {
        command.env(var, url);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{part}: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(&format!("child part done: {part}")),
        "{stdout}"
    );
}

/// The connections in the backlog of `listener`, after a short wait for late ones.
fn accepted(listener: &TcpListener) -> usize {
    let mut count = 0;
    let end = Instant::now() + Duration::from_millis(200);
    while Instant::now() < end {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    }
    count
}

/// The part of a child process: the sender sends one request to a direct listener, or
/// a default ureq agent tries one request, which goes to the proxy.
fn child(part: &str) {
    for var in PROXY_VARS {
        assert!(env::var(var).is_ok(), "{var} is not set");
    }
    let server = Server::start(Reply::bytes(answer("200 OK", "", OK)));
    let port = server.port();
    let target = format!("http://127.0.0.1:{port}/x");
    match part {
        "sender" => {
            let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
            let request = request(&entry(&format!("url: '{target}'")), &policy);
            let response = Fetcher::new(policy, &[]).unwrap().send(&request, soon());
            assert_eq!(http(&response), Some((200, OK)));
            assert_eq!(server.stop().connections, 1);
        }
        "default" => {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_millis(500)))
                .build()
                .into();
            // The proxy never answers, so the request fails after it connects there.
            assert!(agent.get(&target).call().is_err());
            assert_eq!(server.stop().connections, 0);
        }
        other => panic!("unknown child part {other}"),
    }
    println!("child part done: {part}");
}
