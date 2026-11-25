//! The public step API: start and resume with stateless re-derive.

use std::collections::HashMap;

use mds::{
    AllowItem, CompileOptions, CompileResult, FetchContext, FetchState, MdsError, NativeFs,
    NetworkPolicy, Request, RequestId, Response, RunKey, Step, TransportFailure, VirtualFs,
};

fn context(items: &[&str]) -> FetchContext {
    let items = items
        .iter()
        .map(|item| AllowItem::parse(item).unwrap())
        .collect();
    FetchContext::new(
        NetworkPolicy::new(items).unwrap(),
        RunKey::from_bytes([7; 32]),
    )
}

fn step(main: &str, ctx: &FetchContext, state: &FetchState) -> Result<Step, MdsError> {
    let fs = VirtualFs::new(HashMap::from([("main.mds".to_owned(), main.to_owned())]));
    mds::compile_step_virtual_fs(fs, "main.mds", None, CompileOptions::default(), ctx, state)
}

fn need(step: Result<Step, MdsError>) -> Vec<Request> {
    match step {
        Ok(Step::Need(requests)) => requests,
        other => panic!("expected requests, got {other:?}"),
    }
}

fn done(step: Result<Step, MdsError>) -> CompileResult {
    match step {
        Ok(Step::Done(result)) => *result,
        other => panic!("expected a finished compile, got {other:?}"),
    }
}

fn ok(body: &str) -> Response {
    Response::http(200, Vec::new(), body.as_bytes().to_vec())
}

/// The state after an answer of `body` to each request.
fn answered(requests: &[Request], body: &str) -> FetchState {
    requests.iter().fold(FetchState::new(), |state, request| {
        state.with_response(request.id(), ok(body)).unwrap()
    })
}

fn code(error: &MdsError) -> String {
    error.serialize().code
}

const TWO: &str =
    "---\ndata:\n  a: https://a.example/a\n  b: https://b.example/b\n---\n{{a.n}}{{b.n}}\n";

/// TP-78: a second response for one ID fails with fixed text, and an entry name is not a
/// request ID. Control: two IDs are accepted.
#[test]
fn a_duplicate_response_id_fails_and_an_entry_name_is_no_id() {
    let id = RequestId::parse_hex(&"ab".repeat(32)).unwrap();
    let other = RequestId::parse_hex(&"cd".repeat(32)).unwrap();
    let state = FetchState::new().with_response(id, ok("1")).unwrap();
    let err = state.clone().with_response(id, ok("2")).unwrap_err();
    assert_eq!(code(&err), "mds::invalid_options");
    assert_eq!(
        err.to_string(),
        "invalid options: the fetch state already holds a response for this request"
    );
    assert_eq!(state.with_response(other, ok("2")).unwrap().len(), 2);
    for name in ["a", "user", &"A".repeat(64)] {
        assert!(RequestId::parse_hex(name).is_err(), "{name}");
    }
}

/// The state holds at most one response per request of a compile (64). Control: 64.
#[test]
fn the_state_holds_at_most_the_request_cap() {
    let ids: Vec<RequestId> = (0..=64)
        .map(|index| RequestId::parse_hex(&format!("{index:064x}")).unwrap())
        .collect();
    let full = ids[..64].iter().fold(FetchState::new(), |state, id| {
        state.with_response(*id, ok("1")).unwrap()
    });
    assert_eq!(full.len(), 64);
    let err = full.with_response(ids[64], ok("1")).unwrap_err();
    assert_eq!(code(&err), "mds::resource_limit");
    assert_eq!(
        err.to_string(),
        "resource limit exceeded: too many responses in the fetch state (max 64)"
    );
}

/// TP-90: each resume is a fresh compile that runs every check again. A response with
/// an extra ID fails, and so does a narrower allow list. Control: an identical resume
/// completes.
#[test]
fn every_resume_runs_every_check_again() {
    let wide = context(&["a.example", "b.example"]);
    let requests = need(step(TWO, &wide, &FetchState::new()));
    assert_eq!(requests.len(), 2);
    let state = answered(&requests, r#"{"n": 1}"#);

    let extra = RequestId::parse_hex(&"ef".repeat(32)).unwrap();
    let err = step(
        TWO,
        &wide,
        &state.clone().with_response(extra, ok("1")).unwrap(),
    )
    .unwrap_err();
    assert_eq!(code(&err), "mds::invalid_options");
    assert_eq!(
        err.to_string(),
        "invalid options: the fetch state holds a response that this compile did not request"
    );

    let err = step(TWO, &context(&["a.example"]), &state).unwrap_err();
    assert_eq!(code(&err), "mds::network_denied");
    assert!(err.to_string().contains(": allow-list: "), "{err}");

    let first = done(step(TWO, &wide, &state));
    let second = done(step(TWO, &wide, &state));
    assert_eq!(first, second);
    assert_eq!(first.into_markdown().unwrap(), "11\n");
}

/// The result lists each bound URL entry in template form, and `dependencies` holds no
/// URL. Canonical JSON writes `remoteInputs` only when the list is not empty. Control: a
/// compile with no URL entry has no `remoteInputs` key.
#[test]
fn remote_inputs_are_in_template_form_and_dependencies_hold_no_url() {
    let main = "---\nid: 7\ndata:\n  a: https://a.example/items/{{id}}\n---\n{{a.n}}\n";
    let ctx = context(&["a.example"]);
    let requests = need(step(main, &ctx, &FetchState::new()));
    assert_eq!(requests[0].url().path(), "/items/7");
    let result = done(step(main, &ctx, &answered(&requests, r#"{"n": 1}"#)));
    assert!(result.dependencies.is_empty(), "{:?}", result.dependencies);
    let [input] = result.remote_inputs.as_slice() else {
        panic!("expected one remote input, got {:?}", result.remote_inputs);
    };
    assert_eq!(
        (
            input.module.as_str(),
            input.entry.as_str(),
            input.template.as_str(),
            input.origin.as_str()
        ),
        (
            "main.mds",
            "a",
            "https://a.example/items/{{id}}",
            "https://a.example"
        )
    );
    let json = result.to_canonical_json();
    assert_eq!(
        json["remoteInputs"],
        serde_json::json!([{
            "module": "main.mds",
            "entry": "a",
            "template": "https://a.example/items/{{id}}",
            "origin": "https://a.example",
        }])
    );
    assert!(!json.to_string().contains("/items/7"), "{json}");

    let plain = done(step("Hello!\n", &ctx, &FetchState::new()));
    assert!(plain.remote_inputs.is_empty());
    assert!(plain.to_canonical_json().get("remoteInputs").is_none());
}

/// A transport failure enters as a fixed kind: a refused address gives the policy code
/// and any other failure gives `mds::network`.
#[test]
fn a_transport_failure_enters_as_a_fixed_kind() {
    let main = "---\ndata:\n  a: https://a.example/a\n---\n{{a.n}}\n";
    let ctx = context(&["a.example"]);
    let requests = need(step(main, &ctx, &FetchState::new()));
    for (failure, expected, tail) in [
        (
            TransportFailure::AddressRefused,
            "mds::network_denied",
            "transport-address: the address is in a refused range",
        ),
        (
            TransportFailure::Timeout,
            "mds::network",
            "the request timed out",
        ),
    ] {
        let state = FetchState::new()
            .with_response(requests[0].id(), Response::failed(failure))
            .unwrap();
        let err = step(main, &ctx, &state).unwrap_err();
        assert_eq!(code(&err), expected, "{err}");
        assert!(err.to_string().ends_with(tail), "{err}");
    }
}

/// The value depth cap of the data values.
const DEPTH_CAP: usize = 64;

/// An envelope puts the body one level deeper. Thus a body with a value at the depth cap
/// fails with a data error, not with an internal error. Control: the same body with no
/// envelope binds.
#[test]
fn an_envelope_over_a_body_at_the_depth_cap_fails_with_a_data_error() {
    let ctx = context(&["a.example"]);
    let body = format!("{}1{}", r#"{"n":"#.repeat(DEPTH_CAP), "}".repeat(DEPTH_CAP));
    let wrapped =
        "---\ndata:\n  a: {url: 'https://a.example/a', envelope: true}\n---\n{{a.status}}\n";
    let requests = need(step(wrapped, &ctx, &FetchState::new()));
    let err = step(wrapped, &ctx, &answered(&requests, &body)).unwrap_err();
    assert!(!matches!(err, MdsError::Internal), "{err}");
    assert_eq!(code(&err), "mds::data", "{err}");
    assert_eq!(
        err.to_string(),
        r#"data error in "main.mds", entry "a", field "body": value nesting exceeds maximum depth of 64"#
    );

    let plain = "---\ndata:\n  a: https://a.example/a\n---\n@if a.n:\nbound\n@end\n";
    let requests = need(step(plain, &ctx, &FetchState::new()));
    let result = done(step(plain, &ctx, &answered(&requests, &body)));
    assert_eq!(result.into_markdown().unwrap(), "bound\n");
}

/// A data file that changes between two steps is read again: each step is a fresh
/// compile that keeps no value of the step before it.
#[test]
fn a_data_file_changed_between_two_steps_is_read_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(root.join(".mdsroot"), "").unwrap();
    std::fs::write(root.join("cfg.json"), r#"{"name": "old"}"#).unwrap();
    let main = root.join("main.mds");
    std::fs::write(
        &main,
        "---\ndata:\n  cfg: ./cfg.json\n  remote: https://a.example/x\n---\n{{cfg.name}} {{remote.n}}\n",
    )
    .unwrap();
    let ctx = context(&["a.example"]);
    let run = |state: &FetchState| {
        mds::compile_step_native_fs(
            NativeFs::new(),
            &main,
            None,
            CompileOptions::default(),
            &ctx,
            state,
        )
    };
    let requests = need(run(&FetchState::new()));
    std::fs::write(root.join("cfg.json"), r#"{"name": "new"}"#).unwrap();
    let result = done(run(&answered(&requests, r#"{"n": 1}"#)));
    assert!(
        result
            .dependencies
            .iter()
            .all(|path| path.ends_with("cfg.json")),
        "{:?}",
        result.dependencies
    );
    assert_eq!(result.into_markdown().unwrap(), "new 1\n");
}

/// A string source steps as a module does.
#[test]
fn a_string_source_steps_as_a_module_does() {
    let ctx = context(&["a.example"]);
    let source = "---\ndata:\n  a: https://a.example/a\n---\n{{a.n}}\n";
    let run = |state: &FetchState| {
        mds::compile_step_str(source, None, None, CompileOptions::default(), &ctx, state)
    };
    let requests = need(run(&FetchState::new()));
    let result = done(run(&answered(&requests, r#"{"n": 4}"#)));
    assert_eq!(result.into_markdown().unwrap(), "4\n");
}
