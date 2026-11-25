//! The plain HTTP sender: the address check of each lookup answer, one lookup per
//! request, no redirect, the deadline, the body and header caps and the fixed request
//! headers. Every listener is a thread on `127.0.0.1:0` in this process.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{
    answer, entry, failure, header, http, policy, request, soon, step, Reply, Scripted, Server,
};
use mds::{FetchState, Step, TransportFailure, MAX_RESPONSE_BYTES};
use mds_fetch::Fetcher;

const OK: &[u8] = b"{\"n\": 1}";

/// A listener that answers each request with 200 and [`OK`].
fn ok_server() -> Server {
    Server::start(Reply::bytes(answer("200 OK", "", OK)))
}

/// A request for `api.test` on the port of `server`, with the allow items of that name
/// and of `127.0.0.1` with the port, so a lookup answer of `127.0.0.1` can pass.
fn named(server: &Server) -> (std::sync::Arc<mds::NetworkPolicy>, mds::Request) {
    let port = server.port();
    let policy = policy(&[
        &format!("http://api.test:{port}"),
        &format!("127.0.0.1:{port}"),
    ]);
    let request = request(&entry(&format!("url: 'http://api.test:{port}/x'")), &policy);
    (policy, request)
}

/// TP-86: a lookup answer in a refused range fails with the address failure and opens no
/// connection: a mapped IPv6 loopback, the IPv6 loopback, an IPv6 link-local and a
/// private IPv4 address. One refused address in an answer refuses the request. Control:
/// the answer `127.0.0.1`, with that exact address and port listed, connects once.
#[test]
fn a_refused_lookup_answer_fails_and_opens_no_connection() {
    for answer in [
        &["::ffff:127.0.0.1"][..],
        &["::1"],
        &["fe80::1"],
        &["10.0.0.1"],
        &["127.0.0.1", "10.0.0.1"],
    ] {
        let server = ok_server();
        let (policy, request) = named(&server);
        let (lookup, calls) = Scripted::new(&[answer]);
        let fetcher = Fetcher::new(policy, &[]).unwrap().with_lookup(lookup);
        let response = fetcher.send(&request, soon());
        assert_eq!(
            failure(&response),
            Some(TransportFailure::AddressRefused),
            "{answer:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{answer:?}");
        assert_eq!(server.stop().connections, 0, "{answer:?}");
    }

    let server = ok_server();
    let (policy, request) = named(&server);
    let (lookup, calls) = Scripted::new(&[&["127.0.0.1"]]);
    let fetcher = Fetcher::new(policy, &[]).unwrap().with_lookup(lookup);
    let response = fetcher.send(&request, soon());
    assert_eq!(http(&response), Some((200, OK)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(server.stop().connections, 1);
}

/// An IP address host needs no lookup: the lookup is not called, and the request
/// connects once to that address.
#[test]
fn an_ip_address_host_needs_no_lookup() {
    let server = ok_server();
    let port = server.port();
    let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
    let request = request(
        &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
        &policy,
    );
    let (lookup, calls) = Scripted::new(&[&["10.0.0.1"]]);
    let fetcher = Fetcher::new(policy, &[]).unwrap().with_lookup(lookup);
    assert_eq!(http(&fetcher.send(&request, soon())), Some((200, OK)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(server.stop().connections, 1);
}

/// TP-87: a lookup that changes its answer is called once per request, and the
/// connection goes to the address that passed the check. The next request looks up
/// again, so the new answer is checked and refused.
#[test]
fn a_changing_lookup_is_called_once_and_the_checked_address_connects() {
    let server = ok_server();
    let (policy, request) = named(&server);
    let (lookup, calls) = Scripted::new(&[&["127.0.0.1"], &["10.0.0.1"]]);
    let fetcher = Fetcher::new(policy, &[]).unwrap().with_lookup(lookup);
    assert_eq!(http(&fetcher.send(&request, soon())), Some((200, OK)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let again = fetcher.send(&request, soon());
    assert_eq!(failure(&again), Some(TransportFailure::AddressRefused));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(server.stop().connections, 1);
}

/// TP-87: a 302 comes back as it is, with its `Location`. The sender sends one request
/// and follows nothing: the core checks each hop.
#[test]
fn a_redirect_comes_back_unfollowed() {
    let location = "Location: http://169.254.169.254/latest\r\n";
    let server = Server::start(Reply::bytes(answer("302 Found", location, b"")));
    let port = server.port();
    let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
    let request = request(
        &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
        &policy,
    );
    let response = Fetcher::new(policy, &[]).unwrap().send(&request, soon());
    assert_eq!(http(&response), Some((302, &b""[..])));
    assert_eq!(
        header(&response, "location"),
        Some(&b"http://169.254.169.254/latest"[..])
    );
    let seen = server.stop();
    assert_eq!(seen.connections, 1);
    assert!(
        seen.heads[0].starts_with("GET /x HTTP/1.1\r\n"),
        "{:?}",
        seen.heads
    );
}

/// A lookup answer with more addresses than the cap fails the request and opens no
/// connection. Control: an answer at the cap connects.
#[test]
fn a_lookup_answer_past_the_cap_fails() {
    for (count, connections) in [
        (mds::MAX_RESOLVED_ADDRS + 1, 0),
        (mds::MAX_RESOLVED_ADDRS, 1),
    ] {
        let server = ok_server();
        let (policy, request) = named(&server);
        let answer = vec!["127.0.0.1"; count];
        let (lookup, _) = Scripted::new(&[&answer]);
        let fetcher = Fetcher::new(policy, &[]).unwrap().with_lookup(lookup);
        let response = fetcher.send(&request, soon());
        if connections == 0 {
            assert_eq!(
                failure(&response),
                Some(TransportFailure::Connect),
                "{count}"
            );
        } else {
            assert_eq!(http(&response), Some((200, OK)), "{count}");
        }
        assert_eq!(server.stop().connections, connections, "{count}");
    }
}

/// TP-95: a listener that never answers gives the timeout at the narrowed deadline, and
/// a deadline that passed sends nothing. Control: the same request with time left
/// connects.
#[test]
fn a_silent_server_times_out_at_the_narrowed_deadline() {
    let server = Server::start(Reply::Silent);
    let port = server.port();
    let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
    let request = request(
        &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
        &policy,
    );
    let fetcher = Fetcher::new(policy, &[])
        .unwrap()
        .with_deadline(Duration::from_millis(300));
    let start = Instant::now();
    let response = fetcher.send(&request, soon());
    let took = start.elapsed();
    assert_eq!(failure(&response), Some(TransportFailure::Timeout));
    assert!(took >= Duration::from_millis(250), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    let past = fetcher.send(&request, Instant::now());
    assert_eq!(failure(&past), Some(TransportFailure::Timeout));
    assert_eq!(server.stop().connections, 1);
}

/// TP-95: a body of exactly the cap passes and a body one byte over fails.
#[test]
fn the_body_cap_passes_at_the_cap_and_fails_one_byte_over() {
    for (size, passes) in [(16, true), (17, false)] {
        let body = vec![b'a'; size];
        let server = Server::start(Reply::bytes(answer("200 OK", "", &body)));
        let port = server.port();
        let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
        let request = request(
            &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
            &policy,
        );
        let response = Fetcher::new(policy, &[])
            .unwrap()
            .with_body_cap(16)
            .send(&request, soon());
        if passes {
            assert_eq!(http(&response), Some((200, body.as_slice())));
        } else {
            assert_eq!(failure(&response), Some(TransportFailure::BodyTooLarge));
        }
        server.stop();
    }
}

/// The sender stops its body read one byte past the cap: a server that sends the cap
/// plus one byte of a longer body and then stalls gives the body failure at once, not
/// the timeout. Control: the same stall at the cap gives the timeout.
#[test]
fn the_body_read_stops_one_byte_past_the_cap() {
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n";
    for (size, expected) in [
        (17, TransportFailure::BodyTooLarge),
        (16, TransportFailure::Timeout),
    ] {
        let mut bytes = head.as_bytes().to_vec();
        bytes.extend(vec![b'a'; size]);
        let server = Server::start(Reply::Stall(std::sync::Arc::new(bytes)));
        let port = server.port();
        let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
        let request = request(
            &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
            &policy,
        );
        let response = Fetcher::new(policy, &[])
            .unwrap()
            .with_body_cap(16)
            .with_deadline(Duration::from_secs(2))
            .send(&request, soon());
        assert_eq!(failure(&response), Some(expected), "{size}");
        server.stop();
    }
}

/// TP-95: the core body cap binds by default, and a wider cap can not open it: a body
/// of exactly 10 MiB passes, and one byte more fails.
#[test]
fn the_core_body_cap_binds_and_can_not_widen() {
    let cap = usize::try_from(MAX_RESPONSE_BYTES).unwrap();
    for (size, passes) in [(cap, true), (cap + 1, false)] {
        let server = Server::start(Reply::bytes(answer("200 OK", "", &vec![b'a'; size])));
        let port = server.port();
        let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
        let request = request(
            &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
            &policy,
        );
        let response = Fetcher::new(policy, &[])
            .unwrap()
            .with_body_cap(u64::MAX)
            .send(&request, soon());
        if passes {
            assert_eq!(http(&response).map(|(_, body)| body.len()), Some(size));
        } else {
            assert_eq!(failure(&response), Some(TransportFailure::BodyTooLarge));
        }
        server.stop();
    }
}

/// Response headers past the header cap fail the request. Control: short headers pass.
#[test]
fn response_headers_past_the_cap_fail() {
    let long = format!("X-Pad: {}\r\n", "a".repeat(mds::MAX_RESPONSE_HEADER_BYTES));
    for (headers, passes) in [(long.as_str(), false), ("X-Pad: a\r\n", true)] {
        let server = Server::start(Reply::bytes(answer("200 OK", headers, OK)));
        let port = server.port();
        let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
        let request = request(
            &entry(&format!("url: 'http://127.0.0.1:{port}/x'")),
            &policy,
        );
        let response = Fetcher::new(policy, &[]).unwrap().send(&request, soon());
        if passes {
            assert_eq!(http(&response), Some((200, OK)));
        } else {
            assert_eq!(failure(&response), Some(TransportFailure::HeadersTooLarge));
        }
        server.stop();
    }
}

/// Each request sends the fixed `User-Agent`, `Accept` and `Accept-Encoding` and the
/// declared headers, and no proxy header.
#[test]
fn a_request_sends_the_fixed_and_the_declared_headers() {
    let server = ok_server();
    let port = server.port();
    let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
    let fields = format!("url: 'http://127.0.0.1:{port}/x?q=1', headers: {{X-Trace: abc}}");
    let request = request(&entry(&fields), &policy);
    assert_eq!(
        http(&Fetcher::new(policy, &[]).unwrap().send(&request, soon())),
        Some((200, OK))
    );
    let head = server.stop().heads.remove(0).to_ascii_lowercase();
    assert!(head.starts_with("get /x?q=1 http/1.1\r\n"), "{head}");
    for line in [
        concat!("user-agent: mds-fetch/", env!("CARGO_PKG_VERSION")),
        "accept: application/json, application/yaml, text/plain",
        "accept-encoding: identity",
        "x-trace: abc",
    ] {
        assert!(head.contains(&format!("\r\n{line}\r\n")), "{line}: {head}");
    }
    assert!(!head.contains("proxy"), "{head}");
}

/// A HEAD request sends HEAD and gives an empty body.
#[test]
fn a_head_request_gives_an_empty_body() {
    let server = Server::start(Reply::bytes(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n".to_vec(),
    ));
    let port = server.port();
    let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
    let fields = format!("url: 'http://127.0.0.1:{port}/x', method: HEAD, envelope: true");
    let request = request(&entry(&fields), &policy);
    let response = Fetcher::new(policy, &[]).unwrap().send(&request, soon());
    assert_eq!(http(&response), Some((200, &b""[..])));
    assert!(server.stop().heads[0].starts_with("HEAD /x HTTP/1.1\r\n"));
}

/// The response of the sender completes the step compile that asked for it.
#[test]
fn a_sent_response_completes_the_step_compile() {
    let server = ok_server();
    let port = server.port();
    let policy = policy(&[&format!("http://127.0.0.1:{port}")]);
    let main = entry(&format!("url: 'http://127.0.0.1:{port}/x'"));
    let ctx = common::context(&policy);
    let Step::Need(requests) = step(&main, &ctx, &FetchState::new()) else {
        panic!("expected requests");
    };
    let fetcher = Fetcher::new(policy, &[]).unwrap();
    let state = requests.iter().fold(FetchState::new(), |state, request| {
        state
            .with_response(request.id(), fetcher.send(request, soon()))
            .unwrap()
    });
    let Step::Done(result) = step(&main, &ctx, &state) else {
        panic!("expected a finished compile");
    };
    assert!(result.into_markdown().unwrap().ends_with("1\n"));
    assert_eq!(server.stop().connections, 1);
}

/// Each run key is new: the identity of a request with a secret header differs between
/// two keys. Control: the same key bytes give the same identity.
#[test]
fn each_run_key_is_new() {
    let origin = mds::AllowItem::parse("api.test").unwrap().origin();
    let value = mds::SecretValue::new("planted-value".into()).unwrap();
    let policy = mds::NetworkPolicy::new(vec![mds::AllowItem::parse("api.test").unwrap()])
        .unwrap()
        .with_secret(mds::Secret::new("token", value, origin).unwrap())
        .unwrap();
    let main = entry(
        "url: 'https://api.test/x', headers: {Authorization: {secret: token, prefix: 'Bearer '}}",
    );
    let id = |key: mds::RunKey| match step(
        &main,
        &mds::FetchContext::new(policy.clone(), key),
        &FetchState::new(),
    ) {
        Step::Need(requests) => requests[0].id(),
        other => panic!("expected requests, got {other:?}"),
    };
    let first = id(mds_fetch::run_key().unwrap());
    let second = id(mds_fetch::run_key().unwrap());
    assert_ne!(first, second);
    assert_eq!(
        id(mds::RunKey::from_bytes([3; 32])),
        id(mds::RunKey::from_bytes([3; 32]))
    );
}
