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
        ("trailer", "x-t"),
        ("x-keep", "yes"),
    ]);
    sanitize_request_headers(&mut h, RequestKind::Regular);
    assert_eq!(h.len(), 1, "only x-keep survives: {h:?}");
    assert_eq!(h["x-keep"], "yes");
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
        ("transfer-encoding", "chunked"),
        ("keep-alive", "timeout=5"),
        ("content-length", "12"),
        ("content-type", "application/json"),
    ]);
    sanitize_response_headers(&mut h, StatusCode::OK);
    assert!(h.get("connection").is_none());
    assert!(h.get("x-internal").is_none());
    assert!(h.get("transfer-encoding").is_none());
    assert!(h.get("keep-alive").is_none());
    assert_eq!(h["content-length"], "12");
    assert_eq!(h["content-type"], "application/json");
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
        map(&[("upgrade", "h2c"), ("connection", "Upgrade, HTTP2-Settings")]),
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
    assert!(validate_websocket_switch(&map(&[
        ("upgrade", "websocket"),
        ("connection", "Upgrade")
    ]))
    .is_ok());
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
        "[a-z][a-z0-9-]{0,15}",
    ]
}

fn header_value() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("chunked".to_owned()),
        Just("identity, chunked".to_owned()),
        Just("close, content-length".to_owned()),
        Just("100-continue".to_owned()),
        "[ -~&&[^\r\n]]{0,24}",
    ]
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
                     "keep-alive", "proxy-connection", "trailer"] {
            prop_assert!(h.get(name).is_none(), "{name} survived");
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
