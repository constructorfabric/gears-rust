#![cfg_attr(coverage_nightly, coverage(off))]

use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, StatusCode};
use proptest::prelude::*;

use super::*;

fn map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.append(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}

#[test]
fn request_framing_and_control_headers_are_dropped() {
    let mut h = map(&[
        ("content-length", "29"),
        ("transfer-encoding", "identity, chunked"),
        ("expect", "100-continue"),
        ("http2-settings", "AAMAAABkAAQAoAAAAAIAAAAA"),
        ("upgrade", "h2c"),
        ("connection", "Upgrade, HTTP2-Settings, x-secret"),
        ("x-secret", "1"),
        ("keep-alive", "timeout=5"),
        ("proxy-connection", "keep-alive"),
        ("proxy-authorization", "Basic Zm9v"),
        ("te", "gzip"),
        ("trailer", "x-t, x-secret, content-length"),
        ("x-keep", "yes"),
    ]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert_eq!(h.len(), 2, "only x-keep and the declaration survive: {h:?}");
    assert_eq!(h["x-keep"], "yes");
    assert_eq!(h["trailer"], "x-t");
}

#[test]
fn request_trailer_declaration_loses_names_trailers_cannot_carry() {
    let mut h = map(&[
        ("trailer", "X-Checksum, Host"),
        ("trailer", "proxy-authorization, grpc-status"),
    ]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert_eq!(h["trailer"], "X-Checksum, grpc-status");
    assert_eq!(h.get_all("trailer").iter().count(), 1);

    let mut h = map(&[("trailer", "transfer-encoding, te")]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert!(h.get("trailer").is_none(), "nothing left to declare");

    // `Connection` may nominate `Trailer` itself.
    let mut h = map(&[("connection", "trailer"), ("trailer", "x-checksum")]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert!(h.get("trailer").is_none());
}

/// Request trailers cannot re-supply credential, routing or representation
/// fields: an upstream merging trailers into the head would take them as
/// head fields. Response trailers keep them.
#[test]
fn request_trailers_lose_credential_and_forwarding_fields() {
    let names = "authorization, cookie, content-type, forwarded, via, \
                 x-forwarded-for, x-forwarded-host, x-real-ip, x-checksum";
    let mut h = map(&[("trailer", names)]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert_eq!(h["trailer"], "x-checksum");

    let all = || {
        map(&[
            ("authorization", "Bearer x"),
            ("cookie", "s=1"),
            ("x-forwarded-for", "203.0.113.7"),
            ("x-checksum", "abc"),
        ])
    };
    let mut t = all();
    sanitize_request_trailers(&mut t);
    assert_eq!(
        t.keys().map(HeaderName::as_str).collect::<Vec<_>>(),
        ["x-checksum"]
    );

    let mut t = all();
    sanitize_trailers(&mut t);
    assert_eq!(t.len(), 4, "response trailers keep them: {t:?}");
}

/// Request trailers cannot carry request modifiers either: an upstream
/// merging them into the head would turn a `PUT` into a conditional request
/// or a `GET` into a range request.
#[test]
fn request_trailers_lose_request_modifiers() {
    let names = "If-Match, if-none-match, if-modified-since, if-unmodified-since, \
                 if-range, Range, pragma, expect, http2-settings, x-checksum";
    let mut h = map(&[("trailer", names)]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert_eq!(h["trailer"], "x-checksum");

    let mut t = map(&[
        ("if-match", "*"),
        ("if-none-match", "*"),
        ("if-unmodified-since", "Sat, 29 Oct 1994 19:43:31 GMT"),
        ("range", "bytes=0-1"),
        ("pragma", "no-cache"),
        ("expect", "100-continue"),
        ("x-checksum", "abc"),
    ]);
    sanitize_request_trailers(&mut t);
    assert_eq!(
        t.keys().map(HeaderName::as_str).collect::<Vec<_>>(),
        ["x-checksum"]
    );
}

/// A response keeps the credential and representation names in its
/// declaration: only requests lose them.
#[test]
fn response_declaration_keeps_credential_and_representation_names() {
    let mut h = map(&[(
        "trailer",
        "Set-Cookie, Content-Type, Authorization, X-Checksum",
    )]);
    sanitize_response_headers(&mut h, StatusCode::OK);
    assert_eq!(
        h["trailer"],
        "Set-Cookie, Content-Type, Authorization, X-Checksum"
    );
}

/// A head with `Connection: a,a,...` and `Trailer: b,b,...`, `n`
/// one-character tokens each. Every declared name misses every nominated
/// one, the worst case for a filter that scans the nominated list.
fn long_connection_and_declaration(n: usize) -> HeaderMap {
    map(&[
        ("connection", &vec!["a"; n].join(",")),
        ("trailer", &vec!["b"; n].join(",")),
    ])
}

/// How much longer `sanitize` takes on [`long_connection_and_declaration`]
/// with `8 * n` tokens than with `n`, best of five runs each: about 8 when
/// the cost is linear, about 64 when it is quadratic. A ratio does not
/// depend on the machine's speed or the build profile.
fn growth_for_8x_tokens(n: usize, sanitize: impl Fn(&mut HeaderMap)) -> f64 {
    let best = |n: usize| {
        (0..5)
            .map(|_| {
                let mut h = long_connection_and_declaration(n);
                let started = std::time::Instant::now();
                sanitize(&mut h);
                let elapsed = started.elapsed();
                assert_eq!(h["trailer"].len(), 3 * n - 2, "every `b` is kept");
                elapsed
            })
            .min()
            .unwrap_or_default()
    };
    best(8 * n).as_secs_f64() / best(n).as_secs_f64().max(1e-9)
}

/// A long `Connection` list and a long declaration cost linear time, on
/// requests and on responses. Both fields come from outside: the caller's
/// request and the upstream's response head. With 16384 tokens each (two
/// ~32 KiB fields, within the 64 KiB response head cap) a quadratic filter
/// makes 2.7e8 comparisons per message.
#[test]
fn a_long_connection_list_and_declaration_stay_linear() {
    let request = growth_for_8x_tokens(2048, |h| {
        sanitize_request_headers(h, RequestKind::Regular);
    });
    assert!(request < 24.0, "request: x{request:.1} for 8x the tokens");
    let response = growth_for_8x_tokens(2048, |h| {
        let _nominated = sanitize_response_head(h, StatusCode::OK);
    });
    assert!(
        response < 24.0,
        "response: x{response:.1} for 8x the tokens"
    );
}

#[test]
fn websocket_upgrade_drops_the_trailer_declaration() {
    let mut h = map(&[
        ("connection", "Upgrade"),
        ("upgrade", "websocket"),
        ("trailer", "x-checksum"),
    ]);
    sanitize_request_headers(&mut h, RequestKind::WebSocketUpgrade);
    assert!(h.get("trailer").is_none());
}

#[test]
fn te_trailers_survives() {
    let mut h = map(&[("te", "trailers")]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert_eq!(h["te"], "trailers");
}

#[test]
fn websocket_upgrade_reemits_canonical_pair() {
    let mut h = map(&[
        ("connection", "keep-alive, Upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ("sec-websocket-version", "13"),
        ("content-length", "0"),
    ]);
    sanitize_request_headers(&mut h, RequestKind::WebSocketUpgrade);
    assert_eq!(h["connection"], "upgrade");
    assert_eq!(h["upgrade"], "websocket");
    assert!(h.get("content-length").is_none());
    assert!(h.get("sec-websocket-key").is_some());
}

#[test]
fn response_hop_by_hop_and_nominated_are_dropped() {
    let mut h = map(&[
        ("connection", "close, x-internal"),
        ("x-internal", "1"),
        ("trailer", "x-checksum"),
        ("keep-alive", "timeout=5"),
        ("content-length", "12"),
        ("content-type", "application/json"),
    ]);
    sanitize_response_headers(&mut h, StatusCode::OK);
    assert!(h.get("connection").is_none());
    assert!(h.get("x-internal").is_none());
    assert!(h.get("keep-alive").is_none());
    assert_eq!(h["content-length"], "12");
    assert_eq!(h["content-type"], "application/json");
    // End-to-end (RFC 9110 §6.6.2): tells an HTTP/1.1 recipient what follows.
    assert_eq!(h["trailer"], "x-checksum");
}

#[test]
fn response_trailer_declaration_loses_names_trailers_cannot_carry() {
    let mut h = map(&[
        ("trailer", "Content-Length, X-Checksum"),
        ("trailer", "host, grpc-status"),
    ]);
    sanitize_response_headers(&mut h, StatusCode::OK);
    assert_eq!(h["trailer"], "X-Checksum, grpc-status");
    assert_eq!(h.get_all("trailer").iter().count(), 1);

    let mut h = map(&[("trailer", "transfer-encoding, connection")]);
    sanitize_response_headers(&mut h, StatusCode::OK);
    assert!(h.get("trailer").is_none(), "nothing left to declare");
}

#[test]
fn trailers_keep_application_fields_only() {
    let mut t = map(&[
        ("grpc-status", "0"),
        ("grpc-message", "ok"),
        ("x-checksum", "abc"),
        ("content-length", "5"),
        ("transfer-encoding", "chunked"),
        ("connection", "x-hop"),
        ("x-hop", "1"),
        ("keep-alive", "timeout=5"),
        ("te", "trailers"),
        ("trailer", "x-checksum"),
        ("upgrade", "websocket"),
        ("host", "evil.example"),
    ]);
    sanitize_trailers(&mut t);
    let mut left: Vec<&str> = t.keys().map(HeaderName::as_str).collect();
    left.sort_unstable();
    assert_eq!(left, ["grpc-message", "grpc-status", "x-checksum"]);
}

#[test]
fn trailers_lose_proxy_hop_fields_and_what_the_head_nominated() {
    let mut head = map(&[
        ("connection", "X-Hop"),
        ("trailer", "X-Hop, X-Checksum, Proxy-Authorization"),
    ]);
    let hop = sanitize_response_head(&mut head, StatusCode::OK);
    assert_eq!(hop, [HeaderName::from_static("x-hop")]);
    assert_eq!(head["trailer"], "X-Checksum");

    let mut t = map(&[
        ("x-hop", "1"),
        ("x-checksum", "abc"),
        ("proxy-authorization", "Basic x"),
        ("proxy-authenticate", "Basic"),
        ("proxy-authentication-info", "nextnonce=1"),
        ("authentication-info", "nextnonce=2"),
    ]);
    sanitize_trailers_without(&mut t, &hop);
    let mut left: Vec<&str> = t.keys().map(HeaderName::as_str).collect();
    left.sort_unstable();
    // End-to-end `Authentication-Info` legitimately rides in trailers.
    assert_eq!(left, ["authentication-info", "x-checksum"]);
}

#[test]
fn bodiless_statuses_lose_their_length() {
    for status in [
        StatusCode::NO_CONTENT,
        StatusCode::CONTINUE,
        StatusCode::EARLY_HINTS,
    ] {
        let mut h = map(&[("content-length", "10")]);
        sanitize_response_headers(&mut h, status);
        assert!(h.get("content-length").is_none(), "{status}");
    }
    // A 304 (and a response to HEAD, which is a 200 here) keeps the length
    // of the representation it describes.
    for status in [StatusCode::NOT_MODIFIED, StatusCode::OK] {
        let mut h = map(&[("content-length", "10")]);
        sanitize_response_headers(&mut h, status);
        assert_eq!(h["content-length"], "10", "{status}");
    }
}

#[test]
fn response_content_length_is_dropped_next_to_transfer_encoding() {
    let mut h = map(&[("transfer-encoding", "chunked"), ("content-length", "100")]);
    sanitize_response_headers(&mut h, StatusCode::OK);
    assert!(h.get("content-length").is_none());
    assert!(h.get("transfer-encoding").is_none());
}

#[test]
fn switching_protocols_keeps_upgrade_pair_only() {
    let mut h = map(&[
        ("connection", "Upgrade, x-internal"),
        ("upgrade", "websocket"),
        ("x-internal", "1"),
        ("keep-alive", "timeout=5"),
        ("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
    ]);
    sanitize_response_headers(&mut h, StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(h["connection"], "Upgrade, x-internal");
    assert_eq!(h["upgrade"], "websocket");
    assert!(h.get("x-internal").is_none());
    assert!(h.get("keep-alive").is_none());
    assert!(h.get("sec-websocket-accept").is_some());
}

#[test]
fn websocket_detection_is_token_based() {
    assert!(is_websocket_upgrade(&map(&[
        ("upgrade", "WebSocket"),
        ("connection", "keep-alive, upgrade"),
    ])));
    assert!(!is_websocket_upgrade(&map(&[("upgrade", "websocket")])));
    assert!(!is_websocket_upgrade(&map(&[
        ("upgrade", "h2c"),
        ("connection", "upgrade"),
    ])));
}

#[test]
fn upgrade_validation_allowlist() {
    let ok = map(&[("upgrade", "websocket"), ("connection", "Upgrade")]);
    assert!(validate_websocket_upgrade(&Method::GET, &ok, true).is_ok());

    let post = validate_websocket_upgrade(&Method::POST, &ok, true).unwrap_err();
    assert_eq!(post.kind(), ErrorKind::InvalidRequest);

    let body = validate_websocket_upgrade(&Method::GET, &ok, false).unwrap_err();
    assert_eq!(body.kind(), ErrorKind::InvalidRequest);

    for bad in [
        map(&[
            ("upgrade", "h2c"),
            ("connection", "Upgrade, HTTP2-Settings"),
        ]),
        map(&[("upgrade", "websocket, h2c"), ("connection", "Upgrade")]),
        map(&[
            ("upgrade", "websocket"),
            ("upgrade", "foo"),
            ("connection", "Upgrade"),
        ]),
        map(&[("upgrade", "websocket")]),
    ] {
        assert!(
            validate_websocket_upgrade(&Method::GET, &bad, true).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn switch_validation_requires_websocket() {
    assert!(
        validate_websocket_switch(&map(&[("upgrade", "websocket"), ("connection", "Upgrade")]))
            .is_ok()
    );
    let err = validate_websocket_switch(&map(&[("upgrade", "foo"), ("connection", "Upgrade")]))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Protocol);
}

fn header_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("content-length".to_owned()),
        Just("transfer-encoding".to_owned()),
        Just("Transfer-Encoding".to_owned()),
        Just("connection".to_owned()),
        Just("expect".to_owned()),
        Just("upgrade".to_owned()),
        Just("te".to_owned()),
        Just("keep-alive".to_owned()),
        Just("http2-settings".to_owned()),
        Just("trailer".to_owned()),
        Just("authorization".to_owned()),
        Just("if-match".to_owned()),
        Just("range".to_owned()),
        Just("x-forwarded-for".to_owned()),
        "[a-z][a-z0-9-]{0,15}",
    ]
}

fn header_value() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("chunked".to_owned()),
        Just("identity, chunked".to_owned()),
        Just("close, content-length".to_owned()),
        Just("100-continue".to_owned()),
        Just("x-a, Content-Length, host, te".to_owned()),
        Just("Authorization, cookie, Set-Cookie, cache-control, content-encoding".to_owned()),
        Just("content-range, Content-Type, max-forwards, forwarded, via".to_owned()),
        Just("X-Forwarded-Proto, x-real-ip, x-checksum".to_owned()),
        Just("If-Match, if-none-match, If-Modified-Since, if-unmodified-since".to_owned()),
        Just("if-range, Range, pragma, expect, http2-settings, x-checksum".to_owned()),
        "[ -~&&[^\r\n]]{0,24}",
    ]
}

/// What a request's trailer declaration never keeps beyond
/// `TRAILER_DROPPED`, written out here so the property does not reuse the
/// sanitizer's own predicate.
const REQUEST_NEVER_DECLARED: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "cache-control",
    "content-encoding",
    "content-range",
    "content-type",
    "max-forwards",
    "forwarded",
    "via",
    "x-real-ip",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    "if-range",
    "range",
    "pragma",
    "expect",
    "http2-settings",
];

/// Names request trailers never keep, from their own definition (not the
/// sanitizer's): framing, hop-scoped and proxy-hop fields and `Host`. The
/// request-only names are in [`REQUEST_NEVER_DECLARED`].
const REQUEST_TRAILER_NEVER_KEPT: &[&str] = &[
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "host",
    "proxy-authorization",
    "proxy-authenticate",
    "proxy-authentication-info",
];

proptest! {
    /// Whatever the request trailers and the head's nominated fields, no
    /// framing, hop-scoped, nominated or request-only field survives, and
    /// every other field does.
    #[test]
    fn request_trailers_keep_only_application_fields(
        pairs in proptest::collection::vec((header_name(), header_value()), 0..24),
        hop in proptest::collection::vec("[a-z][a-z0-9-]{0,8}", 0..4),
    ) {
        let mut t = HeaderMap::new();
        for (k, v) in &pairs {
            if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                t.append(k, v);
            }
        }
        let hop: Vec<HeaderName> = hop.iter().filter_map(|h| HeaderName::from_bytes(h.as_bytes()).ok()).collect();
        let nominated: Vec<String> = t
            .get_all("connection")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let dropped = |name: &str| {
            REQUEST_TRAILER_NEVER_KEPT.contains(&name)
                || REQUEST_NEVER_DECLARED.contains(&name)
                || name.starts_with("x-forwarded-")
                || hop.iter().any(|h| h.as_str() == name)
                || nominated.iter().any(|n| n == name)
        };
        let expected: Vec<String> = t
            .keys()
            .map(|k| k.as_str().to_owned())
            .filter(|k| !dropped(k))
            .collect();
        sanitize_request_trailers_without(&mut t, &hop);
        let mut kept: Vec<String> = t.keys().map(|k| k.as_str().to_owned()).collect();
        let mut expected = expected;
        kept.sort_unstable();
        expected.sort_unstable();
        prop_assert_eq!(kept, expected);
    }
}

proptest! {
    /// Whatever the inbound headers, no framing, control or hop-by-hop
    /// header and nothing a `Connection` header named survives.
    #[test]
    fn no_framing_or_hop_by_hop_survives(
        pairs in proptest::collection::vec((header_name(), header_value()), 0..24),
        websocket in any::<bool>(),
    ) {
        let mut h = HeaderMap::new();
        for (k, v) in &pairs {
            if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                h.append(k, v);
            }
        }
        let nominated: Vec<String> = h
            .get_all("connection")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|s| s.split(','))
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        let kind = if websocket { RequestKind::WebSocketUpgrade } else { RequestKind::Regular };

        sanitize_request_headers(&mut h, kind);

        for name in ["content-length", "transfer-encoding", "expect", "http2-settings",
                     "keep-alive", "proxy-connection"] {
            prop_assert!(h.get(name).is_none(), "{name} survived");
        }
        // A request keeps its trailer declaration minus the names trailers
        // must not carry; an upgrade has no body and loses it.
        prop_assert!(h.get_all("trailer").iter().count() <= 1);
        if let Some(declared) = h.get("trailer") {
            prop_assert!(!websocket, "an upgrade kept its trailer declaration");
            for token in declared.to_str().unwrap().split(',') {
                let token = token.trim().to_ascii_lowercase();
                prop_assert!(HeaderName::from_bytes(token.as_bytes()).is_ok(), "declared {token}");
                prop_assert!(
                    !REQUEST_TRAILER_NEVER_KEPT.contains(&token.as_str())
                        && !REQUEST_NEVER_DECLARED.contains(&token.as_str())
                        && !token.starts_with("x-forwarded-"),
                    "declared {token}"
                );
                prop_assert!(!nominated.contains(&token), "declared nominated {token}");
            }
        }
        if let Some(te) = h.get("te") {
            prop_assert_eq!(te, "trailers");
        }
        for name in &nominated {
            let allowed_reemitted = websocket && (name == "upgrade" || name == "connection");
            let allowed_te = name == "te" && h.get("te").is_some_and(|v| v == "trailers");
            if !allowed_reemitted && !allowed_te {
                prop_assert!(h.get(name.as_str()).is_none(), "nominated {name} survived");
            }
        }
        if websocket {
            prop_assert_eq!(&h["upgrade"], "websocket");
            prop_assert_eq!(&h["connection"], "upgrade");
        } else {
            prop_assert!(h.get("upgrade").is_none());
            prop_assert!(h.get("connection").is_none());
        }
    }
}

#[test]
fn hop_by_hop_names_match_the_public_list() {
    let hop_by_hop = HOP_BY_HOP;
    let names: Vec<&str> = hop_by_hop.iter().map(HeaderName::as_str).collect();
    assert_eq!(names, HOP_BY_HOP_HEADERS);
    // Requests drop every hop-by-hop header except `Trailer`, then control.
    let hop: Vec<HeaderName> = hop_by_hop
        .into_iter()
        .filter(|n| n != header::TRAILER)
        .collect();
    let dropped = REQUEST_DROPPED;
    assert_eq!(dropped[..hop.len()], hop[..]);
    let control: Vec<&str> = dropped[hop.len()..]
        .iter()
        .map(HeaderName::as_str)
        .collect();
    assert_eq!(control, ["content-length", "expect", "http2-settings"]);
}
