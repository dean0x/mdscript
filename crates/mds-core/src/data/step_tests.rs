//! The step engine through real compiles: each `run_step` call is one round.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use indexmap::IndexMap;

use super::encode::{check_header_value, encode_path_segment, encode_query, EncodeRefusal};
use super::encode_tests::{vectors, Vector};
use super::policy::{AllowItem, NetworkPolicy, Secret, SecretValue};
use super::request::{HeaderValue, Request, RequestId, RunKey};
use super::step::{FetchState, Outcome, RemoteInput, Response, StepCtx, TransportFailure};
use super::{convert, Accept, DataInput, DataLabel};
use crate::evaluator::budget::MemBudget;
use crate::limits::{
    MAX_DATA_BYTES_PER_COMPILE, MAX_HEADER_VALUE_BYTES, MAX_REDIRECTS, MAX_REQUESTS_PER_COMPILE,
    MAX_RESPONSE_BODY_BYTES_PER_COMPILE, MAX_RESPONSE_BYTES,
};
use crate::resolver::ModuleCache;
use crate::{CompiledOutput, MdsError, Value, VirtualFs};

const PLANTED: &str = "planted-secret-value";

type Responses = IndexMap<RequestId, Response>;
type Runtime = HashMap<String, Value>;

fn policy(items: &[&str]) -> NetworkPolicy {
    let items = items
        .iter()
        .map(|item| AllowItem::parse(item).unwrap())
        .collect();
    NetworkPolicy::new(items).unwrap()
}

fn with_secret(policy: NetworkPolicy, name: &str, value: &str, origin: &str) -> NetworkPolicy {
    let origin = AllowItem::parse(origin).unwrap().origin();
    let value = SecretValue::new(value.into()).unwrap();
    policy
        .with_secret(Secret::new(name, value, origin).unwrap())
        .unwrap()
}

/// One round of a step compile of `main.mds`: the text and the outcome, or the first
/// error, and the requests that the compile needed when it stopped.
fn run_round(
    files: &[(&str, &str)],
    runtime: &Runtime,
    policy: &NetworkPolicy,
    responses: &Responses,
) -> (Result<(String, Outcome), MdsError>, Vec<Request>) {
    let files = files
        .iter()
        .map(|&(key, text)| (key.to_owned(), text.to_owned()))
        .collect();
    let ctx = StepCtx::new(
        Arc::new(policy.clone()),
        Arc::new(RunKey::from_bytes([7; 32])),
        Arc::new(responses.clone()),
    );
    let mut cache = ModuleCache::stepping(Box::new(VirtualFs::new(files)), ctx);
    let compiled = cache.resolve_virtual_intrinsic("main.mds", runtime, &mut Vec::new());
    let pending = cache.has_pending_data();
    let ctx = cache.take_step().unwrap();
    let needed = ctx.needed().cloned().collect();
    let result = compiled.and_then(|output| {
        let CompiledOutput::Markdown(text) = output else {
            panic!("expected Markdown output");
        };
        Ok((text, ctx.finish(pending)?))
    });
    (result, needed)
}

fn run_step(
    files: &[(&str, &str)],
    runtime: &Runtime,
    policy: &NetworkPolicy,
    responses: &Responses,
) -> Result<(String, Outcome), MdsError> {
    run_round(files, runtime, policy, responses).0
}

/// The requests of a round that needs more responses.
fn need(result: Result<(String, Outcome), MdsError>) -> Vec<Request> {
    match result {
        Ok((_, Outcome::Need(requests))) => requests,
        other => panic!("expected requests, got {other:?}"),
    }
}

fn done(result: Result<(String, Outcome), MdsError>) -> String {
    match result {
        Ok((text, Outcome::Done(_))) => text,
        other => panic!("expected a finished compile, got {other:?}"),
    }
}

fn urls(requests: &[Request]) -> Vec<String> {
    requests
        .iter()
        .map(|request| request.url().to_text())
        .collect()
}

/// Run rounds until the compile is done, with `serve` as the host. Return the text,
/// the round count and every request in send order.
fn drive(
    files: &[(&str, &str)],
    policy: &NetworkPolicy,
    serve: impl Fn(&Request) -> Response,
) -> Result<(String, usize, Vec<Request>), MdsError> {
    let mut responses = Responses::new();
    let mut sent = Vec::new();
    for round in 1..=MAX_REQUESTS_PER_COMPILE + 1 {
        match run_step(files, &Runtime::new(), policy, &responses)? {
            (text, Outcome::Done(_)) => return Ok((text, round, sent)),
            (_, Outcome::Need(requests)) => {
                for request in requests {
                    responses.insert(request.id(), serve(&request));
                    sent.push(request);
                }
            }
        }
    }
    panic!("the compile did not end within the round bound");
}

fn ok(body: &str) -> Response {
    Response::Http {
        status: 200,
        headers: Vec::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn moved(location: &str) -> Response {
    Response::Http {
        status: 302,
        headers: vec![("Location".into(), location.as_bytes().to_vec())],
        body: Vec::new(),
    }
}

/// Answer each request with the response of its URL, after the round that needs it.
fn answer(requests: &[Request], responses: &mut Responses, by_url: &[(&str, Response)]) {
    for request in requests {
        let url = request.url().to_text();
        let (_, response) = by_url
            .iter()
            .find(|(known, _)| *known == url)
            .unwrap_or_else(|| panic!("no response for {url}"));
        responses.insert(request.id(), response.clone());
    }
}

fn code(error: &MdsError) -> String {
    error.serialize().code
}

/// A JSON string, which is also a YAML double-quoted scalar. A control character is
/// written as a six-character escape.
fn quoted(text: &str) -> String {
    let mut out = String::from('"');
    for ch in text.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            ch if u32::from(ch) < 0x20 => out.push_str(&format!("{}u{:04x}", '\\', u32::from(ch))),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// What one placement of a value gives: the request text or the error message.
fn observed(result: Result<(String, Outcome), MdsError>, header: bool) -> Result<String, String> {
    match result {
        Ok((_, Outcome::Need(requests))) => {
            let [request] = requests.as_slice() else {
                panic!("expected one request, got {}", requests.len());
            };
            if !header {
                return Ok(request.url().to_text());
            }
            match request.headers()[0].value() {
                HeaderValue::Text(text) => Ok(text.clone()),
                other => panic!("expected a text header, got {other:?}"),
            }
        }
        Ok((_, Outcome::Done(_))) => panic!("expected the request of the target entry"),
        Err(error) => Err(error.to_string()),
    }
}

/// The expected text of each placement of `value`, from the encoders.
fn expected(value: &str) -> [Result<String, String>; 3] {
    let refused = |refusal: EncodeRefusal| {
        Err(format!(
            r#"data error in "main.mds", entry "t", field "{}": {}"#,
            if refusal == EncodeRefusal::HeaderByte {
                "X-V"
            } else {
                "url"
            },
            refusal.reason()
        ))
    };
    [
        match encode_path_segment(value) {
            Ok(bytes) => Ok(format!("https://a.example/items/{bytes}")),
            Err(refusal) => refused(refusal),
        },
        Ok(format!("https://a.example/items?q={}", encode_query(value))),
        match check_header_value(value) {
            Ok(()) => Ok(format!("Bearer {value}")),
            Err(refusal) => refused(refusal),
        },
    ]
}

/// The shared vector table and the encoders agree on each placement of a value, so an
/// expectation from the encoders is the expectation of the table.
fn assert_table_row(row: &Vector, expected: &[Result<String, String>; 3]) {
    let (value, path, query, header) = row;
    match path {
        Ok(bytes) => assert_eq!(expected[0], Ok(format!("https://a.example/items/{bytes}"))),
        Err(refusal) => assert!(
            expected[0]
                .as_ref()
                .unwrap_err()
                .ends_with(refusal.reason()),
            "{value:?}"
        ),
    }
    assert_eq!(
        expected[1],
        Ok(format!("https://a.example/items?q={query}"))
    );
    match header {
        Some(bytes) => assert_eq!(expected[2], Ok(format!("Bearer {bytes}"))),
        None => assert!(
            expected[2]
                .as_ref()
                .unwrap_err()
                .ends_with(EncodeRefusal::HeaderByte.reason()),
            "{value:?}"
        ),
    }
}

/// TP-75: each vector of the encoder table gives the same request bytes, or the same
/// refusal, from a frontmatter variable, a runtime variable, a replayed entry, a data
/// file and a response. A data file and a response change CR LF to LF, so their
/// expectation is the one of the changed value.
#[test]
fn each_value_source_gives_the_same_request_bytes_or_refusal() {
    let targets = [
        "'https://a.example/items/{{v}}'",
        "'https://a.example/items?q={{v}}'",
        "{url: 'https://a.example/h', headers: {X-V: 'Bearer {{v}}'}}",
    ];
    let net = policy(&["a.example"]);
    for (value, path, query, header) in vectors() {
        let raw_expected = expected(&value);
        assert_table_row(&(value.clone(), path, query, header), &raw_expected);
        let data_expected = expected(&value.replace("\r\n", "\n"));
        let json = quoted(&value);
        for (index, target) in targets.iter().enumerate() {
            let header = index == 2;
            let with = |data: &str| format!("---\ndata:\n{data}  t: {target}\n---\n{{{{t}}}}\n");
            let runtime = Runtime::from([("v".to_owned(), Value::String(value.clone()))]);
            let frontmatter = format!("---\nv: {json}\ndata:\n  t: {target}\n---\n{{{{t}}}}\n");
            let replayed = with("  v: https://a.example/v\n");
            let file = with("  v: ./v.json\n");
            let remote = with("  v: https://a.example/v\n");
            let sources = [
                run_step(
                    &[("main.mds", &frontmatter)],
                    &Runtime::new(),
                    &net,
                    &Responses::new(),
                ),
                run_step(
                    &[("main.mds", &with(""))],
                    &runtime,
                    &net,
                    &Responses::new(),
                ),
                run_step(
                    &[("main.mds", &replayed)],
                    &runtime,
                    &net,
                    &Responses::new(),
                ),
                run_step(
                    &[("main.mds", &file), ("v.json", &json)],
                    &Runtime::new(),
                    &net,
                    &Responses::new(),
                ),
            ];
            for (source, result) in sources.into_iter().enumerate() {
                let want = if source == 3 {
                    &data_expected
                } else {
                    &raw_expected
                };
                assert_eq!(
                    observed(result, header),
                    want[index],
                    "{value:?} source {source}"
                );
            }
            let first = need(run_step(
                &[("main.mds", &remote)],
                &Runtime::new(),
                &net,
                &Responses::new(),
            ));
            assert_eq!(urls(&first), ["https://a.example/v"]);
            let mut responses = Responses::new();
            answer(
                &first,
                &mut responses,
                &[("https://a.example/v", ok(&json))],
            );
            let result = run_step(&[("main.mds", &remote)], &Runtime::new(), &net, &responses);
            assert_eq!(
                observed(result, header),
                data_expected[index],
                "{value:?} response"
            );
        }
    }
}

/// TP-77: a two-level chain needs one request per round, and the compile ends in the
/// round after the last level: depth plus one rounds.
#[test]
fn a_two_level_chain_needs_one_round_per_level_and_one_more() {
    let main = "---\ndata:\n  user: https://a.example/user\n  repos: https://a.example/users/{{user.id}}/repos\n---\n{{repos.count}}\n";
    let files = [("main.mds", main)];
    let net = policy(&["a.example"]);
    let first = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
    assert_eq!(urls(&first), ["https://a.example/user"]);
    let serve = |request: &Request| match request.url().path() {
        "/user" => ok(r#"{"id": "u1"}"#),
        "/users/u1/repos" => ok(r#"{"count": 3}"#),
        other => panic!("unexpected path {other}"),
    };
    let (text, rounds, sent) = drive(&files, &net, serve).unwrap();
    assert!(text.ends_with("3\n"), "{text:?}");
    assert_eq!(rounds, 3);
    assert_eq!(
        urls(&sent),
        ["https://a.example/user", "https://a.example/users/u1/repos"]
    );
}

/// TP-78: a response for a request that the compile did not derive fails with fixed
/// text. Control: the exact responses complete the compile.
#[test]
fn a_response_that_the_compile_did_not_request_fails() {
    let net = policy(&["a.example"]);
    let main = "---\ndata:\n  a: https://a.example/a\n---\n{{a.n}}\n";
    let other = "---\ndata:\n  a: https://a.example/other\n---\n{{a.n}}\n";
    let requests = need(run_step(
        &[("main.mds", main)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    let foreign = need(run_step(
        &[("main.mds", other)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    let mut responses = Responses::new();
    answer(
        &requests,
        &mut responses,
        &[("https://a.example/a", ok(r#"{"n": 1}"#))],
    );
    let exact = responses.clone();
    responses.insert(foreign[0].id(), ok(r#"{"n": 2}"#));
    let err = run_step(&[("main.mds", main)], &Runtime::new(), &net, &responses).unwrap_err();
    assert_eq!(code(&err), "mds::invalid_options");
    assert_eq!(
        err.to_string(),
        "invalid options: the fetch state holds a response that this compile did not request"
    );
    let text = done(run_step(
        &[("main.mds", main)],
        &Runtime::new(),
        &net,
        &exact,
    ));
    assert!(text.ends_with("1\n"), "{text:?}");
}

/// TP-79: one request that two modules derive is needed once, and its response binds
/// both entries.
#[test]
fn one_request_of_two_modules_is_needed_once() {
    let main = "---\ndata:\n  a: https://a.example/shared\n---\n@import \"./lib.mds\" as lib\n{{a.n}}\n@include lib\n";
    let lib = "---\ndata:\n  b: https://a.example/shared\n---\nlib {{b.n}}\n";
    let files = [("main.mds", main), ("lib.mds", lib)];
    let net = policy(&["a.example"]).with_imports(true);
    let requests = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
    assert_eq!(urls(&requests), ["https://a.example/shared"]);
    let mut responses = Responses::new();
    answer(
        &requests,
        &mut responses,
        &[("https://a.example/shared", ok(r#"{"n": 5}"#))],
    );
    let Ok((text, Outcome::Done(remote))) = run_step(&files, &Runtime::new(), &net, &responses)
    else {
        panic!("expected a finished compile");
    };
    assert!(text.ends_with("5\nlib 5\n"), "{text:?}");
    let input = |module: &str, entry: &str| RemoteInput {
        module: module.into(),
        entry: entry.into(),
        template: "https://a.example/shared".into(),
        origin: "https://a.example".into(),
    };
    assert_eq!(remote, [input("main.mds", "a"), input("lib.mds", "b")]);
}

/// TP-80: a request that fails a policy check never enters the needed list.
#[test]
fn a_request_that_fails_a_check_never_enters_the_needed_list() {
    let net = policy(&["a.example"]);
    for (bad, layer) in [
        ("{url: 'https://a.example/x', method: POST}", "method"),
        ("'https://b.example/x'", "allow-list"),
        (
            "{url: 'https://a.example/x', headers: {Authorization: {secret: NONE, prefix: 'Bearer '}}}",
            "secret-binding",
        ),
    ] {
        let main = format!("---\ndata:\n  ok: https://a.example/ok\n  bad: {bad}\n---\n{{{{ok}}}}{{{{bad}}}}\n");
        let (result, needed) = run_round(&[("main.mds", &main)], &Runtime::new(), &net, &Responses::new());
        let err = result.unwrap_err();
        assert_eq!(code(&err), "mds::network_denied", "{err}");
        assert!(err.to_string().contains(&format!(": {layer}: ")), "{err}");
        assert_eq!(urls(&needed), ["https://a.example/ok"], "{bad}");
    }
}

/// TP-81: a base URL entry reads the merged scope, so a child frontmatter variable fills
/// its path. The base is another module than the entry, so it needs the import opt-in.
#[test]
fn a_base_url_entry_reads_a_child_frontmatter_variable() {
    let base = "---\ndata:\n  remote: 'https://a.example/users/{{name}}'\n---\nBase.\n";
    let child = "---\nname: child\n---\n@extends \"./base.mds\"\n";
    let files = [("main.mds", child), ("base.mds", base)];
    let net = policy(&["a.example"]);
    let requests = need(run_step(
        &files,
        &Runtime::new(),
        &net.clone().with_imports(true),
        &Responses::new(),
    ));
    assert_eq!(urls(&requests), ["https://a.example/users/child"]);
    let err = run_step(&files, &Runtime::new(), &net, &Responses::new()).unwrap_err();
    assert!(err.to_string().contains(": import-opt-in: "), "{err}");
}

/// TP-82: a response value that holds reference text goes into the next URL as text and
/// is never expanded again. Control: plain text.
#[test]
fn a_response_value_is_not_expanded_again() {
    let main = "---\nx: zzz\ndata:\n  a: https://a.example/a\n  b: https://a.example/b/{{a.id}}\n---\n{{b}}\n";
    let net = policy(&["a.example"]);
    for (id, url) in [
        ("{{x}}", "https://a.example/b/%7B%7Bx%7D%7D"),
        ("abc", "https://a.example/b/abc"),
    ] {
        let first = need(run_step(
            &[("main.mds", main)],
            &Runtime::new(),
            &net,
            &Responses::new(),
        ));
        let mut responses = Responses::new();
        let body = format!("{{\"id\": {}}}", quoted(id));
        answer(
            &first,
            &mut responses,
            &[("https://a.example/a", ok(&body))],
        );
        let next = need(run_step(
            &[("main.mds", main)],
            &Runtime::new(),
            &net,
            &responses,
        ));
        assert_eq!(urls(&next), [url]);
    }
}

/// TP-83: a response body goes through the data file converter, so each refused form
/// fails with the code of a data file. A message names the entry, never the URL.
#[test]
fn a_response_body_fails_as_a_data_file_fails() {
    let backslash = '\\';
    let cases: [(&str, Vec<u8>); 6] = [
        ("json", br#"{"a": 1, "a": 2}"#.to_vec()),
        ("json", format!("\"a{backslash}u0001b\"").into_bytes()),
        ("json", b"9007199254740993".to_vec()),
        ("json", vec![b'"', 0xff, b'"']),
        ("yaml", b"a: 1\n---\nb: 2\n".to_vec()),
        ("json", b"{".to_vec()),
    ];
    let net = policy(&["a.example"]);
    let mut syntax = 0;
    for (accept, body) in cases {
        let main = format!("---\nhidden: secret-path\ndata:\n  a: {{url: 'https://a.example/{{{{hidden}}}}', accept: {accept}}}\n---\n{{{{a}}}}\n");
        let first = need(run_step(
            &[("main.mds", &main)],
            &Runtime::new(),
            &net,
            &Responses::new(),
        ));
        let mut responses = Responses::new();
        responses.insert(
            first[0].id(),
            Response::Http {
                status: 200,
                headers: Vec::new(),
                body: body.clone(),
            },
        );
        let err = run_step(&[("main.mds", &main)], &Runtime::new(), &net, &responses).unwrap_err();
        let format = if accept == "json" {
            Accept::Json
        } else {
            Accept::Yaml
        };
        let label = DataLabel {
            module: "main.mds",
            entry: "a",
            input: DataInput::File("a.json"),
        };
        let file = convert(&body, format, label, &mut MemBudget::default()).unwrap_err();
        assert_eq!(code(&err), code(&file), "{err}");
        let message = err.to_string();
        assert!(
            !message.contains("a.example") && !message.contains("secret-path"),
            "{message}"
        );
        if code(&err) == "mds::yaml" {
            syntax += 1;
            assert!(
                message.contains(r#""main.mds", response for entry "a""#),
                "{message}"
            );
        }
    }
    assert!(syntax > 0, "no case gave a syntax error");
}

/// TP-87: each redirect hop goes through the hop checks, and a chain has at most
/// `MAX_REDIRECTS` hops. Controls: a hop to a listed host gives one next request, and
/// five hops pass.
#[test]
fn each_redirect_hop_passes_the_hop_checks() {
    let main = "---\ndata:\n  a: https://a.example/0\n---\n{{a.n}}\n";
    let net = policy(&["a.example", "b.example"]);
    let first = need(run_step(
        &[("main.mds", main)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    for (location, refused) in [
        (
            "http://169.254.169.254/",
            Some("redirect-check: a redirect from https to http is refused"),
        ),
        (
            "https://c.example/",
            Some("redirect-check: the redirect target is not in the allow list"),
        ),
        ("https://b.example/x", None),
    ] {
        let mut responses = Responses::new();
        answer(
            &first,
            &mut responses,
            &[("https://a.example/0", moved(location))],
        );
        let result = run_step(&[("main.mds", main)], &Runtime::new(), &net, &responses);
        match refused {
            Some(reason) => {
                let err = result.unwrap_err();
                assert_eq!(code(&err), "mds::network_denied");
                assert!(err.to_string().ends_with(reason), "{err}");
            }
            None => assert_eq!(urls(&need(result)), [location]),
        }
    }
    let chain = |hops: usize| {
        let serve = move |request: &Request| {
            let step: usize = request.url().path()[1..].parse().unwrap();
            if step < hops {
                moved(&format!("/{}", step + 1))
            } else {
                ok(r#"{"n": 9}"#)
            }
        };
        drive(&[("main.mds", main)], &net, serve)
    };
    let (text, rounds, _) = chain(MAX_REDIRECTS).unwrap();
    assert!(text.ends_with("9\n"), "{text:?}");
    assert_eq!(rounds, MAX_REDIRECTS + 2);
    let err = chain(MAX_REDIRECTS + 1).unwrap_err();
    assert_eq!(code(&err), "mds::resource_limit");
    assert!(
        err.to_string()
            .ends_with("too many redirects (max 5 per entry)"),
        "{err}"
    );
}

/// A hop to another origin drops the secret header. Control: a hop on the same origin
/// keeps it.
#[test]
fn a_hop_to_another_origin_drops_the_secret_header() {
    let main = "---\ndata:\n  a:\n    url: https://a.example/0\n    headers:\n      Authorization: {secret: TOKEN, prefix: 'Bearer '}\n      X-Plain: kept\n---\n{{a.n}}\n";
    let net = with_secret(
        policy(&["a.example", "b.example"]),
        "TOKEN",
        PLANTED,
        "a.example",
    );
    let first = need(run_step(
        &[("main.mds", main)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    for (location, names) in [
        ("https://b.example/1", vec!["X-Plain"]),
        ("/1", vec!["Authorization", "X-Plain"]),
    ] {
        let mut responses = Responses::new();
        answer(
            &first,
            &mut responses,
            &[("https://a.example/0", moved(location))],
        );
        let next = need(run_step(
            &[("main.mds", main)],
            &Runtime::new(),
            &net,
            &responses,
        ));
        let kept: Vec<_> = next[0]
            .headers()
            .iter()
            .map(|header| header.name())
            .collect();
        assert_eq!(kept, names, "{location}");
    }
}

/// TP-88: a host secret is not a scope name, a runtime variable never fills a secret
/// header, and a response that holds the secret value fails. Control: a fixed body.
#[test]
fn a_host_secret_is_only_a_header_value() {
    let net = with_secret(policy(&["a.example"]), "GITHUB", PLANTED, "a.example");
    let reference = "---\ndata:\n  a: https://a.example/{{GITHUB}}\n---\n{{a}}\n";
    let err = run_step(
        &[("main.mds", reference)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    )
    .unwrap_err();
    assert_eq!(code(&err), "mds::data");
    assert!(
        err.to_string()
            .contains("unknown data reference name \"GITHUB\""),
        "{err}"
    );

    let main = "---\ndata:\n  a:\n    url: https://a.example/x\n    headers:\n      Authorization: {secret: GITHUB, prefix: 'Bearer '}\n---\n{{a.n}}\n";
    let files = [("main.mds", main)];
    let runtime = Runtime::from([("GITHUB".to_owned(), Value::String("runtime-value".into()))]);
    let first = need(run_step(&files, &runtime, &net, &Responses::new()));
    let plain = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
    assert_eq!(first[0].id(), plain[0].id());
    assert_eq!(
        first[0].headers()[0].value(),
        &HeaderValue::Secret {
            name: "GITHUB".into(),
            prefix: "Bearer ".into()
        }
    );
    let err = run_step(&files, &runtime, &policy(&["a.example"]), &Responses::new()).unwrap_err();
    assert!(err.to_string().contains(": secret-binding: "), "{err}");

    let echo_value = format!("{{\"echo\": \"{PLANTED}\"}}");
    let echo_key = format!("{{\"x-{PLANTED}\": 1}}");
    let in_array = format!("{{\"a\": [\"x\", \"{PLANTED}\"]}}");
    let in_top_array = format!("[{{\"b\": \"{PLANTED}\"}}]");
    for (response, refused) in [
        (ok(&echo_value), true),
        (ok(&echo_key), true),
        (moved(&format!("/next?k={PLANTED}")), true),
        (ok(&in_array), true),
        (ok(&in_top_array), true),
        (ok(r#"{"n": 4}"#), false),
        // Control: the same array shapes without the value bind.
        (
            ok(r#"{"n": 4, "a": ["x", "plain"], "c": [{"b": "plain"}]}"#),
            false,
        ),
    ] {
        let mut responses = Responses::new();
        answer(&first, &mut responses, &[("https://a.example/x", response)]);
        let result = run_step(&files, &Runtime::new(), &net, &responses);
        if refused {
            let err = result.unwrap_err();
            assert_eq!(code(&err), "mds::network_denied");
            let message = err.to_string();
            // The value check comes first with a fixed message, so `{message}` below
            // cannot show the value.
            assert!(
                !message.contains(PLANTED),
                "the message must not show the secret value"
            );
            assert!(message.ends_with("response-check: the response holds the value of a host secret (secret \"GITHUB\")"), "{message}");
        } else {
            assert!(done(result).ends_with("4\n"));
        }
    }
}

/// The prefix and the value of a secret header count against `MAX_HEADER_VALUE_BYTES`
/// before the request exists. Control: a header of exactly the cap.
#[test]
fn a_secret_header_value_is_bounded_before_the_request_exists() {
    let main = "---\ndata:\n  a:\n    url: https://a.example/x\n    headers:\n      Authorization: {secret: TOKEN, prefix: 'Bearer '}\n---\n{{a.n}}\n";
    let room = MAX_HEADER_VALUE_BYTES - "Bearer ".len();
    for (length, fits) in [(room, true), (room + 1, false)] {
        let net = with_secret(
            policy(&["a.example"]),
            "TOKEN",
            &"k".repeat(length),
            "a.example",
        );
        let (result, needed) = run_round(
            &[("main.mds", main)],
            &Runtime::new(),
            &net,
            &Responses::new(),
        );
        if fits {
            assert_eq!(need(result).len(), 1);
        } else {
            let err = result.unwrap_err();
            assert_eq!(code(&err), "mds::resource_limit");
            assert_eq!(
                err.to_string(),
                "resource limit exceeded: \"main.mds\", entry \"a\", field \"Authorization\": header value exceeds the maximum length of 8192 bytes"
            );
            assert!(needed.is_empty());
        }
    }
}

/// TP-89: a secret header in an imported module fails, with the import opt-in too.
/// Control: the same header in the entry module.
#[test]
fn a_secret_header_is_allowed_in_the_entry_module_only() {
    let entry = "a:\n    url: https://a.example/x\n    headers:\n      Authorization: {secret: TOKEN, prefix: 'Bearer '}\n";
    let lib = format!("---\ndata:\n  {entry}---\n{{{{a.n}}}}\n");
    let main = "@import \"./lib.mds\" as lib\n@include lib\n";
    let net = with_secret(policy(&["a.example"]), "TOKEN", PLANTED, "a.example").with_imports(true);
    let files = [("main.mds", main), ("lib.mds", lib.as_str())];
    let err = run_step(&files, &Runtime::new(), &net, &Responses::new()).unwrap_err();
    assert_eq!(code(&err), "mds::data");
    assert!(
        err.to_string()
            .ends_with("a secret header is allowed in the entry module only"),
        "{err}"
    );
    let requests = need(run_step(
        &[("main.mds", &lib)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    assert_eq!(urls(&requests), ["https://a.example/x"]);
}

/// A request cannot set a header that the sender writes from the URL, that frames the
/// message or that changes the method, in any letter case and in any value form, so a
/// `Host` header can not name a host that the allow list did not check. The compile fails
/// before any request exists. Controls: a name that only holds one of these names, and the
/// plain request.
#[test]
fn a_request_cannot_set_a_sender_header() {
    let net = with_secret(policy(&["a.example"]), "TOKEN", PLANTED, "a.example");
    let main = |header: &str| {
        format!("---\ndata:\n  a:\n    url: https://a.example/x\n    headers:\n      {header}\n---\n{{{{a.n}}}}\n")
    };
    let runtime = Runtime::from([("v".to_owned(), Value::String("other.example".into()))]);
    for name in [
        "Host",
        "HOST",
        "host",
        "Content-Length",
        "transfer-encoding",
        "Connection",
        "Keep-Alive",
        "Proxy-Connection",
        "TE",
        "Trailer",
        "Upgrade",
        "X-HTTP-Method-Override",
        "x-http-method",
        "X-Method-OVERRIDE",
    ] {
        for value in ["other.example", "'{{v}}'", "{secret: TOKEN, prefix: ''}"] {
            let text = main(&format!("{name}: {value}"));
            let (result, needed) = run_round(
                &[("main.mds", text.as_str())],
                &runtime,
                &net,
                &Responses::new(),
            );
            let err = result.unwrap_err();
            assert_eq!(code(&err), "mds::data", "{name}: {value}");
            let message = err.to_string();
            assert!(
                message.ends_with(&format!(
                    "field \"{name}\": a request cannot set this header"
                )),
                "{message}"
            );
            assert!(!message.contains("other.example"), "{message}");
            assert!(needed.is_empty(), "{name}: {value}");
        }
    }
    for control in [
        "X-Host: other.example",
        "Hosts: other.example",
        "X-Method-Overrides: other.example",
        "X-Plain: kept",
    ] {
        let text = main(control);
        let requests = need(run_step(
            &[("main.mds", text.as_str())],
            &runtime,
            &net,
            &Responses::new(),
        ));
        assert_eq!(urls(&requests), ["https://a.example/x"], "{control}");
        assert_eq!(requests[0].headers().len(), 1, "{control}");
    }
}

/// An `@extends` base is a module other than the entry module, so its URL entry cannot
/// use a secret header, also with the import opt-in. Control: the same base text as the
/// entry module derives its request.
#[test]
fn an_extends_base_cannot_use_a_secret_header() {
    let entry = "a:\n    url: https://a.example/x\n    headers:\n      Authorization: {secret: TOKEN, prefix: 'Bearer '}\n";
    let base = format!("---\ndata:\n  {entry}---\n{{{{a.n}}}}\n");
    let child = "@extends \"./base.mds\"\n";
    let net = with_secret(policy(&["a.example"]), "TOKEN", PLANTED, "a.example").with_imports(true);
    let files = [("main.mds", child), ("base.mds", base.as_str())];
    let (result, needed) = run_round(&files, &Runtime::new(), &net, &Responses::new());
    let err = result.unwrap_err();
    assert_eq!(code(&err), "mds::data");
    assert!(
        err.to_string()
            .ends_with("a secret header is allowed in the entry module only"),
        "{err}"
    );
    assert!(needed.is_empty());
    let requests = need(run_step(
        &[("main.mds", &base)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    assert_eq!(urls(&requests), ["https://a.example/x"]);
}

/// Replay does not lift the secret header rule: a replayed URL entry with a secret
/// header fails in an imported module and in an `@extends` base, and no request exists.
/// Control: the same replayed entry in the entry module renders the runtime value.
#[test]
fn a_replayed_secret_header_is_allowed_in_the_entry_module_only() {
    let entry = "a:\n    url: https://a.example/x\n    headers:\n      Authorization: {secret: TOKEN, prefix: 'Bearer '}\n";
    let module = format!("---\ndata:\n  {entry}---\n{{{{a}}}}\n");
    let runtime = Runtime::from([("a".to_owned(), Value::String("replayed".into()))]);
    let net = with_secret(policy(&["a.example"]), "TOKEN", PLANTED, "a.example").with_imports(true);
    for main in [
        "@import \"./lib.mds\" as lib\n@include lib\n",
        "@extends \"./lib.mds\"\n",
    ] {
        let files = [("main.mds", main), ("lib.mds", module.as_str())];
        let (result, needed) = run_round(&files, &runtime, &net, &Responses::new());
        let err = result.unwrap_err();
        assert_eq!(code(&err), "mds::data", "{main}");
        let message = err.to_string();
        assert!(message.contains("lib.mds"), "{message}");
        assert!(
            message.ends_with("a secret header is allowed in the entry module only"),
            "{message}"
        );
        assert!(needed.is_empty(), "{main}");
    }
    let text = done(run_step(
        &[("main.mds", &module)],
        &runtime,
        &net,
        &Responses::new(),
    ));
    assert!(text.ends_with("replayed\n"), "{text}");
}

/// TP-97: a URL entry of an imported module needs the import opt-in. Control: with it.
#[test]
fn a_url_entry_of_an_imported_module_needs_the_import_opt_in() {
    let lib = "---\ndata:\n  b: https://a.example/lib\n---\n{{b.n}}\n";
    let main = "@import \"./lib.mds\" as lib\n@include lib\n";
    let files = [("main.mds", main), ("lib.mds", lib)];
    let net = policy(&["a.example"]);
    let err = run_step(&files, &Runtime::new(), &net, &Responses::new()).unwrap_err();
    assert_eq!(code(&err), "mds::network_denied");
    assert!(err.to_string().contains(": import-opt-in: "), "{err}");
    let requests = need(run_step(
        &files,
        &Runtime::new(),
        &net.with_imports(true),
        &Responses::new(),
    ));
    assert_eq!(urls(&requests), ["https://a.example/lib"]);
}

/// TP-90: each round checks the policy again, so a narrower allow list on resume fails.
/// Control: the same list.
#[test]
fn a_narrower_allow_list_on_resume_fails() {
    let main =
        "---\ndata:\n  a: https://a.example/a\n  b: https://b.example/b\n---\n{{a.n}}{{b.n}}\n";
    let files = [("main.mds", main)];
    let wide = policy(&["a.example", "b.example"]);
    let requests = need(run_step(&files, &Runtime::new(), &wide, &Responses::new()));
    let mut responses = Responses::new();
    let body = || ok(r#"{"n": 1}"#);
    answer(
        &requests,
        &mut responses,
        &[
            ("https://a.example/a", body()),
            ("https://b.example/b", body()),
        ],
    );
    let err = run_step(&files, &Runtime::new(), &policy(&["a.example"]), &responses).unwrap_err();
    assert!(err.to_string().contains(": allow-list: "), "{err}");
    assert!(done(run_step(&files, &Runtime::new(), &wide, &responses)).ends_with("11\n"));
}

/// A compile derives at most `MAX_REQUESTS_PER_COMPILE` distinct requests, redirect hops
/// included; the next one fails before it is added. Control: exactly the cap.
#[test]
fn the_request_cap_binds_before_a_new_request_is_added() {
    let mut main = String::from("---\ndata:\n");
    for index in 0..MAX_REQUESTS_PER_COMPILE {
        main.push_str(&format!("  e{index}: https://a.example/e{index}\n"));
    }
    main.push_str("---\n");
    let files = [("main.mds", main.as_str())];
    let net = policy(&["a.example"]);
    let requests = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
    assert_eq!(requests.len(), MAX_REQUESTS_PER_COMPILE);
    let mut responses = Responses::new();
    for (index, request) in requests.iter().enumerate() {
        let response = if index == 0 { moved("/hop") } else { ok("1") };
        responses.insert(request.id(), response);
    }
    let (result, needed) = run_round(&files, &Runtime::new(), &net, &responses);
    let err = result.unwrap_err();
    assert_eq!(
        err.to_string(),
        "resource limit exceeded: too many requests (max 64 per compile)"
    );
    assert_eq!(needed.len(), 1);
}

/// A transport failure, a status that is not 2xx or 3xx and a refused address fail the
/// entry. A message names the URL as declared and the origin, never the expanded URL.
#[test]
fn a_failed_answer_fails_the_entry() {
    let main = "---\nhidden: secret-path\ndata:\n  a: https://a.example/{{hidden}}\n---\n{{a}}\n";
    let net = policy(&["a.example"]);
    let first = need(run_step(
        &[("main.mds", main)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    let large = Response::Http {
        status: 200,
        headers: vec![("x-big".into(), vec![b'a'; 16 * 1024])],
        body: b"1".to_vec(),
    };
    let unlocated = Response::Http {
        status: 302,
        headers: Vec::new(),
        body: Vec::new(),
    };
    for (response, code_text, tail) in [
        (
            Response::Failed(TransportFailure::Timeout),
            "mds::network",
            "the request timed out",
        ),
        (
            Response::Failed(TransportFailure::Connect),
            "mds::network",
            "the connection failed",
        ),
        (
            Response::Failed(TransportFailure::Tls),
            "mds::network",
            "the TLS handshake failed",
        ),
        (
            Response::Failed(TransportFailure::Protocol),
            "mds::network",
            "the response is not valid HTTP",
        ),
        (
            Response::Failed(TransportFailure::BodyTooLarge),
            "mds::network",
            "the response body exceeds 10485760 bytes",
        ),
        (
            Response::Http {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
            },
            "mds::network",
            "the server answered with HTTP status 404",
        ),
        (
            Response::Failed(TransportFailure::AddressRefused),
            "mds::network_denied",
            "transport-address: the address is in a refused range",
        ),
        (
            large,
            "mds::network",
            "the response headers exceed 16384 bytes",
        ),
        (
            unlocated,
            "mds::network_denied",
            "redirect-check: a redirect needs exactly one location header",
        ),
    ] {
        let mut responses = Responses::new();
        responses.insert(first[0].id(), response);
        let err = run_step(&[("main.mds", main)], &Runtime::new(), &net, &responses).unwrap_err();
        assert_eq!(code(&err), code_text, "{err}");
        let message = err.to_string();
        assert!(
            message.contains(
                r#"entry "a", URL "https://a.example/{{hidden}}", origin "https://a.example": "#
            ),
            "{message}"
        );
        assert!(message.ends_with(tail), "{message}");
        assert!(!message.contains("secret-path"), "{message}");
    }
}

fn answered(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
    Response::Http {
        status,
        headers: headers
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.as_bytes().to_vec()))
            .collect(),
        body: body.as_bytes().to_vec(),
    }
}

/// Run one entry `a` of `main` to its end, with `response` as the answer to its request.
fn run_answered(main: &str, net: &NetworkPolicy, response: Response) -> Result<String, MdsError> {
    let files = [("main.mds", main)];
    let requests = need(run_step(&files, &Runtime::new(), net, &Responses::new()));
    let mut responses = Responses::new();
    for request in &requests {
        responses.insert(request.id(), response.clone());
    }
    run_step(&files, &Runtime::new(), net, &responses).map(|(text, outcome)| {
        assert!(matches!(outcome, Outcome::Done(_)), "{outcome:?}");
        text
    })
}

/// TP-95 and TP-100: with the envelope, a status that is not 2xx binds as
/// `{status, headers, body}`, and a reference reads a header with dot access. Control:
/// the same answer without the envelope fails with the status number.
#[test]
fn a_status_that_is_not_2xx_binds_only_with_the_envelope() {
    let net = policy(&["a.example"]);
    let response = answered(404, &[("X-Rate-Limit", "7")], r#"{"n": 1}"#);
    let main = "---\ndata:\n  a: {url: 'https://a.example/x', envelope: true}\n---\n{{a.status}} {{a.headers.x_rate_limit}} {{a.body.n}}\n";
    let text = run_answered(main, &net, response.clone()).unwrap();
    assert!(text.ends_with("404 7 1\n"), "{text:?}");
    let plain = "---\ndata:\n  a: https://a.example/x\n---\n{{a.n}}\n";
    let err = run_answered(plain, &net, response).unwrap_err();
    assert_eq!(code(&err), "mds::network");
    assert!(
        err.to_string()
            .ends_with("the server answered with HTTP status 404"),
        "{err}"
    );
}

/// The envelope binds a status from 200 to 599 only. Control: 599 binds.
#[test]
fn the_envelope_binds_a_status_from_200_to_599_only() {
    let net = policy(&["a.example"]);
    let main = "---\ndata:\n  a: {url: 'https://a.example/x', envelope: true}\n---\n{{a.status}}\n";
    for status in [100, 199, 600] {
        let err = run_answered(main, &net, answered(status, &[], "1")).unwrap_err();
        assert_eq!(code(&err), "mds::network", "{err}");
        assert!(
            err.to_string()
                .ends_with(&format!("the server answered with HTTP status {status}")),
            "{err}"
        );
    }
    let text = run_answered(main, &net, answered(599, &[], "1")).unwrap();
    assert!(text.ends_with("599\n"), "{text:?}");
}

/// A HEAD envelope has `body: null`: the core never converts the body of a HEAD answer,
/// so a body that is not JSON does not fail. Control: the same body for GET fails.
#[test]
fn a_head_envelope_has_a_null_body() {
    let net = policy(&["a.example"]);
    let response = answered(200, &[("Content-Length", "12")], "not json [");
    let entry = |method: &str| {
        format!("---\ndata:\n  a: {{url: 'https://a.example/x', method: {method}, envelope: true}}\n---\n{{{{a.headers.content_length}}}}\n@if a.body:\nbody\n@else:\nno body\n@end\n")
    };
    let text = run_answered(&entry("HEAD"), &net, response.clone()).unwrap();
    assert!(text.ends_with("12\nno body\n"), "{text:?}");
    let err = run_answered(&entry("GET"), &net, response).unwrap_err();
    assert_eq!(code(&err), "mds::yaml", "{err}");
}

/// One response binds a body entry and an envelope entry of the same request, each in
/// its own shape: the memo keeps the envelope flag next to the request ID.
#[test]
fn one_response_binds_a_body_entry_and_an_envelope_entry() {
    let main = "---\ndata:\n  a: https://a.example/x\n  b: {url: 'https://a.example/x', envelope: true}\n---\n{{a.n}} {{b.status}} {{b.body.n}}\n";
    let net = policy(&["a.example"]);
    let requests = need(run_step(
        &[("main.mds", main)],
        &Runtime::new(),
        &net,
        &Responses::new(),
    ));
    assert_eq!(urls(&requests), ["https://a.example/x"]);
    let text = run_answered(main, &net, answered(200, &[], r#"{"n": 3}"#)).unwrap();
    assert!(text.ends_with("3 200 3\n"), "{text:?}");
}

/// A host secret value in a header of the envelope fails the response check, and the
/// message names the secret, never its value. Control: another header value binds.
#[test]
fn a_secret_value_in_an_envelope_header_fails_the_response_check() {
    let net = with_secret(policy(&["a.example"]), "TOKEN", PLANTED, "a.example");
    let main = "---\ndata:\n  a: {url: 'https://a.example/x', envelope: true}\n---\n{{a.status}}\n";
    let err = run_answered(main, &net, answered(200, &[("X-Echo", PLANTED)], "1")).unwrap_err();
    assert_eq!(code(&err), "mds::network_denied");
    let message = err.to_string();
    // The value check comes first with a fixed message, so `{message}` below cannot
    // show the value.
    assert!(
        !message.contains(PLANTED),
        "the message must not show the secret value"
    );
    assert!(
        message.ends_with(
            r#"response-check: the response holds the value of a host secret (secret "TOKEN")"#
        ),
        "{message}"
    );
    let text = run_answered(main, &net, answered(200, &[("X-Echo", "plain")], "1")).unwrap();
    assert!(text.ends_with("200\n"), "{text:?}");
}

/// The raw bodies of one fetch state count against one total, the compile memory budget,
/// whatever the status: a redirect body and an error body count too. Exactly the total
/// passes, an answer with no body and a failed send still pass at the total, and one
/// more byte fails with `mds::resource_limit`.
#[test]
fn the_fetch_state_counts_every_body_against_one_total() {
    let cap = usize::try_from(MAX_RESPONSE_BYTES).unwrap();
    let total = MAX_RESPONSE_BODY_BYTES_PER_COMPILE;
    assert_eq!(total % cap, 0, "the total is a whole number of full bodies");
    let id = |n: usize| RequestId::parse_hex(&format!("{n:064x}")).unwrap();
    let mut state = FetchState::new();
    assert_eq!(state.body_bytes_left(), total);
    // The bodies come zeroed from the allocator, so the test writes no 200 MiB.
    let statuses = [200, 301, 302, 404, 500].into_iter().cycle();
    for (n, status) in statuses.take(total / cap).enumerate() {
        let response = Response::http(status, Vec::new(), vec![0; cap]);
        state = state.with_response(id(n), response).unwrap();
        assert_eq!(state.body_bytes_left(), total - (n + 1) * cap);
    }
    state = state
        .with_response(id(100), Response::http(302, Vec::new(), Vec::new()))
        .unwrap();
    state = state
        .with_response(id(101), Response::failed(TransportFailure::Timeout))
        .unwrap();
    let err = state
        .with_response(id(102), Response::http(302, Vec::new(), vec![0; 1]))
        .unwrap_err();
    assert_eq!(code(&err), "mds::resource_limit");
    assert_eq!(
        err.to_string(),
        "resource limit exceeded: response bodies exceed 209715200 bytes per compile"
    );
    assert_eq!(
        err.to_string(),
        MdsError::response_bodies_exceeded().to_string()
    );
}

/// The core checks the body of an answer that the host gives again: a body of exactly
/// 10 MiB binds, and one byte more fails with `mds::network`. A redirect answer is
/// checked too: at the cap its hop goes on, and one byte more fails.
#[test]
fn the_core_body_cap_binds_at_the_cap_and_fails_one_byte_over() {
    let cap = usize::try_from(MAX_RESPONSE_BYTES).unwrap();
    let net = policy(&["a.example"]);
    let text =
        "---\ndata:\n  a: {url: 'https://a.example/x', accept: text}\n---\n@if a:\nbound\n@end\n";
    let main = "---\ndata:\n  a: https://a.example/x\n---\n{{a.n}}\n";
    let body = |len| Response::http(200, Vec::new(), vec![b'a'; len]);
    let hop = |len| {
        Response::http(
            302,
            vec![("Location".into(), b"/next".to_vec())],
            vec![b'a'; len],
        )
    };
    let bound = run_answered(text, &net, body(cap)).unwrap();
    assert!(bound.ends_with("bound\n"), "control: a full body binds");
    let files = [("main.mds", main)];
    let first = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
    let mut responses = Responses::new();
    answer(&first, &mut responses, &[("https://a.example/x", hop(cap))]);
    let next = need(run_step(&files, &Runtime::new(), &net, &responses));
    assert_eq!(
        urls(&next),
        ["https://a.example/next"],
        "control: a full hop"
    );
    for (source, response) in [(text, body(cap + 1)), (main, hop(cap + 1))] {
        let err = run_answered(source, &net, response).unwrap_err();
        assert_eq!(code(&err), "mds::network", "{err}");
        assert!(
            err.to_string()
                .ends_with("the response body exceeds 10485760 bytes"),
            "{err}"
        );
    }
}

/// A response body counts against the data byte total of a compile, as a data file
/// does: four bodies of 8 MiB give exactly 32 MiB and bind, and one byte more fails with
/// `mds::resource_limit`.
#[test]
fn response_bodies_count_against_the_data_total_of_a_compile() {
    let mut main = String::from("---\ndata:\n");
    for name in ["a", "b", "c", "d"] {
        main.push_str(&format!(
            "  {name}: {{url: 'https://a.example/{name}', accept: text}}\n"
        ));
    }
    main.push_str("---\nbound\n");
    let files = [("main.mds", main.as_str())];
    let net = policy(&["a.example"]);
    let requests = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
    assert_eq!(requests.len(), 4);
    let quarter = MAX_DATA_BYTES_PER_COMPILE / 4;
    for (extra, fits) in [(0, true), (1, false)] {
        let mut responses = Responses::new();
        for (index, request) in requests.iter().enumerate() {
            let len = quarter + if index == 3 { extra } else { 0 };
            responses.insert(
                request.id(),
                Response::http(200, Vec::new(), vec![b'a'; len]),
            );
        }
        let result = run_step(&files, &Runtime::new(), &net, &responses);
        if fits {
            assert!(
                done(result).ends_with("bound\n"),
                "control: the total binds"
            );
        } else {
            let err = result.unwrap_err();
            assert_eq!(code(&err), "mds::resource_limit", "{err}");
            assert_eq!(
                err.to_string(),
                "resource limit exceeded: data files exceed 33554432 bytes per compile"
            );
        }
    }
}

/// Each redirect status runs the hop checks, with and without the envelope: a
/// `location` outside the allow list fails. Controls: a listed `location` gives the next
/// request, and 300 and 304, which are not redirects, bind with the envelope and fail
/// without it.
#[test]
fn each_redirect_status_runs_the_hop_checks() {
    let net = policy(&["a.example"]);
    let to = |status, location: &str| {
        Response::http(
            status,
            vec![("Location".into(), location.as_bytes().to_vec())],
            b"1".to_vec(),
        )
    };
    let plain = "---\ndata:\n  a: https://a.example/x\n---\n{{a}}\n";
    let envelope =
        "---\ndata:\n  a: {url: 'https://a.example/x', envelope: true}\n---\n{{a.status}}\n";
    for status in [301, 302, 303, 307, 308] {
        for main in [plain, envelope] {
            let files = [("main.mds", main)];
            let first = need(run_step(&files, &Runtime::new(), &net, &Responses::new()));
            for (location, refused) in [
                ("https://c.example/", true),
                ("https://a.example/next", false),
            ] {
                let mut responses = Responses::new();
                answer(
                    &first,
                    &mut responses,
                    &[("https://a.example/x", to(status, location))],
                );
                let result = run_step(&files, &Runtime::new(), &net, &responses);
                if refused {
                    let err = result.unwrap_err();
                    assert_eq!(code(&err), "mds::network_denied", "{status}: {err}");
                    assert!(
                        err.to_string().ends_with(
                            "redirect-check: the redirect target is not in the allow list"
                        ),
                        "{status}: {err}"
                    );
                } else {
                    assert_eq!(urls(&need(result)), [location], "{status}");
                }
            }
        }
    }
    for status in [300, 304] {
        let text = run_answered(envelope, &net, to(status, "https://c.example/")).unwrap();
        assert!(text.ends_with(&format!("{status}\n")), "{text:?}");
        let err = run_answered(plain, &net, to(status, "https://c.example/")).unwrap_err();
        assert!(
            err.to_string()
                .ends_with(&format!("the server answered with HTTP status {status}")),
            "{err}"
        );
    }
}

/// The names that only the step engine may use. A use outside the engine files links
/// engine code into a build that never starts a step compile, such as the WASM build.
const ENGINE_NAMES: [&str; 12] = [
    "StepCtx::new",
    "bind_with_engine",
    "scan_secrets",
    "request_label",
    "encode::expand",
    "envelope::envelope",
    "Request::new",
    "parse_location",
    ".check_hop(",
    ".check_method(",
    ".check_origin(",
    ".keeps_header(",
];

/// The files of the step engine, relative to `src/`.
const ENGINE_FILES: [&str; 6] = [
    "data/step.rs",
    "data/policy.rs",
    "data/url.rs",
    "data/encode.rs",
    "data/envelope.rs",
    "data/request.rs",
];

/// The findings of one source file at `path` (relative to `src/`, with `/` between parts):
/// each place where code outside the engine reaches the engine other than through the
/// `StepCtx.bind` pointer. Comments and literals are not code.
fn engine_reach_findings(path: &str, text: &str) -> Vec<String> {
    let code = code_only(text);
    let mut findings = Vec::new();
    if path == "data/step.rs" {
        let mut pointer_site = 0..0;
        match fn_body(&code, "pub(crate) fn bind_url_entry(") {
            Some((lines, body)) => {
                if !body.contains("ctx.bind") || has_name(&body, "bind_with_engine") {
                    findings.push(format!(
                        "{path}: bind_url_entry must reach the engine through ctx.bind only"
                    ));
                }
                pointer_site = lines;
            }
            None => findings.push(format!("{path}: bind_url_entry is missing")),
        }
        for (index, line) in code.lines().enumerate() {
            let allowed = line.trim_start().starts_with("fn bind_with_engine(")
                || line.trim() == "bind: bind_with_engine,";
            if !pointer_site.contains(&index) && !allowed && has_name(line, "bind_with_engine") {
                findings.push(format!("{path}:{}: bind_with_engine", index + 1));
            }
        }
    }
    if !ENGINE_FILES.contains(&path) {
        for (index, line) in code.lines().enumerate() {
            let names = ENGINE_NAMES
                .iter()
                .chain(Some(&"ctx.start(").filter(|_| path != "lib.rs"));
            for name in names {
                if has_name(line, name) {
                    findings.push(format!("{path}:{}: {name}", index + 1));
                }
            }
        }
    }
    findings
}

/// The line range and the text of the function whose line starts with `head`, to the
/// next line that starts with `}`.
fn fn_body(code: &str, head: &str) -> Option<(Range<usize>, String)> {
    let lines: Vec<&str> = code.lines().collect();
    let start = lines.iter().position(|line| line.starts_with(head))?;
    let end = start
        + lines[start..]
            .iter()
            .position(|line| line.starts_with('}'))?;
    Some((start..end + 1, lines[start..=end].join("\n")))
}

/// `line` names `name`: an identifier character is not next to an identifier edge of
/// the name.
fn has_name(line: &str, name: &str) -> bool {
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    line.match_indices(name).any(|(at, _)| {
        let before = line[..at].chars().next_back().is_some_and(ident);
        let after = line[at + name.len()..].chars().next().is_some_and(ident);
        let joined_before = before && name.starts_with(ident);
        let joined_after = after && name.ends_with(ident);
        !joined_before && !joined_after
    })
}

/// `text` with each comment and each string or char literal made blank. Each char gives
/// one char and each line break stays, so the line numbers stay.
fn code_only(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let blank = |out: &mut String, chars: &[char]| {
        for &ch in chars {
            out.push(if ch == '\n' { '\n' } else { ' ' });
        }
    };
    let at = |i: usize, pattern: &str| {
        pattern
            .chars()
            .enumerate()
            .all(|(k, ch)| chars.get(i + k) == Some(&ch))
    };
    let ident = |i: usize| {
        chars
            .get(i)
            .is_some_and(|&ch| ch.is_alphanumeric() || ch == '_')
    };
    // A raw string starts with `r`, or `br`, that ends no identifier.
    let raw_start = |i: usize| {
        let start = if i > 0 && chars[i - 1] == 'b' {
            i - 1
        } else {
            i
        };
        chars[i] == 'r' && !(start > 0 && ident(start - 1)) && (at(i, "r\"") || at(i, "r#"))
    };
    let mut i = 0;
    // Each turn moves `i` on by one char or more, so the bound is never reached first.
    for _ in 0..=chars.len() {
        let Some(&ch) = chars.get(i) else { break };
        let end = if at(i, "//") {
            chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |n| i + n)
        } else if at(i, "/*") {
            let mut depth = 0usize;
            let mut j = i;
            for _ in 0..chars.len() {
                if j >= chars.len() {
                    break;
                }
                if at(j, "/*") {
                    depth += 1;
                    j += 2;
                } else if at(j, "*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            j.min(chars.len())
        } else if ch == '"' || raw_start(i) {
            let hashes = if ch == 'r' {
                chars[i + 1..].iter().take_while(|&&c| c == '#').count()
            } else {
                0
            };
            let open = if ch == 'r' { i + 1 + hashes } else { i };
            if chars.get(open) != Some(&'"') {
                // A raw identifier such as `r#type`.
                out.push(ch);
                i += 1;
                continue;
            }
            let close: String = std::iter::once('"')
                .chain("#".repeat(hashes).chars())
                .collect();
            let mut j = open + 1;
            for _ in 0..chars.len() {
                if j >= chars.len() || at(j, &close) {
                    break;
                }
                j += if ch == '"' && chars[j] == '\\' { 2 } else { 1 };
            }
            (j + close.chars().count()).min(chars.len())
        } else if ch == '\'' && chars.get(i + 1) == Some(&'\\') {
            // An escape char literal: the closing quote comes after the escaped char.
            chars
                .get(i + 3..)
                .and_then(|rest| rest.iter().position(|&c| c == '\''))
                .map_or(chars.len(), |n| i + 4 + n)
        } else if ch == '\'' && chars.get(i + 2) == Some(&'\'') {
            i + 3
        } else {
            out.push(ch);
            i += 1;
            continue;
        };
        blank(&mut out, &chars[i..end]);
        i = end;
    }
    out
}

/// The `.rs` files below `root` that are not `*_tests.rs` files, as paths relative to
/// `root` with `/` between parts, with their text, and the directories that the walk read.
/// Every loop is bounded, and the walk must read every directory.
fn source_files(root: &Path) -> (Vec<String>, Vec<(String, String)>) {
    let relative = |path: &Path| {
        let parts: Vec<String> = path
            .strip_prefix(root)
            .unwrap()
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        parts.join("/")
    };
    let mut dirs = vec![root.to_path_buf()];
    let mut visited = Vec::new();
    let mut files = Vec::new();
    for _ in 0..1024 {
        let Some(dir) = dirs.pop() else { break };
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().take(4096).collect();
        assert!(
            entries.len() < 4096,
            "a source directory has too many entries"
        );
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() && name.ends_with(".rs") && !name.ends_with("_tests.rs") {
                files.push((relative(&path), std::fs::read_to_string(&path).unwrap()));
            }
        }
        visited.push(relative(&dir));
    }
    assert!(dirs.is_empty(), "the walk must read every source directory");
    (visited, files)
}

/// Resolver code reaches the step engine only through the `StepCtx.bind` pointer, so a
/// build that never starts a step compile keeps no engine code. The scan pins the code
/// shape; the WASM size limit of CI stays the measure of the linked code.
#[test]
fn code_outside_the_engine_reaches_it_through_the_bind_pointer_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let (dirs, files) = source_files(&root);
    for dir in ["data", "resolver", "evaluator", "lint"] {
        assert!(
            dirs.iter().any(|read| read == dir),
            "the walk must read {dir}"
        );
    }
    let read = |path: &str| {
        files
            .iter()
            .find(|(known, _)| known == path)
            .map(|(_, text)| text.as_str())
            .unwrap_or_else(|| panic!("the walk must read {path}"))
    };
    for path in ["data/step.rs", "resolver/process.rs", "lib.rs"] {
        assert!(!read(path).is_empty(), "{path} must hold code");
    }
    let module_scope = code_only(read("resolver/module_scope.rs"));
    for call in ["step::bind_url_entry", "step::check_secret_module"] {
        assert!(
            module_scope.contains(call),
            "resolver/module_scope.rs must call {call}"
        );
    }
    let findings: Vec<String> = files
        .iter()
        .flat_map(|(path, text)| engine_reach_findings(path, text))
        .collect();
    assert_eq!(
        findings,
        Vec::<String>::new(),
        "code outside the engine must reach it through StepCtx.bind only"
    );
}

/// Each planted reach of the engine gives exactly its own finding: a direct call at the
/// pointer site, a call in a planted child module and a hop check in resolver code. The
/// walk reads a planted child directory and skips a test file. Control: comments and
/// literals that name the engine give no finding.
#[test]
fn the_engine_scan_reports_each_planted_reach() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let step = std::fs::read_to_string(root.join("data/step.rs")).unwrap();
    let process = std::fs::read_to_string(root.join("resolver/process.rs")).unwrap();
    let quiet = "// scan_secrets(v, p)\n/* a /* nested */ policy.check_hop(a, b, 1) */\nconst A: &str = \"StepCtx::new\";\nconst B: &str = r#\"envelope::envelope\"#;\nconst C: char = '\\'';\nfn f<'a>(x: &'a str) -> &'a str { x }\n";
    assert_eq!(
        engine_reach_findings("resolver/quiet.rs", quiet),
        Vec::<String>::new()
    );
    assert_eq!(
        engine_reach_findings("data/step.rs", &step),
        Vec::<String>::new()
    );

    let pointer = step.replacen("    bind(node,", "    bind_with_engine(node,", 1);
    assert_ne!(pointer, step, "the plant must change the pointer site");
    assert_eq!(
        engine_reach_findings("data/step.rs", &pointer),
        ["data/step.rs: bind_url_entry must reach the engine through ctx.bind only"]
    );

    let planted = "pub(super) fn leak(v: &Value, p: &NetworkPolicy) {\n    let _ = crate::data::step::scan_secrets(v, p);\n}\n";
    assert_eq!(
        engine_reach_findings("resolver/nested/planted.rs", planted),
        ["resolver/nested/planted.rs:2: scan_secrets"]
    );

    let hop = format!("{process}\nfn planted(policy: &NetworkPolicy) {{\n    let _ = policy.check_hop(a, b, 1);\n}}\n");
    let line = hop
        .lines()
        .position(|text| text.contains(".check_hop("))
        .unwrap()
        + 1;
    assert_eq!(
        engine_reach_findings("resolver/process.rs", &hop),
        [format!("resolver/process.rs:{line}: .check_hop(")]
    );

    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("resolver").join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("planted.rs"), planted).unwrap();
    std::fs::write(nested.join("planted_tests.rs"), planted).unwrap();
    let (dirs, files) = source_files(dir.path());
    assert!(dirs.iter().any(|read| read == "resolver/nested"));
    let findings: Vec<String> = files
        .iter()
        .flat_map(|(path, text)| engine_reach_findings(path, text))
        .collect();
    assert_eq!(findings, ["resolver/nested/planted.rs:2: scan_secrets"]);
}
