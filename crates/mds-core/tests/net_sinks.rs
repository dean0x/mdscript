//! The sinks of request text: a message, a warning, `Debug` and a result field show the
//! URL as declared and the origin, never the expanded URL or response text.

use std::collections::HashMap;

use mds::{
    AllowItem, CompileOptions, CompileResult, FetchContext, FetchState, MdsError, NetworkPolicy,
    Request, Response, RunKey, Step, VirtualFs,
};

const MARKER: &str = "marker-from-response";

/// `b` reads a value of the response of `a` into its URL.
const CHAIN: &str =
    "---\ndata:\n  a: https://a.example/a\n  b: https://a.example/b/{{a.x}}\n---\n{{b.n}}\n";
const TEMPLATE: &str = "https://a.example/b/{{a.x}}";

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

fn step(files: &[(&str, &str)], ctx: &FetchContext, state: &FetchState) -> Result<Step, MdsError> {
    let files = files
        .iter()
        .map(|&(key, text)| (key.to_owned(), text.to_owned()))
        .collect::<HashMap<_, _>>();
    let fs = VirtualFs::new(files);
    mds::compile_step_virtual_fs(fs, "main.mds", None, CompileOptions::default(), ctx, state)
}

/// Run `main` to its end. `serve` answers each request by its URL path.
fn drive(
    main: &str,
    ctx: &FetchContext,
    serve: impl Fn(&Request) -> Response,
) -> (Result<CompileResult, MdsError>, Vec<Request>) {
    let mut state = FetchState::new();
    let mut sent = Vec::new();
    for _ in 0..=64 {
        match step(&[("main.mds", main)], ctx, &state) {
            Ok(Step::Need(requests)) => {
                for request in requests {
                    state = state.with_response(request.id(), serve(&request)).unwrap();
                    sent.push(request);
                }
            }
            Ok(Step::Done(result)) => return (Ok(*result), sent),
            Ok(other) => panic!("unexpected step {other:?}"),
            Err(error) => return (Err(error), sent),
        }
    }
    panic!("the compile did not end within the round bound");
}

fn serve_chain(status: u16) -> impl Fn(&Request) -> Response {
    move |request| {
        if request.url().path() == "/a" {
            let body = format!(r#"{{"x": "{MARKER}"}}"#);
            Response::http(200, Vec::new(), body.into_bytes())
        } else {
            Response::http(status, Vec::new(), br#"{"n": 1}"#.to_vec())
        }
    }
}

fn record(error: &MdsError) -> String {
    serde_json::to_string(&error.serialize()).unwrap()
}

/// TP-96: a response value in the next URL, which gets 404, is absent from `Display`,
/// `Debug` and the serialized error. Control: the URL template and the origin are in
/// the message, and the marker is in the URL that the sender got.
#[test]
fn a_response_value_in_a_failed_next_url_is_in_no_message() {
    let (result, sent) = drive(CHAIN, &context(&["a.example"]), serve_chain(404));
    let err = result.unwrap_err();
    assert_eq!(err.serialize().code, "mds::network");
    assert_eq!(sent[1].url().path(), format!("/b/{MARKER}"));
    let sinks = [err.to_string(), format!("{err:?}"), record(&err)];
    for sink in &sinks {
        assert!(!sink.contains(MARKER), "{sink}");
    }
    assert!(
        sinks[0].contains(&format!(
            r#"entry "b", URL "{TEMPLATE}", origin "https://a.example": "#
        )),
        "{}",
        sinks[0]
    );
}

/// The `Debug` of a needed request shows the template and the origin, never the
/// expanded URL. Control: the template is there.
#[test]
fn a_needed_request_shows_the_template_in_debug() {
    let ctx = context(&["a.example"]);
    let (_, sent) = drive(CHAIN, &ctx, serve_chain(404));
    let debug = format!("{:?}", sent[1]);
    assert!(!debug.contains(MARKER), "{debug}");
    assert!(debug.contains(TEMPLATE), "{debug}");
    assert_eq!(sent[1].template(), TEMPLATE);
}

/// A finished compile shows the marker in no warning, no remote input and no dependency,
/// in Rust and in canonical JSON. Control: the remote input holds the template.
#[test]
fn a_finished_compile_shows_no_response_value_in_its_metadata() {
    let main = "---\ndata:\n  a: https://a.example/a\n  b: {url: 'https://a.example/b/{{a.x}}', envelope: true}\n---\n{{b.status}}\n";
    let (result, _) = drive(main, &context(&["a.example"]), serve_chain(404));
    let result = result.unwrap();
    let inputs = format!("{:?}", result.remote_inputs);
    for sink in [
        format!("{:?}", result.warnings),
        inputs.clone(),
        format!("{:?}", result.dependencies),
    ] {
        assert!(!sink.contains(MARKER), "{sink}");
    }
    assert!(inputs.contains(TEMPLATE), "{inputs}");
    let mut json = result.to_canonical_json();
    json["output"] = serde_json::Value::Null;
    assert!(!json.to_string().contains(MARKER), "{json}");
    assert!(json.to_string().contains(TEMPLATE), "{json}");
}

/// A policy refusal has the fixed help of its check, in the message record too.
/// Control: the replay help of entries with no replay value stays the same.
#[test]
fn a_refusal_has_the_fixed_help_of_its_check() {
    let main = "---\ndata:\n  a: https://b.example/x\n---\n{{a.n}}\n";
    let err = step(
        &[("main.mds", main)],
        &context(&["a.example"]),
        &FetchState::new(),
    )
    .unwrap_err();
    let serialized = err.serialize();
    assert_eq!(serialized.code, "mds::network_denied");
    assert_eq!(
        serialized.help.as_deref(),
        Some("add the origin to the network allow list of the host")
    );
    let built = MdsError::network_refused(
        "main.mds",
        "a",
        "https://b.example/x",
        "https://b.example",
        context(&["a.example"])
            .policy()
            .check_address("10.0.0.2:443".parse().unwrap())
            .unwrap_err(),
    );
    assert_eq!(
        built.to_string(),
        r#"network access denied for data entries: "main.mds", entry "a", URL "https://b.example/x", origin "https://b.example": transport-address: the address is in a refused range"#
    );
    assert_eq!(
        built.serialize().help.as_deref(),
        Some("to reach an address in a refused range, add its exact IP address and port to the allow list")
    );
    let frontier = MdsError::network_denied(&[("main.mds", "a")]);
    assert_eq!(
        frontier.serialize().help.as_deref(),
        Some("provide a runtime value for each entry to replay it without network access")
    );
}

/// A value error of a response names the entry, never a key path, because the key is
/// response text. Control: the same value in a data file names its key path.
#[test]
fn a_value_error_of_a_response_names_no_key_path() {
    let body = format!(r#"{{"{MARKER}\u001b": 1}}"#);
    let ctx = context(&["a.example"]);
    let main = "---\ndata:\n  a: https://a.example/a\n---\n{{a}}\n";
    let (result, _) = drive(main, &ctx, |_| {
        Response::http(200, Vec::new(), body.clone().into_bytes())
    });
    let err = result.unwrap_err();
    assert_eq!(err.serialize().code, "mds::data");
    assert_eq!(
        err.to_string(),
        r#"data error in "main.mds", entry "a": forbidden character in a data string or key"#
    );
    let file = "---\ndata:\n  a: ./a.json\n---\n{{a}}\n";
    let err = step(
        &[("main.mds", file), ("a.json", body.as_str())],
        &ctx,
        &FetchState::new(),
    )
    .unwrap_err();
    assert_eq!(err.serialize().code, "mds::data");
    assert!(err.to_string().contains(MARKER), "{err}");
}
