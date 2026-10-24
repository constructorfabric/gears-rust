//! Message hygiene at the proxy boundary.
//!
//! The rules here are the engine's defence against request smuggling and
//! desynchronisation, independent of what the caller already validated:
//!
//! - **Framing is never forwarded.** `Content-Length` and `Transfer-Encoding`
//!   from the inbound request are dropped; hyper derives the outbound
//!   framing from the body itself, and request bodies are never
//!   close-delimited (CVE-2026-2835 class: `identity, chunked`, HTTP/1.0 +
//!   TE, CL/TE disagreement).
//! - **Hop-by-hop headers, including every `Connection`-nominated one, are
//!   dropped** (RFC 9110 §7.6.1).
//! - **`Expect` is dropped**: the engine sends complete bodies, and a
//!   forwarded `Expect` is the lever for 0.CL / early-response desyncs.
//! - **Protocol switches are allowlisted.** Only `GET` + `Upgrade: websocket`
//!   without a body is tunnelled, and only after the upstream answered `101`
//!   (CVE-2026-2833 class: premature upgrade). `h2c` upgrades and
//!   `HTTP2-Settings` never reach the upstream.
//! - `TE` survives only as `TE: trailers` (needed by gRPC over HTTP/2).

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Method, StatusCode};

use crate::error::{ErrorKind, ProxyError};

/// Hop-by-hop headers (RFC 9110 §7.6.1, plus legacy `proxy-connection`).
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Request headers dropped in addition to the hop-by-hop set.
const REQUEST_CONTROL_HEADERS: &[&str] = &["content-length", "expect", "http2-settings"];

/// How a request is going to be forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// Ordinary request/response exchange.
    Regular,
    /// WebSocket opening handshake (RFC 6455 §4.1).
    WebSocketUpgrade,
}

/// Sanitize outbound request headers in place.
///
/// For [`RequestKind::WebSocketUpgrade`] the `Connection: Upgrade` /
/// `Upgrade: websocket` pair is re-emitted canonically after stripping.
pub fn sanitize_request_headers(headers: &mut HeaderMap, kind: RequestKind) {
    let te_trailers = headers
        .get_all(header::TE)
        .iter()
        .any(|v| v.as_bytes().eq_ignore_ascii_case(b"trailers"));

    strip_connection_nominated(headers, &[]);
    for name in HOP_BY_HOP_HEADERS.iter().chain(REQUEST_CONTROL_HEADERS) {
        headers.remove(*name);
    }

    if te_trailers {
        headers.insert(header::TE, HeaderValue::from_static("trailers"));
    }
    if kind == RequestKind::WebSocketUpgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
}

/// Sanitize upstream response headers in place.
///
/// A `101` keeps `Connection` and `Upgrade` (but nothing else they
/// nominate); every other status loses all hop-by-hop headers.
/// `Content-Length` is kept when it is the framing the body was decoded
/// with, and dropped when `Transfer-Encoding` was present too: the body was
/// then decoded as chunked and the length is a lie (RFC 9112 §6.3).
pub fn sanitize_response_headers(headers: &mut HeaderMap, status: StatusCode) {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        headers.remove(header::CONTENT_LENGTH);
    }
    if status == StatusCode::SWITCHING_PROTOCOLS {
        strip_connection_nominated(headers, &["upgrade"]);
        for name in HOP_BY_HOP_HEADERS {
            if *name != "connection" && *name != "upgrade" {
                headers.remove(*name);
            }
        }
    } else {
        strip_connection_nominated(headers, &[]);
        for name in HOP_BY_HOP_HEADERS {
            headers.remove(*name);
        }
    }
}

/// Whether the request asks for a WebSocket upgrade: `Upgrade` lists
/// `websocket` and `Connection` lists `upgrade` (case-insensitive, across
/// repeated header lines).
#[must_use]
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    has_token(headers, &header::UPGRADE, "websocket")
        && has_token(headers, &header::CONNECTION, "upgrade")
}

/// Validate a WebSocket opening handshake before it is forwarded.
///
/// # Errors
///
/// [`ErrorKind::InvalidRequest`] unless the method is `GET`, `Upgrade` is
/// exactly the single token `websocket`, and the request has no body.
pub fn validate_websocket_upgrade(
    method: &Method,
    headers: &HeaderMap,
    body_is_empty: bool,
) -> Result<(), ProxyError> {
    if method != Method::GET {
        return Err(invalid("WebSocket upgrade requires GET"));
    }
    let mut upgrades = headers.get_all(header::UPGRADE).iter();
    let only_websocket = match (upgrades.next(), upgrades.next()) {
        (Some(v), None) => v.as_bytes().trim_ascii().eq_ignore_ascii_case(b"websocket"),
        _ => false,
    };
    if !only_websocket {
        return Err(invalid("only `Upgrade: websocket` is supported"));
    }
    if !has_token(headers, &header::CONNECTION, "upgrade") {
        return Err(invalid("WebSocket upgrade requires `Connection: upgrade`"));
    }
    if !body_is_empty {
        return Err(invalid("WebSocket upgrade request must not carry a body"));
    }
    Ok(())
}

/// Validate the upstream's `101` for a WebSocket upgrade.
///
/// # Errors
///
/// [`ErrorKind::Protocol`] unless the response switches to `websocket`.
pub fn validate_websocket_switch(headers: &HeaderMap) -> Result<(), ProxyError> {
    if is_websocket_upgrade(headers) {
        Ok(())
    } else {
        Err(ProxyError::new(
            ErrorKind::Protocol,
            "upstream 101 response does not switch to websocket",
        ))
    }
}

fn invalid(msg: &'static str) -> ProxyError {
    ProxyError::new(ErrorKind::InvalidRequest, msg)
}

fn has_token(headers: &HeaderMap, name: &HeaderName, token: &str) -> bool {
    headers.get_all(name).iter().any(|v| {
        v.to_str()
            .is_ok_and(|s| s.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    })
}

/// Remove every header named by a `Connection` token, except `keep`.
fn strip_connection_nominated(headers: &mut HeaderMap, keep: &[&str]) {
    let nominated: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty() && !keep.iter().any(|k| t.eq_ignore_ascii_case(k)))
        .filter_map(|t| HeaderName::from_bytes(t.as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
}

#[cfg(test)]
#[path = "sanitize_tests.rs"]
mod sanitize_tests;
