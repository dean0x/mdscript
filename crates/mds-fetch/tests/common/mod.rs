//! Helpers of the sender tests: requests from the core step API, a listener thread on
//! `127.0.0.1:0` inside the test process, and lookups with fixed answers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mds::{
    AllowItem, CompileOptions, FetchContext, FetchState, NetworkPolicy, Request, Response, RunKey,
    Step, TransportFailure, VirtualFs,
};
use mds_fetch::Lookup;

/// The poll interval of a listener thread.
const POLL: Duration = Duration::from_millis(5);
/// The longest life of a listener thread: its poll loops stop after this time.
const LIFE: Duration = Duration::from_secs(60);
/// The longest request head that a listener reads.
const MAX_HEAD: usize = 64 * 1024;

/// A policy that allows `items`.
pub fn policy(items: &[&str]) -> Arc<NetworkPolicy> {
    let items = items
        .iter()
        .map(|item| AllowItem::parse(item).unwrap())
        .collect();
    Arc::new(NetworkPolicy::new(items).unwrap())
}

/// The source of one entry `item` with the request map `fields`.
pub fn entry(fields: &str) -> String {
    format!("---\ndata:\n  item: {{{fields}}}\n---\n{{{{item.n}}}}\n")
}

/// The context of a step compile under `policy` with a fixed run key.
pub fn context(policy: &NetworkPolicy) -> FetchContext {
    FetchContext::new(policy.clone(), RunKey::from_bytes([7; 32]))
}

/// The result of a step compile of `main` under `ctx` with `state`.
pub fn step(main: &str, ctx: &FetchContext, state: &FetchState) -> Step {
    let fs = VirtualFs::new(HashMap::from([("main.mds".to_owned(), main.to_owned())]));
    mds::compile_step_virtual_fs(fs, "main.mds", None, CompileOptions::default(), ctx, state)
        .unwrap()
}

/// The one request that the first step compile of `main` needs.
pub fn request(main: &str, policy: &NetworkPolicy) -> Request {
    match step(main, &context(policy), &FetchState::new()) {
        Step::Need(mut requests) if requests.len() == 1 => requests.remove(0),
        other => panic!("expected one request, got {other:?}"),
    }
}

/// A deadline far enough for every test request.
pub fn soon() -> Instant {
    Instant::now() + Duration::from_secs(20)
}

/// The failure of a failed response.
pub fn failure(response: &Response) -> Option<TransportFailure> {
    match response {
        Response::Failed { 0: failure, .. } => Some(*failure),
        _ => None,
    }
}

/// The status and the body of an HTTP response.
pub fn http(response: &Response) -> Option<(u16, &[u8])> {
    match response {
        Response::Http { status, body, .. } => Some((*status, body.as_slice())),
        _ => None,
    }
}

/// The value of the response header `name` (lower case).
pub fn header<'a>(response: &'a Response, name: &str) -> Option<&'a [u8]> {
    match response {
        Response::Http { headers, .. } => headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_slice()),
        _ => None,
    }
}

/// An HTTP/1.1 answer with `status`, the extra header lines `headers` and `body`.
pub fn answer(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// What a listener does after it reads a request head.
#[derive(Clone)]
pub enum Reply {
    /// Write these bytes and close.
    Bytes(Arc<Vec<u8>>),
    /// Write these bytes, then write nothing more and keep the connection open until the
    /// listener stops.
    Stall(Arc<Vec<u8>>),
    /// Write nothing until the listener stops.
    Silent,
}

impl Reply {
    pub fn bytes(bytes: Vec<u8>) -> Self {
        Reply::Bytes(Arc::new(bytes))
    }
}

/// What a listener saw.
#[derive(Debug, Default)]
pub struct Seen {
    /// The accepted connections.
    pub connections: usize,
    /// The request heads, as text.
    pub heads: Vec<String>,
}

/// A listener thread on `127.0.0.1:0` that gives each connection the same reply.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Seen>,
}

impl Server {
    /// A plain HTTP listener.
    pub fn start(reply: Reply) -> Self {
        Self::start_with(move |stream, stop| serve_plain(stream, &reply, stop))
    }

    /// A listener that runs `serve` on each connection and keeps the head it gives.
    pub fn start_with<F>(serve: F) -> Self
    where
        F: Fn(TcpStream, &AtomicBool) -> Option<String> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut seen = Seen::default();
            let end = Instant::now() + LIFE;
            // Accept until a stop, then take every connection still in the backlog.
            while Instant::now() < end {
                match listener.accept() {
                    Ok((stream, _)) => {
                        seen.connections += 1;
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        if let Some(head) = serve(stream, &flag) {
                            seen.heads.push(head);
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if flag.load(Ordering::SeqCst) {
                            break;
                        }
                        thread::sleep(POLL);
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
            seen
        });
        Self { addr, stop, thread }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop the listener and give what it saw. The listener takes every connection that
    /// completed before the stop, so a count of zero means that no connection came.
    pub fn stop(self) -> Seen {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap()
    }
}

/// Read a request head: the bytes up to the first blank line, at most [`MAX_HEAD`].
pub fn read_head(stream: &mut impl Read) -> io::Result<String> {
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

/// Give `reply` on `stream` after its head. A stalled or silent reply holds the
/// connection open until the stop.
pub fn reply_on(stream: &mut impl Write, reply: &Reply, stop: &AtomicBool) {
    // A client that stops at its body cap closes first: a write error is expected.
    match reply {
        Reply::Bytes(bytes) => {
            let _ = stream.write_all(bytes);
            let _ = stream.flush();
        }
        Reply::Stall(bytes) => {
            let _ = stream.write_all(bytes);
            let _ = stream.flush();
            wait_for(stop);
        }
        Reply::Silent => wait_for(stop),
    }
}

fn wait_for(stop: &AtomicBool) {
    let end = Instant::now() + LIFE;
    while !stop.load(Ordering::SeqCst) && Instant::now() < end {
        thread::sleep(POLL);
    }
}

fn serve_plain(mut stream: TcpStream, reply: &Reply, stop: &AtomicBool) -> Option<String> {
    let head = read_head(&mut stream).ok()?;
    reply_on(&mut stream, reply, stop);
    Some(head)
}

/// A lookup that gives the IP addresses of answer `n` to call `n`, and the last answer
/// to each later call, each with the asked port. It counts its calls.
pub struct Scripted {
    answers: Vec<Vec<IpAddr>>,
    calls: Arc<AtomicUsize>,
}

impl Scripted {
    pub fn new(answers: &[&[&str]]) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let answers = answers
            .iter()
            .map(|answer| answer.iter().map(|ip| ip.parse().unwrap()).collect())
            .collect();
        let lookup = Self {
            answers,
            calls: Arc::clone(&calls),
        };
        (lookup, calls)
    }
}

impl Lookup for Scripted {
    fn lookup(&self, host: &str, port: u16, _: Duration) -> io::Result<Vec<SocketAddr>> {
        assert_eq!(host, "api.test", "the lookup is for the request host");
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = &self.answers[call.min(self.answers.len() - 1)];
        Ok(answer.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
    }
}
