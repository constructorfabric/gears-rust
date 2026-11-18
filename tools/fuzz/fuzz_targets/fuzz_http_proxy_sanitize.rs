#![no_main]

//! The upstream engine's request sanitizer must never let framing, control
//! or hop-by-hop headers (or anything a `Connection` header nominates)
//! through, whatever the inbound header set looks like. `Trailer` may stay
//! on a regular request, declaring none of those names.

use http::header::{HeaderMap, HeaderName, HeaderValue};
use libfuzzer_sys::fuzz_target;
use toolkit_http_proxy::sanitize::{RequestKind, sanitize_request_headers};

const ALWAYS_DROPPED: &[&str] = &[
    "content-length",
    "transfer-encoding",
    "expect",
    "http2-settings",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "proxy-authenticate",
];

/// Names a request's `Trailer` declaration must never keep.
const NEVER_DECLARED: &[&str] = &[
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
    // Request trailers only: credentials, routing, representation, request
    // modifiers, and fields the engine strips from request heads.
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

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let Some((&flags, rest)) = data.split_first() else {
        return;
    };
    let kind = if flags & 1 == 0 {
        RequestKind::Regular
    } else {
        RequestKind::WebSocketUpgrade
    };

    // One `name: value` header per line; lines that are not a valid header
    // are skipped, as an HTTP parser would have rejected them upstream.
    let mut headers = HeaderMap::new();
    for line in rest.split(|b| *b == b'\n') {
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            continue;
        };
        let (name, value) = (&line[..colon], line[colon + 1..].trim_ascii());
        if let (Ok(name), Ok(value)) =
            (HeaderName::from_bytes(name), HeaderValue::from_bytes(value))
        {
            headers.append(name, value);
        }
    }

    let nominated: Vec<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();

    sanitize_request_headers(&mut headers, kind);

    for name in ALWAYS_DROPPED {
        assert!(headers.get(*name).is_none(), "{name} survived");
    }
    if let Some(te) = headers.get("te") {
        assert_eq!(te, "trailers");
    }
    let websocket = kind == RequestKind::WebSocketUpgrade;
    assert!(headers.get_all("trailer").iter().count() <= 1);
    if let Some(declared) = headers.get("trailer") {
        assert!(!websocket, "an upgrade kept its trailer declaration");
        for token in declared.to_str().unwrap().split(',') {
            let token = token.trim().to_ascii_lowercase();
            assert!(
                !NEVER_DECLARED.contains(&token.as_str()) && !token.starts_with("x-forwarded-"),
                "declared {token}"
            );
            assert!(!nominated.contains(&token), "declared nominated {token}");
        }
    }
    for name in &nominated {
        let reemitted = websocket && (name == "upgrade" || name == "connection");
        let trailers = name == "te" && headers.get("te").is_some_and(|v| v == "trailers");
        if !reemitted && !trailers {
            assert!(
                headers.get(name.as_str()).is_none(),
                "nominated {name} survived"
            );
        }
    }
    if websocket {
        assert_eq!(headers.get_all("upgrade").iter().count(), 1);
        assert_eq!(headers["upgrade"], "websocket");
        assert_eq!(headers["connection"], "upgrade");
    } else {
        assert!(headers.get("upgrade").is_none());
        assert!(headers.get("connection").is_none());
    }
});
