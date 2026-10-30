#![cfg_attr(coverage_nightly, coverage(off))]

use std::net::SocketAddr;
use std::time::Duration;

use http::{HeaderMap, HeaderValue, Request, Version, header};

use super::{keep_alive_timeout, shape};
use crate::body::empty_body;
use crate::target::{Target, Transport};

fn target(transport: Transport, port: u16) -> Target {
    Target::new(
        SocketAddr::from(([10, 0, 0, 1], port)),
        "api.example.com",
        transport,
    )
}

fn req(uri: &str, host: Option<&str>) -> Request<crate::ProxyBody> {
    let mut b = Request::builder().uri(uri);
    if let Some(h) = host {
        b = b.header(header::HOST, h);
    }
    b.body(empty_body()).unwrap()
}

#[test]
fn h1_uses_origin_form_and_host_header() {
    let r = shape(
        req("https://api.example.com/v1/chat?x=1", None),
        &target(Transport::Tls, 443),
        Version::HTTP_11,
    )
    .unwrap();
    assert_eq!(r.uri().to_string(), "/v1/chat?x=1");
    assert_eq!(r.headers()[header::HOST], "api.example.com");
    assert_eq!(r.version(), Version::HTTP_11);
}

#[test]
fn h1_prefers_explicit_host_header() {
    let r = shape(
        req("/v1", Some("override.example:8443")),
        &target(Transport::Tls, 443),
        Version::HTTP_11,
    )
    .unwrap();
    assert_eq!(r.headers()[header::HOST], "override.example:8443");
}

#[test]
fn h1_falls_back_to_target_authority() {
    let r = shape(
        req("/v1", None),
        &target(Transport::Plain, 8080),
        Version::HTTP_11,
    )
    .unwrap();
    assert_eq!(r.headers()[header::HOST], "api.example.com:8080");
}

#[test]
fn h2_uses_absolute_form_without_host() {
    let r = shape(
        req("/v1?q", Some("api.example.com")),
        &target(Transport::Tls, 443),
        Version::HTTP_2,
    )
    .unwrap();
    assert_eq!(r.uri().to_string(), "https://api.example.com/v1?q");
    assert!(r.headers().get(header::HOST).is_none());
    assert_eq!(r.version(), Version::HTTP_2);
}

#[test]
fn shaping_is_idempotent_across_versions() {
    let t = target(Transport::Tls, 443);
    let h2 = shape(req("/a?b", Some("api.example.com")), &t, Version::HTTP_2).unwrap();
    let h1 = shape(h2, &t, Version::HTTP_11).unwrap();
    assert_eq!(h1.uri().to_string(), "/a?b");
    assert_eq!(h1.headers()[header::HOST], "api.example.com");
    let h2_again = shape(h1, &t, Version::HTTP_2).unwrap();
    assert_eq!(h2_again.uri().to_string(), "https://api.example.com/a?b");
}

#[test]
fn percent_encoded_crlf_stays_encoded() {
    let r = shape(
        req("/a%0d%0aInjected:%20x", None),
        &target(Transport::Tls, 443),
        Version::HTTP_11,
    )
    .unwrap();
    assert_eq!(r.uri().path(), "/a%0d%0aInjected:%20x");
}

fn keep_alive(values: &[&str]) -> Option<Duration> {
    let mut h = HeaderMap::new();
    for v in values {
        h.append("keep-alive", HeaderValue::from_str(v).unwrap());
    }
    keep_alive_timeout(&h)
}

#[test]
fn keep_alive_timeout_is_read_from_real_world_spellings() {
    let secs = |n| Some(Duration::from_secs(n));
    let cases: &[(&[&str], Option<Duration>)] = &[
        (&["timeout=5"], secs(5)),
        (&["timeout=5, max=1000"], secs(5)),
        (&["max=1000, timeout=5"], secs(5)),
        (&[" timeout = 5 , max = 1000 "], secs(5)),
        (&["Timeout=5"], secs(5)),
        (&["timeout=\"5\""], secs(5)),
        // Several header lines; the first usable timeout counts.
        (&["max=10", "timeout=7"], secs(7)),
        (&["timeout=abc, timeout=5"], secs(5)),
        (&["timeout=0"], secs(0)),
        (&["timeout=18446744073709551615"], secs(u64::MAX)),
        // Nothing usable: the pool's own idle timeout applies.
        (&["max=1000"], None),
        (&["foo=bar"], None),
        (&[""], None),
        (&["timeout"], None),
        (&["timeout="], None),
        (&["timeout=-1"], None),
        (&["timeout=5s"], None),
        (&["timeout=18446744073709551616"], None),
    ];
    for (values, expected) in cases {
        assert_eq!(keep_alive(values), *expected, "{values:?}");
    }
    assert_eq!(keep_alive_timeout(&HeaderMap::new()), None);
}
