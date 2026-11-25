use std::net::SocketAddr;

use super::policy::*;
use super::url::{parse_absolute, CheckedUrl, Origin};
use crate::limits::{MAX_ALLOW_ITEMS, MAX_REDIRECTS, MAX_SECRETS};

fn item(text: &str) -> AllowItem {
    AllowItem::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn url(text: &str) -> CheckedUrl {
    parse_absolute(text).unwrap()
}

fn origin(text: &str) -> Origin {
    url(text).origin()
}

fn policy(items: &[&str]) -> NetworkPolicy {
    NetworkPolicy::new(items.iter().map(|text| item(text)).collect()).unwrap()
}

fn value(text: &str) -> SecretValue {
    SecretValue::new(text.into()).unwrap()
}

fn secret(name: &str, at: &str) -> Secret {
    Secret::new(name, value("planted-secret-value"), origin(at)).unwrap()
}

fn address(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

fn layer<T: std::fmt::Debug>(result: Result<T, Refusal>) -> Layer {
    result.unwrap_err().layer()
}

/// Each accepted form names one exact origin; the default port and no port are equal.
#[test]
fn allow_items_name_one_exact_origin() {
    for (text, expected) in [
        ("api.example", "https://api.example"),
        ("api.example:8443", "https://api.example:8443"),
        ("https://api.example", "https://api.example"),
        ("https://api.example:443", "https://api.example"),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ("http://api.example:80", "http://api.example"),
        ("127.0.0.1:8080", "https://127.0.0.1:8080"),
        ("[::1]:8080", "https://[::1]:8080"),
        ("https://[::1]", "https://[::1]"),
    ] {
        assert_eq!(item(text).origin().to_string(), expected, "{text}");
    }
}

/// The item grammar refuses each form outside it, with fixed text. Short IPv4 forms are
/// refused as items, as in declarations. Control: the canonical address with a port.
#[test]
fn allow_items_refuse_every_other_form() {
    for text in [
        "",
        "https://",
        "http://api.example",
        "http://127.0.0.1",
        "ftp://api.example",
        "HTTPS://api.example",
        "Api.example",
        "api.example.",
        "*.example",
        "api.example/path",
        "api.example/",
        "api.example?q",
        "api.example#part",
        "user@api.example",
        " api.example",
        "api.example:",
        "api.example:0",
        "api.example:65536",
        "api.example:+80",
        "127.1",
        "2130706433",
        "0x7f000001",
        "127.1:8080",
        "[::1",
        "[fe80::1%25en0]:8080",
    ] {
        let error = AllowItem::parse(text).unwrap_err();
        assert!(
            matches!(error, PolicyError::AllowItem(_)),
            "{text}: {error:?}"
        );
        if !text.is_empty() {
            assert!(!error.to_string().contains(text), "{text}: {error}");
        }
    }
    assert!(AllowItem::parse("127.0.0.1:8080").is_ok());
}

/// A match needs the same scheme, the exact host and the same port.
#[test]
fn allow_items_match_the_exact_host_scheme_and_port() {
    let plain = item("api.example");
    let explicit = item("api.example:443");
    for at in ["https://api.example/", "https://api.example:443/x"] {
        assert!(plain.matches(&origin(at)), "{at}");
        assert!(explicit.matches(&origin(at)), "{at}");
    }
    for at in [
        "http://api.example:443/",
        "https://api.example:8443/",
        "https://sub.api.example/",
        "https://example/",
        "https://api.example.org/",
    ] {
        assert!(!plain.matches(&origin(at)), "{at}");
    }
    assert!(item("http://127.0.0.1:8080").matches(&origin("http://127.0.0.1:8080/")));
    assert!(!item("127.0.0.1:8080").matches(&origin("http://127.0.0.1:8080/")));
}

#[test]
fn allow_list_takes_one_to_the_item_cap() {
    let items = |count: usize| (0..count).map(|i| item(&format!("h{i}.example"))).collect();
    assert_eq!(
        NetworkPolicy::new(Vec::new()).unwrap_err(),
        PolicyError::NoItems
    );
    assert!(NetworkPolicy::new(items(MAX_ALLOW_ITEMS)).is_ok());
    let error = NetworkPolicy::new(items(MAX_ALLOW_ITEMS + 1)).unwrap_err();
    assert_eq!(error, PolicyError::TooManyItems);
    assert!(error.to_string().contains(&MAX_ALLOW_ITEMS.to_string()));
}

/// Names are identifiers in their own name space, values are visible ASCII, and a
/// secret goes to one `https` origin. Controls: the valid form of each.
#[test]
fn secrets_have_identifier_names_visible_values_and_an_https_origin() {
    for name in ["", "9token", "two.parts", "a-b", "café"] {
        let result = Secret::new(name, value("v"), origin("https://api.example/"));
        assert_eq!(result.unwrap_err(), PolicyError::SecretName, "{name}");
    }
    for text in ["", "a b", "tab\there", "line\n", "café"] {
        assert_eq!(
            SecretValue::new(text.into()).unwrap_err(),
            PolicyError::SecretValue
        );
    }
    let error = Secret::new("token", value("v"), origin("http://127.0.0.1:8080/")).unwrap_err();
    assert_eq!(error, PolicyError::SecretScheme);
    assert_eq!(value("!a~Z9").expose_for_send(), "!a~Z9");
    assert!(Secret::new("_token9", value("v"), origin("https://api.example/")).is_ok());
}

#[test]
fn policy_takes_unique_secrets_up_to_the_cap() {
    let mut full = policy(&["api.example"]);
    for i in 0..MAX_SECRETS {
        full = full
            .with_secret(secret(&format!("s{i}"), "https://api.example/"))
            .unwrap();
    }
    let error = full
        .clone()
        .with_secret(secret("extra", "https://api.example/"))
        .unwrap_err();
    assert_eq!(error, PolicyError::TooManySecrets);
    assert!(error.to_string().contains(&MAX_SECRETS.to_string()));
    let error = policy(&["api.example"])
        .with_secret(secret("token", "https://api.example/"))
        .and_then(|policy| policy.with_secret(secret("token", "https://other.example/")))
        .unwrap_err();
    assert_eq!(error, PolicyError::DuplicateSecret);
}

/// The value is absent from `Debug` of the value, the secret and the policy, while the
/// send accessor holds it.
#[test]
fn debug_output_does_not_show_a_secret_value() {
    let policy = policy(&["api.example"])
        .with_secret(secret("token", "https://api.example/"))
        .unwrap();
    let bound = policy
        .bind_secret("token", &origin("https://api.example/"))
        .unwrap();
    assert_eq!(bound.expose_for_send(), "planted-secret-value");
    let debug = format!("{policy:?} {bound:?}");
    // Fixed messages: a failure must not show the text that holds the value.
    assert!(
        !debug.contains("planted-secret-value"),
        "Debug must not show the secret value"
    );
    assert!(debug.contains("token"), "Debug must show the secret name");
}

/// A secret binds only to its declared name and its exact origin.
#[test]
fn secrets_bind_to_their_name_and_origin_only() {
    let policy = policy(&["api.example", "other.example"])
        .with_secret(secret("token", "https://api.example/"))
        .unwrap();
    for (name, at) in [
        ("missing", "https://api.example/"),
        ("token", "https://other.example/"),
        ("token", "https://api.example:8443/"),
    ] {
        assert_eq!(
            layer(policy.bind_secret(name, &origin(at))),
            Layer::SecretBinding,
            "{name} {at}"
        );
    }
    assert!(policy
        .bind_secret("token", &origin("https://api.example:443/path"))
        .is_ok());
}

/// The allow list comes first, then the import opt-in. Controls: the entry module, and
/// an imported module with the opt-in.
#[test]
fn origins_need_the_allow_list_then_the_import_opt_in() {
    let closed = policy(&["api.example"]);
    let open = policy(&["api.example"]).with_imports(true);
    let listed = origin("https://api.example/");
    let unlisted = origin("https://other.example/");
    for from_entry in [true, false] {
        assert_eq!(
            layer(closed.check_origin(&unlisted, from_entry)),
            Layer::AllowList
        );
        assert_eq!(
            layer(open.check_origin(&unlisted, from_entry)),
            Layer::AllowList
        );
        assert!(open.check_origin(&listed, from_entry).is_ok());
    }
    assert_eq!(
        layer(closed.check_origin(&listed, false)),
        Layer::ImportOptIn
    );
    assert!(closed.check_origin(&listed, true).is_ok());
}

/// TP-85 unit rows: a wider list or a wider import grant fails, a narrower list narrows,
/// and an equal list keeps the same grant.
#[test]
fn narrowing_never_widens_a_policy() {
    let wide = policy(&["api.example", "http://127.0.0.1:8080"])
        .with_secret(secret("token", "https://api.example/"))
        .unwrap();
    let narrowed = |items: &[&str], imports| {
        wide.narrow(items.iter().map(|text| item(text)).collect(), imports)
    };
    assert_eq!(
        narrowed(&["api.example", "other.example"], false).unwrap_err(),
        NarrowError::ItemNotAllowed { index: 1 }
    );
    assert_eq!(
        narrowed(&["api.example"], true).unwrap_err(),
        NarrowError::ImportsNotAllowed
    );
    assert_eq!(
        narrowed(&[], false).unwrap_err(),
        NarrowError::Policy(PolicyError::NoItems)
    );

    let narrower = narrowed(&["api.example"], false).unwrap();
    let local = origin("http://127.0.0.1:8080/");
    assert_eq!(layer(narrower.check_origin(&local, true)), Layer::AllowList);
    assert!(narrower
        .bind_secret("token", &origin("https://api.example/"))
        .is_ok());

    let equal = narrowed(&["http://127.0.0.1:8080", "api.example:443"], false).unwrap();
    assert!(equal.check_origin(&local, true).is_ok());
    assert!(equal
        .check_origin(&origin("https://api.example/"), true)
        .is_ok());

    let open = wide.clone().with_imports(true);
    let kept = open.narrow(vec![item("api.example")], true).unwrap();
    assert!(kept
        .check_origin(&origin("https://api.example/"), false)
        .is_ok());
    let closed = open.narrow(vec![item("api.example")], false).unwrap();
    assert_eq!(
        layer(closed.check_origin(&origin("https://api.example/"), false)),
        Layer::ImportOptIn
    );
}

/// A narrower item can not gain a port that opens a refused address. Control: an item
/// can drop that port.
#[test]
fn narrowing_cannot_add_the_port_that_opens_an_address() {
    let plain = policy(&["127.0.0.1"]);
    assert_eq!(
        plain
            .narrow(vec![item("127.0.0.1:443")], false)
            .unwrap_err(),
        NarrowError::ItemNotAllowed { index: 0 }
    );
    let explicit = policy(&["127.0.0.1:443"]);
    assert!(explicit.narrow(vec![item("127.0.0.1")], false).is_ok());
    assert!(explicit.narrow(vec![item("127.0.0.1:443")], false).is_ok());
}

/// Both edges of each refused range are refused, and each next address outside the
/// table passes.
#[test]
fn address_table_refuses_both_edges_of_each_range() {
    let open = policy(&["api.example"]);
    let refused = [
        "0.0.0.0",
        "0.255.255.255",
        "10.0.0.0",
        "10.255.255.255",
        "100.64.0.0",
        "100.127.255.255",
        "127.0.0.0",
        "127.255.255.255",
        "169.254.0.0",
        "169.254.255.255",
        "172.16.0.0",
        "172.31.255.255",
        "192.0.0.0",
        "192.0.0.255",
        "192.168.0.0",
        "192.168.255.255",
        "198.18.0.0",
        "198.19.255.255",
        "224.0.0.0",
        "239.255.255.255",
        "240.0.0.0",
        "255.255.255.254",
        "255.255.255.255",
        "[::]",
        "[::1]",
        "[::2]",
        "[::ffff:ffff]",
        "[::ffff:0.0.0.0]",
        "[::ffff:255.255.255.255]",
        "[::ffff:8.8.8.8]",
        "[::ffff:0:0:0]",
        "[::ffff:0:ffff:ffff]",
        "[2001::]",
        "[2001:0:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[2002::]",
        "[2002:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[64:ff9b::]",
        "[64:ff9b::ffff:ffff]",
        "[64:ff9b:1::]",
        "[64:ff9b:1:ffff:ffff:ffff:ffff:ffff]",
        "[fc00::]",
        "[fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[fe80::]",
        "[febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[fec0::]",
        "[feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[ff00::]",
        "[ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
    ];
    for text in refused {
        assert_eq!(
            layer(open.check_address(address(&format!("{text}:443")))),
            Layer::TransportAddress,
            "{text}"
        );
    }
    let passed = [
        "1.0.0.0",
        "9.255.255.255",
        "11.0.0.0",
        "100.63.255.255",
        "100.128.0.0",
        "126.255.255.255",
        "128.0.0.0",
        "169.253.255.255",
        "169.255.0.0",
        "172.15.255.255",
        "172.32.0.0",
        "191.255.255.255",
        "192.0.1.0",
        "192.167.255.255",
        "192.169.0.0",
        "198.17.255.255",
        "198.20.0.0",
        "223.255.255.255",
        "[::1:0:0]",
        "[::fffe:ffff:ffff]",
        "[::fffe:ffff:ffff:ffff]",
        "[::ffff:1:0:0]",
        "[::1:0:0:0]",
        "[2000:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[2001:1::]",
        "[2001:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[2003::]",
        "[64:ff9a:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[64:ff9b::1:0:0]",
        "[64:ff9b:0:ffff:ffff:ffff:ffff:ffff]",
        "[64:ff9b:2::]",
        "[fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "[fe00::]",
        "[fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
    ];
    for text in passed {
        assert!(
            open.check_address(address(&format!("{text}:443"))).is_ok(),
            "{text}"
        );
    }
}

/// A refused address passes only for an item of that exact IP literal with that exact
/// port. A mapped form never equals an IPv4 item. Controls: the exact address and port.
#[test]
fn only_an_exact_ip_item_with_its_port_opens_a_refused_address() {
    let listed = policy(&["127.0.0.1:8080", "http://[::1]:9090", "api.example"]);
    assert!(listed.check_address(address("127.0.0.1:8080")).is_ok());
    assert!(listed.check_address(address("[::1]:9090")).is_ok());
    for text in [
        "127.0.0.1:8081",
        "127.0.0.2:8080",
        "127.0.0.1:443",
        "[::ffff:127.0.0.1]:8080",
        "[::1]:8080",
        "10.0.0.1:443",
    ] {
        assert_eq!(
            layer(listed.check_address(address(text))),
            Layer::TransportAddress,
            "{text}"
        );
    }
    let no_port = policy(&["127.0.0.1"]);
    assert_eq!(
        layer(no_port.check_address(address("127.0.0.1:443"))),
        Layer::TransportAddress
    );
    let mapped = policy(&["[::ffff:127.0.0.1]:8080"]);
    assert!(mapped
        .check_address(address("[::ffff:127.0.0.1]:8080"))
        .is_ok());
    assert_eq!(
        layer(mapped.check_address(address("127.0.0.1:8080"))),
        Layer::TransportAddress
    );
}

/// TP-87 unit rows: a hop to `http://169.254.169.254/` and a hop to an unlisted host fail
/// the redirect check, and so does https to http. Control: a hop to a listed origin.
#[test]
fn redirect_hops_stay_on_the_allow_list_and_never_drop_to_http() {
    let listed = policy(&[
        "http://127.0.0.1:8080",
        "api.example",
        "http://api.example:80",
    ]);
    let refused = |from: &str, to: &str| match listed.check_hop(&url(from), &url(to), 1) {
        Err(HopError::Refused(refusal)) => refusal.layer(),
        other => panic!("{from} -> {to}: {other:?}"),
    };
    assert_eq!(
        refused("http://127.0.0.1:8080/", "http://169.254.169.254/"),
        Layer::RedirectCheck
    );
    assert_eq!(
        refused("https://api.example/", "https://other.example/"),
        Layer::RedirectCheck
    );
    assert_eq!(
        refused("https://api.example/", "http://api.example/"),
        Layer::RedirectCheck
    );
    for (from, to) in [
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/next"),
        ("http://api.example/", "https://api.example/"),
        ("https://api.example/", "https://api.example:443/next"),
    ] {
        assert!(listed.check_hop(&url(from), &url(to), 1).is_ok(), "{to}");
    }
}

#[test]
fn redirect_chains_take_the_hop_cap() {
    let listed = policy(&["api.example"]);
    let (from, to) = (url("https://api.example/a"), url("https://api.example/b"));
    assert!(listed.check_hop(&from, &to, MAX_REDIRECTS).is_ok());
    assert_eq!(
        listed.check_hop(&from, &to, MAX_REDIRECTS + 1).unwrap_err(),
        HopError::TooMany
    );
}

/// A hop to another origin drops each secret header and each credential header in any
/// letter case. Controls: a same-origin hop keeps them, and a plain header goes on.
#[test]
fn cross_origin_hops_drop_secret_and_credential_headers() {
    let listed = policy(&["api.example", "other.example"]);
    let start = url("https://api.example/a");
    let same = listed
        .check_hop(&start, &url("https://api.example:443/b"), 1)
        .unwrap();
    let cross = listed
        .check_hop(&start, &url("https://other.example/b"), 1)
        .unwrap();
    for name in [
        "Authorization",
        "cookie",
        "PROXY-AUTHORIZATION",
        "X-Api-Key",
    ] {
        assert!(same.keeps_header(name, false), "{name}");
        assert!(!cross.keeps_header(name, false), "{name}");
    }
    assert!(same.keeps_header("X-Token", true));
    assert!(!cross.keeps_header("X-Token", true));
    assert!(cross.keeps_header("X-Trace", false));
}

#[test]
fn layer_names_are_distinct_kebab_case_words() {
    let names = [
        Layer::AllowList,
        Layer::ImportOptIn,
        Layer::UrlParser,
        Layer::RedirectCheck,
        Layer::SecretBinding,
        Layer::TransportAddress,
        Layer::ResponseCheck,
        Layer::Method,
    ]
    .map(Layer::name);
    assert_eq!(
        names,
        [
            "allow-list",
            "import-opt-in",
            "url-parser",
            "redirect-check",
            "secret-binding",
            "transport-address",
            "response-check",
            "method",
        ]
    );
}

/// This release sends GET and HEAD only. Every other declared method is refused with
/// the method layer.
#[test]
fn only_get_and_head_pass_the_method_check() {
    use super::decl::DeclaredMethod;
    use super::request::Method;
    let policy = NetworkPolicy::new(vec![AllowItem::parse("a.example").unwrap()]).unwrap();
    assert_eq!(policy.check_method(DeclaredMethod::Get), Ok(Method::Get));
    assert_eq!(policy.check_method(DeclaredMethod::Head), Ok(Method::Head));
    for method in [
        DeclaredMethod::Post,
        DeclaredMethod::Put,
        DeclaredMethod::Patch,
        DeclaredMethod::Delete,
        DeclaredMethod::Options,
    ] {
        let refusal = policy.check_method(method).unwrap_err();
        assert_eq!(refusal.layer(), Layer::Method);
        assert_eq!(
            refusal.reason(),
            "this release sends GET and HEAD requests only"
        );
    }
}
