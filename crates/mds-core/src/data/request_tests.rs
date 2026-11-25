use super::digest::{hex, hmac_sha256, sha256};
use super::policy::{AllowItem, Layer, NetworkPolicy, Secret, SecretValue};
use super::request::*;
use super::url::parse_absolute;
use super::Accept;

const PLANTED: &str = "planted-secret-value";
const URL: &str = "https://api.example/items?q=1";
const TEMPLATE: &str = "https://api.example/{{kind}}?q={{query}}";

fn key(byte: u8) -> RunKey {
    RunKey::from_bytes([byte; 32])
}

fn policy() -> NetworkPolicy {
    let items = vec![AllowItem::parse("api.example").unwrap()];
    let origin = AllowItem::parse("api.example").unwrap().origin();
    let value = SecretValue::new(PLANTED.into()).unwrap();
    NetworkPolicy::new(items)
        .unwrap()
        .with_secret(Secret::new("token", value, origin).unwrap())
        .unwrap()
}

fn text(name: &str, value: &str) -> RequestHeader {
    RequestHeader::new(name.into(), HeaderValue::Text(value.into()))
}

fn secret(name: &str, secret: &str) -> RequestHeader {
    let value = HeaderValue::Secret {
        name: secret.into(),
        prefix: "Bearer ".into(),
    };
    RequestHeader::new(name.into(), value)
}

fn request(url: &str, headers: Vec<RequestHeader>, key: &RunKey) -> Request {
    let url = parse_absolute(url).unwrap();
    Request::new(
        Method::Get,
        TEMPLATE,
        url,
        Accept::Json,
        headers,
        key,
        &policy(),
    )
    .unwrap()
}

fn id(url: &str, headers: Vec<RequestHeader>) -> RequestId {
    request(url, headers, &key(1)).id()
}

/// TP-79 unit: equal requests have one ID. The header order and the letter case of a
/// header name do not change it.
#[test]
fn equal_requests_have_one_id() {
    let headers = || vec![text("X-B", "2"), text("x-a", "1")];
    assert_eq!(id(URL, headers()), id(URL, headers()));
    assert_eq!(
        id(URL, headers()),
        id(URL, vec![text("X-A", "1"), text("x-b", "2")])
    );
}

/// A change to any one part of a request gives another ID.
#[test]
fn each_request_part_changes_the_id() {
    let base = id(URL, vec![text("X-A", "1")]);
    let url = parse_absolute(URL).unwrap();
    let headers = vec![text("X-A", "1")];
    let other = |method, accept| {
        Request::new(
            method,
            TEMPLATE,
            url.clone(),
            accept,
            headers.clone(),
            &key(1),
            &policy(),
        )
        .unwrap()
        .id()
    };
    for changed in [
        id("https://api.example/items?q=2", vec![text("X-A", "1")]),
        id("https://api.example/items", vec![text("X-A", "1")]),
        id(URL, vec![text("X-A", "2")]),
        id(URL, vec![text("X-B", "1")]),
        id(URL, vec![text("X-A", "1"), text("X-A", "1")]),
        id(URL, Vec::new()),
        other(Method::Head, Accept::Json),
        other(Method::Get, Accept::Yaml),
        other(Method::Get, Accept::Text),
    ] {
        assert_ne!(changed, base);
    }
    assert_eq!(other(Method::Get, Accept::Json), base);
}

/// The same URL with and without a secret header gives two IDs, and a secret header
/// differs from text that names the same parts.
#[test]
fn a_secret_header_changes_the_id() {
    let with_secret = id(URL, vec![secret("Authorization", "token")]);
    assert_ne!(with_secret, id(URL, Vec::new()));
    assert_ne!(with_secret, id(URL, vec![text("Authorization", "Bearer ")]));
    assert_ne!(
        with_secret,
        id(
            URL,
            vec![text("Authorization", &format!("Bearer {PLANTED}"))]
        )
    );
}

/// Two run keys change only the ID of a request with a secret header.
#[test]
fn the_run_key_changes_only_secret_request_ids() {
    let plain = || vec![text("X-A", "1")];
    assert_eq!(
        request(URL, plain(), &key(1)).id(),
        request(URL, plain(), &key(2)).id()
    );
    let bound = || vec![secret("Authorization", "token")];
    assert_ne!(
        request(URL, bound(), &key(1)).id(),
        request(URL, bound(), &key(2)).id()
    );
}

/// A secret header needs a host secret of that name that is bound to the request origin.
/// Control: the bound origin with its default port written.
#[test]
fn secret_headers_bind_to_the_secret_origin() {
    for (url, name) in [
        ("https://api.example/x", "missing"),
        ("https://api.example:8443/x", "token"),
    ] {
        let url = parse_absolute(url).unwrap();
        let refused = Request::new(
            Method::Get,
            TEMPLATE,
            url,
            Accept::Json,
            vec![secret("Authorization", name)],
            &key(1),
            &policy(),
        )
        .unwrap_err();
        assert_eq!(refused.layer(), Layer::SecretBinding, "{name}");
    }
    let bound = request(
        "https://api.example:443/x",
        vec![secret("Authorization", "token")],
        &key(1),
    );
    assert_eq!(bound.headers().len(), 1);
}

/// `Debug` of a request with a secret header shows the descriptor, never the value.
/// Control: the descriptor parts are in `Debug`.
#[test]
fn debug_output_of_a_secret_request_has_no_secret_value() {
    let bound = request(URL, vec![secret("Authorization", "token")], &key(1));
    let debug = format!("{bound:?} {:?}", key(7));
    // Fixed messages: a failure must not show the text that holds the value.
    assert!(
        !debug.contains(PLANTED),
        "Debug must not show the secret value"
    );
    assert!(
        debug.contains("token") && debug.contains("Bearer "),
        "Debug must show the secret name and prefix"
    );
    assert!(
        !debug.contains("[7, 7"),
        "Debug must not show the key bytes"
    );
}

/// `Debug` of a request and of its URL shows the URL as declared, the origin and the
/// header names, never the expanded path or query or a header value. Control: the
/// template, the origin and the header name are in `Debug`.
#[test]
fn debug_output_of_a_request_has_no_expanded_url_or_header_value() {
    let markers = ["marker-path", "marker-query", "marker-header"];
    let planted = request(
        "https://api.example/marker-path?q=marker-query",
        vec![text("X-Echo", "marker-header")],
        &key(1),
    );
    assert_eq!(planted.url().path(), "/marker-path");
    let request = format!("{planted:?}");
    let url = format!("{:?}", planted.url());
    let headers = format!("{:?}", planted.headers());
    for debug in [&request, &url, &headers] {
        for marker in markers {
            assert!(!debug.contains(marker), "{marker} in {debug}");
        }
    }
    for debug in [&request, &url] {
        assert!(debug.contains(r#"origin: https://api.example"#), "{debug}");
    }
    assert!(request.contains(TEMPLATE), "{request}");
    assert!(headers.contains("X-Echo"), "{headers}");
    assert_eq!(planted.template(), TEMPLATE);
}

/// The ID is the SHA-256 digest of the documented canonical text, with the HMAC of the
/// secret value under the run key.
#[test]
fn the_id_is_the_digest_of_the_canonical_text() {
    fn field(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let hmac = hex(&hmac_sha256(&[1; 32], &[PLANTED.as_bytes()]));
    let mut canonical = Vec::new();
    for part in ["mds-request-1", "GET", URL, "json"] {
        field(&mut canonical, part.as_bytes());
    }
    field(&mut canonical, &2u64.to_be_bytes());
    let secret_key = format!("authorization\0s13:authorization5:token7:Bearer 64:{hmac}");
    field(&mut canonical, secret_key.as_bytes());
    field(&mut canonical, b"x-a\0t3:x-a1:1");
    field(&mut canonical, b"");
    let expected = sha256(&[&canonical]);
    let actual = id(
        URL,
        vec![text("X-A", "1"), secret("Authorization", "token")],
    );
    assert_eq!(actual.as_hex(), hex(&expected));
}

/// The charge bound of the identity covers twice the canonical text, for a request with
/// no header and for one with a long text header and a secret header.
#[test]
fn the_identity_bound_covers_the_canonical_text() {
    fn netstring(text: &str) -> String {
        format!("{}:{text}", text.len())
    }
    let long = "v".repeat(8 * 1024);
    let hmac = hex(&hmac_sha256(&[1; 32], &[PLANTED.as_bytes()]));
    let cases = [
        (Vec::new(), Vec::new()),
        (
            vec![text("X-Long", &long), secret("Authorization", "token")],
            vec![
                format!("x-long\0t{}{}", netstring("x-long"), netstring(&long)),
                format!(
                    "authorization\0s{}{}{}{}",
                    netstring("authorization"),
                    netstring("token"),
                    netstring("Bearer "),
                    netstring(&hmac)
                ),
            ],
        ),
    ];
    for (headers, keys) in cases {
        let fields = ["mds-request-1", "GET", URL, "json"]
            .iter()
            .map(|part| part.len())
            .chain([8])
            .chain(keys.iter().map(String::len))
            .chain([0]);
        let canonical: usize = fields.map(|len| len + 8).sum();
        let url = parse_absolute(URL).unwrap();
        let bound = identity_bytes(&url, &headers).unwrap();
        assert!(bound >= 2 * canonical, "{bound} < 2 * {canonical}");
    }
}

/// The hex form is exactly 64 lower-case hex digits. Control: the form of a real ID.
#[test]
fn request_ids_round_trip_through_lower_case_hex() {
    let original = id(URL, Vec::new());
    let text = original.as_hex();
    assert_eq!(text.len(), 64);
    assert_eq!(RequestId::parse_hex(&text), Ok(original));
    for bad in [
        String::new(),
        "remote".into(),
        text.to_uppercase(),
        text[1..].to_owned(),
        format!("{text}0"),
        format!("g{}", &text[1..]),
        format!("+{}", &text[1..]),
        format!(" {}", &text[1..]),
    ] {
        assert_eq!(RequestId::parse_hex(&bad), Err(RequestIdError), "{bad}");
    }
}
