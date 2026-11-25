use std::net::{Ipv4Addr, Ipv6Addr};

use super::url::{parse_absolute, parse_location, CheckedUrl, Host, Scheme, UrlRefusal};
use crate::limits::MAX_URL_BYTES;

fn refused(text: &str) -> UrlRefusal {
    match parse_absolute(text) {
        Ok(url) => panic!("{text:?} was accepted as {url:?}"),
        Err(refusal) => refusal,
    }
}

fn accepted(text: &str) -> CheckedUrl {
    parse_absolute(text).unwrap_or_else(|refusal| panic!("{text:?}: {}", refusal.reason()))
}

#[test]
fn accepts_a_name_an_ipv4_literal_and_a_bracketed_ipv6_literal() {
    let name = accepted("https://a.example/x?q=1");
    assert_eq!(name.scheme, Scheme::Https);
    assert_eq!(name.host, Host::Name("a.example".to_owned()));
    assert_eq!(name.port, None);
    assert_eq!(name.path, "/x");
    assert_eq!(name.query.as_deref(), Some("q=1"));
    assert_eq!(name.origin().to_string(), "https://a.example");

    let v4 = accepted("http://127.0.0.1:8080/");
    assert_eq!(v4.scheme, Scheme::Http);
    assert_eq!(v4.host, Host::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(v4.port, Some(8080));
    assert_eq!((v4.path.as_str(), v4.query.as_deref()), ("/", None));
    assert_eq!(v4.origin().to_string(), "http://127.0.0.1:8080");

    let v6 = accepted("https://[2001:db8::1]:8443/p");
    let address: Ipv6Addr = "2001:db8::1".parse().unwrap();
    assert_eq!(v6.host, Host::V6(address));
    assert_eq!(v6.port, Some(8443));
    assert_eq!(v6.origin().to_string(), "https://[2001:db8::1]:8443");
}

#[test]
fn an_explicit_default_port_names_the_same_origin_as_no_port() {
    let explicit = accepted("https://a.example:443/");
    assert_eq!(explicit.port, Some(443));
    assert_eq!(explicit.origin(), accepted("https://a.example/").origin());
    assert_eq!(explicit.origin().to_string(), "https://a.example");
    assert_ne!(
        accepted("http://a.example/").origin(),
        accepted("https://a.example/").origin()
    );
}

#[test]
fn a_url_with_no_path_has_the_root_path_and_a_query_needs_a_path() {
    let bare = accepted("https://a.example");
    assert_eq!((bare.path.as_str(), bare.query.as_deref()), ("/", None));
    assert_eq!(refused("https://a.example?q=1"), UrlRefusal::Path);
    assert_eq!(accepted("https://a.example/?").query.as_deref(), Some(""));
}

#[test]
fn only_the_lower_case_https_and_http_schemes_are_accepted() {
    for text in [
        "ftp://a.example/",
        "HTTPS://a.example/",
        "Http://a.example/",
        "https:/a.example/",
        "https:a.example/",
        "//a.example/",
        "a.example/",
    ] {
        assert_eq!(refused(text), UrlRefusal::Scheme, "{text}");
    }
}

#[test]
fn user_information_backslashes_and_fragments_are_refused_anywhere() {
    for text in [
        "https://user@a.example/",
        "https://a.example/@me",
        "https://a.example\\b.example/",
        "https://a.example/x\\y",
        "https://a.example/#top",
        "https://a.example/x?q=1#top",
    ] {
        assert_eq!(refused(text), UrlRefusal::ForbiddenChar, "{text}");
    }
}

#[test]
fn only_visible_ascii_is_accepted() {
    for text in [
        "https://a.example/a b",
        "https://a.example/\t",
        "https://a.example/\r\n",
        "https://a.example/\u{7f}",
        "https://a.example/\u{e9}",
        "https://\u{430}.example/",
    ] {
        assert_eq!(refused(text), UrlRefusal::NotVisibleAscii, "{text:?}");
    }
}

#[test]
fn the_length_cap_admits_a_url_of_exactly_the_cap() {
    let prefix = "https://a.example/";
    let at_cap = format!("{prefix}{}", "a".repeat(MAX_URL_BYTES - prefix.len()));
    assert_eq!(at_cap.len(), MAX_URL_BYTES);
    assert!(parse_absolute(&at_cap).is_ok());
    let over = format!("{at_cap}a");
    assert_eq!(refused(&over), UrlRefusal::TooLong);
    // The cap is checked first: an over-cap URL with another fault gets the length reason.
    assert_eq!(refused(&format!("{over} ")), UrlRefusal::TooLong);
    assert!(UrlRefusal::TooLong
        .reason()
        .contains(&MAX_URL_BYTES.to_string()));
}

#[test]
fn host_names_are_lower_case_letter_digit_hyphen_labels() {
    for (text, refusal) in [
        ("https:///x", UrlRefusal::EmptyHost),
        ("https://:443/", UrlRefusal::EmptyHost),
        ("https://a..example/", UrlRefusal::HostName),
        ("https://.a.example/", UrlRefusal::HostName),
        ("https://A.example/", UrlRefusal::HostName),
        ("https://-a.example/", UrlRefusal::HostName),
        ("https://a-.example/", UrlRefusal::HostName),
        ("https://a_b.example/", UrlRefusal::HostName),
        ("https://a.example./", UrlRefusal::TrailingDot),
        ("https://a.example.:443/", UrlRefusal::TrailingDot),
    ] {
        assert_eq!(refused(text), refusal, "{text}");
    }
    let label = "a".repeat(63);
    assert!(parse_absolute(&format!("https://{label}.example/")).is_ok());
    assert_eq!(
        refused(&format!("https://{label}a.example/")),
        UrlRefusal::HostName
    );
    assert!(parse_absolute("https://xn--d1acufc.xn--p1ai/").is_ok());
    assert!(parse_absolute("https://a-b.example/").is_ok());
}

#[test]
fn a_host_name_has_at_most_253_bytes() {
    let label = "a".repeat(61);
    // Four labels of 61 bytes, four dots and a final label of 5 bytes: 253 bytes.
    let at_cap = format!("{label}.{label}.{label}.{label}.abcde");
    assert_eq!(at_cap.len(), 253);
    assert!(parse_absolute(&format!("https://{at_cap}/")).is_ok());
    assert_eq!(
        refused(&format!("https://{at_cap}g/")),
        UrlRefusal::HostTooLong
    );
}

#[test]
fn ipv4_literals_need_four_canonical_decimal_parts() {
    for text in [
        "https://127.1/",
        "https://2130706433/",
        "https://0x7f000001/",
        "https://0x7f.0.0.1/",
        "https://0177.0.0.1/",
        "https://127.0.0.01/",
        "https://1.2.3.256/",
        "https://1.2.3.4.5/",
        "https://1.2.3./",
        "https://a.example.123/",
        "https://a.0x1/",
        "https://a.0x/",
    ] {
        let expected = if text.ends_with("./") {
            UrlRefusal::TrailingDot
        } else {
            UrlRefusal::Ipv4
        };
        assert_eq!(refused(text), expected, "{text}");
    }
    assert_eq!(
        accepted("https://10.0.0.255/").host,
        Host::V4(Ipv4Addr::new(10, 0, 0, 255))
    );
    assert_eq!(
        accepted("https://0.0.0.0/").host,
        Host::V4(Ipv4Addr::UNSPECIFIED)
    );
    // A label that is hex letters only is a name: only a `0x` label reads as a number.
    assert!(matches!(accepted("https://cafe.beef/").host, Host::Name(_)));
}

#[test]
fn ipv6_literals_need_brackets_and_no_zone() {
    for (text, refusal) in [
        ("https://[::1%25eth0]/", UrlRefusal::Zone),
        ("https://[fe80::1%251]/", UrlRefusal::Zone),
        ("https://[::1/", UrlRefusal::Ipv6),
        ("https://[not-v6]/", UrlRefusal::Ipv6),
        ("https://[::1]x/", UrlRefusal::Ipv6),
        ("https://[127.0.0.1]/", UrlRefusal::Ipv6),
        ("https://::1/", UrlRefusal::EmptyHost),
    ] {
        assert_eq!(refused(text), refusal, "{text}");
    }
    let mapped = accepted("https://[::ffff:127.0.0.1]/");
    assert_eq!(mapped.host, Host::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()));
    assert_eq!(accepted("https://[::1]/").port, None);
}

#[test]
fn ports_are_decimal_from_1_to_65535() {
    for text in [
        "https://a.example:0/",
        "https://a.example:65536/",
        "https://a.example:/",
        "https://a.example:+80/",
        "https://a.example:80a/",
        "https://a.example:123456/",
        "https://a.example:1:2/",
        "https://[::1]:/",
        "https://[::1]:0/",
    ] {
        assert_eq!(refused(text), UrlRefusal::Port, "{text}");
    }
    assert_eq!(accepted("https://a.example:65535/").port, Some(65535));
    assert_eq!(accepted("https://a.example:1/").port, Some(1));
}

#[test]
fn each_percent_sign_needs_two_hex_digits() {
    for text in [
        "https://a.example/%zz",
        "https://a.example/%2",
        "https://a.example/%",
        "https://a.example/x?y=%",
        "https://a.example/x?y=%g0",
    ] {
        assert_eq!(refused(text), UrlRefusal::Percent, "{text}");
    }
    assert_eq!(accepted("https://a.example/%2F%2e").path, "/%2F%2e");
}

#[test]
fn a_location_is_an_absolute_url_or_an_absolute_path_on_the_base_origin() {
    let base = accepted("https://a.example:8443/x?y=1");
    let path = parse_location(&base, "/next?page=2").unwrap();
    assert_eq!(path.origin(), base.origin());
    assert_eq!(path.path, "/next");
    assert_eq!(path.query.as_deref(), Some("page=2"));

    let absolute = parse_location(&base, "http://b.example/z").unwrap();
    assert_eq!(absolute.origin().to_string(), "http://b.example");

    for text in ["//b.example/z", "next", "../x", "./x", "?q=1", ""] {
        assert_eq!(
            parse_location(&base, text).unwrap_err(),
            UrlRefusal::NotAbsolute,
            "{text}"
        );
    }
    // Each form goes through the one strict parser.
    for (text, refusal) in [
        ("/\\b.example/", UrlRefusal::ForbiddenChar),
        ("/x#top", UrlRefusal::ForbiddenChar),
        ("/a b", UrlRefusal::NotVisibleAscii),
        ("javascript:alert", UrlRefusal::Scheme),
        ("https://127.1/", UrlRefusal::Ipv4),
    ] {
        assert_eq!(parse_location(&base, text).unwrap_err(), refusal, "{text}");
    }
}

#[test]
fn an_absolute_path_location_counts_the_base_origin_in_the_length_cap() {
    let base = accepted("https://a.example/");
    let origin_len = "https://a.example".len();
    let fits = format!("/{}", "a".repeat(MAX_URL_BYTES - origin_len - 1));
    assert!(parse_location(&base, &fits).is_ok());
    let over = format!("{fits}a");
    assert_eq!(
        parse_location(&base, &over).unwrap_err(),
        UrlRefusal::TooLong
    );
}

#[test]
fn each_refusal_has_its_own_fixed_reason() {
    let all = [
        UrlRefusal::TooLong,
        UrlRefusal::NotVisibleAscii,
        UrlRefusal::ForbiddenChar,
        UrlRefusal::Percent,
        UrlRefusal::Scheme,
        UrlRefusal::EmptyHost,
        UrlRefusal::HostName,
        UrlRefusal::TrailingDot,
        UrlRefusal::HostTooLong,
        UrlRefusal::Ipv4,
        UrlRefusal::Ipv6,
        UrlRefusal::Zone,
        UrlRefusal::Port,
        UrlRefusal::Path,
        UrlRefusal::NotAbsolute,
    ];
    let mut reasons: Vec<&str> = all.iter().map(|refusal| refusal.reason()).collect();
    assert!(reasons.iter().all(|reason| !reason.is_empty()));
    reasons.sort_unstable();
    reasons.dedup();
    assert_eq!(reasons.len(), all.len());
}
