//! The step engine through real compiles: each `run_step` call is one round.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use super::encode::{check_header_value, encode_path_segment, encode_query, EncodeRefusal};
use super::encode_tests::{vectors, Vector};
use super::policy::{AllowItem, Layer, NetworkPolicy, Refusal, Secret, SecretValue};
use super::request::{HeaderValue, Request, RequestId, RunKey};
use super::step::{Outcome, RemoteInput, Response, StepCtx, Transport};
use super::{convert, Accept, DataInput, DataLabel};
use crate::evaluator::budget::MemBudget;
use crate::limits::{MAX_HEADER_VALUE_BYTES, MAX_REDIRECTS, MAX_REQUESTS_PER_COMPILE};
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
    for (response, refused) in [
        (ok(&echo_value), true),
        (ok(&echo_key), true),
        (moved(&format!("/next?k={PLANTED}")), true),
        (ok(r#"{"n": 4}"#), false),
    ] {
        let mut responses = Responses::new();
        answer(&first, &mut responses, &[("https://a.example/x", response)]);
        let result = run_step(&files, &Runtime::new(), &net, &responses);
        if refused {
            let err = result.unwrap_err();
            assert_eq!(code(&err), "mds::network_denied");
            let message = err.to_string();
            assert!(message.ends_with("response-check: the response holds the value of a host secret (secret \"GITHUB\")"), "{message}");
            assert!(!message.contains(PLANTED), "{message}");
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
    let refused = Refusal::new(Layer::TransportAddress, "the address is in a refused range");
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
            Response::Failed(Transport::Timeout),
            "mds::network",
            "the request timed out",
        ),
        (
            Response::Failed(Transport::Connect),
            "mds::network",
            "the connection failed",
        ),
        (
            Response::Failed(Transport::Tls),
            "mds::network",
            "the TLS handshake failed",
        ),
        (
            Response::Failed(Transport::Protocol),
            "mds::network",
            "the response is not valid HTTP",
        ),
        (
            Response::Failed(Transport::BodyTooLarge),
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
            Response::Failed(Transport::Refused(refused)),
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
    assert!(
        message.ends_with(
            r#"response-check: the response holds the value of a host secret (secret "TOKEN")"#
        ),
        "{message}"
    );
    assert!(!message.contains(PLANTED), "{message}");
    let text = run_answered(main, &net, answered(200, &[("X-Echo", "plain")], "1")).unwrap();
    assert!(text.ends_with("200\n"), "{text:?}");
}
