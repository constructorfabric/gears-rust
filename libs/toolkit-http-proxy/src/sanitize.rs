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
//!   dropped** (RFC 9110 §7.6.1), except `Trailer`, which is end-to-end
//!   (§6.6.2) and is kept filtered, as below.
//! - **`Expect` is dropped**: the engine sends complete bodies, and a
//!   forwarded `Expect` is the lever for 0.CL / early-response desyncs.
//! - **Protocol switches are allowlisted.** Only `GET` + `Upgrade: websocket`
//!   without a body is tunnelled, and only after the upstream answered `101`
//!   (CVE-2026-2833 class: premature upgrade). `h2c` upgrades and
//!   `HTTP2-Settings` never reach the upstream.
//! - `TE` survives only as `TE: trailers` (needed by gRPC over HTTP/2).
//! - **Trailers keep every field except framing, connection-scoped fields and
//!   `Host`** (RFC 9110 §6.5.1). Request trailers also lose credential,
//!   routing and representation fields and request modifiers
//!   ([`sanitize_request_trailers`]). Requests and responses keep their
//!   `Trailer` declaration, minus the names their trailers lose: an HTTP/1.1
//!   recipient learns from it what follows, and hyper sends only the request
//!   trailers it declares.

use std::sync::Arc;

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Method, StatusCode};

use crate::error::{ErrorKind, ProxyError};

/// Hop-by-hop headers: the RFC 2616 §13.5.1 / RFC 7230 §6.1 set, plus legacy
/// `proxy-connection`. Not the list the engine drops: requests and responses
/// keep `Trailer`, filtered (RFC 9110 §6.6.2; see the module docs).
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

/// [`HOP_BY_HOP_HEADERS`] as header names, so removing them does not parse
/// a string per header per message.
const HOP_BY_HOP: [HeaderName; 9] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    HeaderName::from_static("proxy-connection"),
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

/// Request headers dropped: the hop-by-hop set plus framing and control,
/// except `Trailer`, which is end-to-end (RFC 9110 §6.6.2) and is filtered
/// instead. It does not frame the message: hyper derives the framing from
/// the body and uses the declaration only to choose which trailers to send.
const REQUEST_DROPPED: [HeaderName; 11] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    HeaderName::from_static("proxy-connection"),
    header::TE,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
    header::CONTENT_LENGTH,
    header::EXPECT,
    HeaderName::from_static("http2-settings"),
];

/// Response headers dropped: the hop-by-hop set except `Trailer`, which is
/// end-to-end (RFC 9110 §6.6.2) and is filtered instead.
const RESPONSE_DROPPED: [HeaderName; 8] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    HeaderName::from_static("proxy-connection"),
    header::TE,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

/// Fields dropped from trailers: framing, connection- and proxy-hop-scoped
/// fields and `Host`. They mean nothing there or belong to one hop (RFC 9110
/// §6.5.1, §11.7), and an HTTP/2 hop refuses to send connection-specific
/// fields in a trailer block.
const TRAILER_DROPPED: [HeaderName; 12] = [
    header::CONTENT_LENGTH,
    header::TRANSFER_ENCODING,
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    HeaderName::from_static("proxy-connection"),
    header::TE,
    header::TRAILER,
    header::UPGRADE,
    header::HOST,
    header::PROXY_AUTHORIZATION,
    header::PROXY_AUTHENTICATE,
    HeaderName::from_static("proxy-authentication-info"),
];

/// Fields a request's trailers (and its `Trailer` declaration) lose on top of
/// [`TRAILER_DROPPED`]: credentials, routing, representation metadata and
/// request modifiers (controls and conditionals, RFC 9110 §6.5.1), plus any
/// `X-Forwarded-*`, and the `Expect` and `HTTP2-Settings` fields the engine
/// strips from request heads. An upstream that merges trailers into the
/// header section would otherwise take them as head fields the caller's own
/// proxy stripped or overrode (CVE-2013-5704 class), or turn the request
/// into a conditional or range request. hyper refuses only a subset of
/// these in the trailers it encodes, and h2 none.
const REQUEST_TRAILER_DROPPED: [HeaderName; 20] = [
    header::AUTHORIZATION,
    header::COOKIE,
    header::SET_COOKIE,
    header::CACHE_CONTROL,
    header::CONTENT_ENCODING,
    header::CONTENT_RANGE,
    header::CONTENT_TYPE,
    header::MAX_FORWARDS,
    header::FORWARDED,
    header::VIA,
    HeaderName::from_static("x-real-ip"),
    header::IF_MATCH,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::IF_UNMODIFIED_SINCE,
    header::IF_RANGE,
    header::RANGE,
    header::PRAGMA,
    header::EXPECT,
    HeaderName::from_static("http2-settings"),
];

/// Whether a request's trailers drop `name` (beyond [`TRAILER_DROPPED`]).
fn request_trailer_dropped(name: &HeaderName) -> bool {
    REQUEST_TRAILER_DROPPED.contains(name) || name.as_str().starts_with("x-forwarded-")
}

/// The fields a message's `Connection` header nominated. They are hop-by-hop
/// in the trailers too (RFC 9110 §7.6.1), so they travel with the request
/// (as this extension) or the response body to where the trailers are
/// sanitized.
#[derive(Debug, Clone)]
pub(crate) struct HopFields(
    /// The field names `Connection` nominated.
    pub(crate) Arc<[HeaderName]>,
);

/// Remove `names` from `headers`. Most messages carry none of them, and one
/// pass over the few headers present is cheaper than a lookup per name.
fn remove_all(headers: &mut HeaderMap, names: &[HeaderName]) {
    if headers.keys().any(|k| names.contains(k)) {
        for name in names {
            headers.remove(name);
        }
    }
}

/// How a request is going to be forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// Ordinary request/response exchange.
    Regular,
    /// WebSocket opening handshake (RFC 6455 §4.1).
    WebSocketUpgrade,
}

/// Whether [`sanitize_request_headers`] drops every `name` line from a
/// request head, whatever the request's `Connection` field nominates. Lets
/// a caller that changes the head after sanitizing it refuse a header the
/// engine would drop. `Trailer` is filtered rather than dropped, so it is
/// not included. `TE` is included although `TE: trailers` survives: a
/// caller using this predicate refuses every `TE` line.
#[must_use]
pub fn is_dropped_request_header(name: &HeaderName) -> bool {
    REQUEST_DROPPED.contains(name)
}

/// Sanitize outbound request headers in place.
///
/// `Trailer` is filtered like the trailers themselves
/// ([`sanitize_request_trailers`]), also losing the fields `Connection`
/// nominated.
/// For [`RequestKind::WebSocketUpgrade`] it is dropped (the handshake has
/// no body), and the `Connection: Upgrade` / `Upgrade: websocket` pair is
/// re-emitted canonically after stripping.
pub fn sanitize_request_headers(headers: &mut HeaderMap, kind: RequestKind) {
    let te_trailers = headers
        .get_all(header::TE)
        .iter()
        .any(|v| v.as_bytes().eq_ignore_ascii_case(b"trailers"));

    let nominated = connection_nominated(headers);
    for name in &nominated {
        headers.remove(name);
    }
    remove_all(headers, &REQUEST_DROPPED);
    if headers.contains_key(header::TRAILER) {
        if kind == RequestKind::Regular {
            filter_trailer_declaration(headers, &nominated, request_trailer_dropped);
        } else {
            headers.remove(header::TRAILER);
        }
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
/// nominate); every other status loses all hop-by-hop headers except
/// `Trailer`, which is filtered like the trailers themselves
/// ([`sanitize_trailers`]).
/// `Content-Length` is kept when it is the framing the body was decoded
/// with, and dropped when `Transfer-Encoding` was present too: the body was
/// then decoded as chunked and the length is a lie (RFC 9112 §6.3). A `204`
/// or `1xx` loses it too: such a response has no body, and a recipient
/// shown a length may wait for one (RFC 9110 §8.6). `HEAD` and `304`
/// responses keep it, as the length of the representation.
pub fn sanitize_response_headers(headers: &mut HeaderMap, status: StatusCode) {
    let _nominated = sanitize_response_head(headers, status);
}

/// [`sanitize_response_headers`], returning the fields the response's
/// `Connection` header nominated, which its trailers must lose as well.
pub(crate) fn sanitize_response_head(
    headers: &mut HeaderMap,
    status: StatusCode,
) -> Vec<HeaderName> {
    if headers.contains_key(header::TRANSFER_ENCODING)
        || status == StatusCode::NO_CONTENT
        || status.is_informational()
    {
        headers.remove(header::CONTENT_LENGTH);
    }
    if status == StatusCode::SWITCHING_PROTOCOLS {
        strip_connection_nominated(headers, &["upgrade"]);
        for name in &HOP_BY_HOP {
            if name != header::CONNECTION && name != header::UPGRADE {
                headers.remove(name);
            }
        }
        return Vec::new();
    }
    let nominated = connection_nominated(headers);
    for name in &nominated {
        headers.remove(name);
    }
    remove_all(headers, &RESPONSE_DROPPED);
    if headers.contains_key(header::TRAILER) {
        filter_trailer_declaration(headers, &nominated, |_| false);
    }
    nominated
}

/// Sanitize response trailers in place: drop framing, connection- and
/// proxy-hop-scoped fields (including any a `Connection` field in the
/// trailers nominates) and `Host`; keep everything else (`grpc-status`,
/// checksums, timing, custom fields). Request trailers lose more: use
/// [`sanitize_request_trailers`].
pub fn sanitize_trailers(trailers: &mut HeaderMap) {
    sanitize_trailers_without(trailers, &[]);
}

/// [`sanitize_trailers`], also dropping `hop`: the fields the message
/// head's `Connection` header nominated.
pub(crate) fn sanitize_trailers_without(trailers: &mut HeaderMap, hop: &[HeaderName]) {
    strip_connection_nominated(trailers, &[]);
    remove_all(trailers, &TRAILER_DROPPED);
    for name in hop {
        trailers.remove(name);
    }
}

/// Sanitize a request's trailers in place: what [`sanitize_trailers`] drops,
/// plus credential, routing and representation fields (`Authorization`,
/// `Cookie`, `Set-Cookie`, `Cache-Control`, `Content-Encoding`,
/// `Content-Range`, `Content-Type`, `Max-Forwards`, `Forwarded`, `Via`,
/// `X-Real-IP`, `X-Forwarded-*`), request modifiers (`If-Match`,
/// `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`, `If-Range`,
/// `Range`, `Pragma`) and `Expect` / `HTTP2-Settings`. An upstream that
/// merges trailers into the header section would otherwise take them as head
/// fields.
///
/// This is a denylist. Whether any other field may be sent in trailers is
/// the caller's decision: RFC 9110 §6.5.1 leaves that check to the sender.
pub fn sanitize_request_trailers(trailers: &mut HeaderMap) {
    sanitize_request_trailers_without(trailers, &[]);
}

/// [`sanitize_request_trailers`], also dropping `hop`: the fields the request
/// head's `Connection` header nominated.
pub(crate) fn sanitize_request_trailers_without(trailers: &mut HeaderMap, hop: &[HeaderName]) {
    sanitize_trailers_without(trailers, hop);
    let dropped: Vec<HeaderName> = trailers
        .keys()
        .filter(|name| request_trailer_dropped(name))
        .cloned()
        .collect();
    for name in dropped {
        trailers.remove(name);
    }
}

/// Rewrite `Trailer` without the names [`sanitize_trailers_without`] drops,
/// the names in `hop` and those `also_dropped` selects; remove it when
/// nothing is left.
fn filter_trailer_declaration(
    headers: &mut HeaderMap,
    hop: &[HeaderName],
    also_dropped: fn(&HeaderName) -> bool,
) {
    // `hop` comes from the caller's `Connection` field and is not bounded:
    // look names up in a set, so the cost stays linear in both lists.
    let hop: std::collections::HashSet<&HeaderName> = hop.iter().collect();
    let kept: Vec<&str> = headers
        .get_all(header::TRAILER)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|t| {
            !t.is_empty()
                && HeaderName::from_bytes(t.as_bytes()).is_ok_and(|n| {
                    !TRAILER_DROPPED.contains(&n) && !hop.contains(&n) && !also_dropped(&n)
                })
        })
        .collect();
    let value = (!kept.is_empty())
        .then(|| HeaderValue::from_str(&kept.join(", ")).ok())
        .flatten();
    match value {
        Some(value) => {
            headers.insert(header::TRAILER, value);
        }
        None => {
            headers.remove(header::TRAILER);
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
    for name in nominated(headers, keep) {
        headers.remove(name);
    }
}

/// The fields `Connection` nominates.
pub(crate) fn connection_nominated(headers: &HeaderMap) -> Vec<HeaderName> {
    nominated(headers, &[])
}

/// The fields `Connection` nominates, except `keep`.
fn nominated(headers: &HeaderMap, keep: &[&str]) -> Vec<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|t| !t.is_empty() && !keep.iter().any(|k| t.eq_ignore_ascii_case(k)))
        .filter_map(|t| HeaderName::from_bytes(t.as_bytes()).ok())
        .collect()
}

#[cfg(test)]
#[path = "sanitize_tests.rs"]
mod sanitize_tests;
