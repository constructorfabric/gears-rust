use std::collections::HashMap;

use crate::domain::model::{PassthroughMode, RequestHeaderRules, ResponseHeaderRules};
use http::{HeaderMap, HeaderName, HeaderValue};
use oagw_sdk::api::ErrorSource;

use super::HOP_BY_HOP_HEADERS;

/// Sensitive headers that must never be forwarded to upstream services,
/// even when `PassthroughMode::All` is used.
const STRIPPED_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
];

/// Apply passthrough filter: decide which inbound headers to forward.
/// Content-Type is always forwarded when present (needed for POST/PUT bodies).
pub fn apply_passthrough(
    inbound: &HeaderMap,
    mode: &PassthroughMode,
    allowlist: &[String],
) -> HeaderMap {
    let mut out = match mode {
        PassthroughMode::None => HeaderMap::new(),
        PassthroughMode::All => inbound.clone(),
        PassthroughMode::Allowlist => {
            let mut h = HeaderMap::new();
            for name in allowlist {
                if let Ok(n) = HeaderName::from_bytes(name.to_lowercase().as_bytes())
                    && let Some(v) = inbound.get(&n)
                {
                    h.insert(n, v.clone());
                }
            }
            h
        }
    };

    // Always forward Content-Type if present.
    if !out.contains_key(http::header::CONTENT_TYPE)
        && let Some(ct) = inbound.get(http::header::CONTENT_TYPE)
    {
        out.insert(http::header::CONTENT_TYPE, ct.clone());
    }

    // Strip sensitive headers that must never leak to upstream.
    for name in STRIPPED_HEADERS {
        out.remove(*name);
    }

    out
}

/// Remove hop-by-hop headers that must not be forwarded.
///
/// Per RFC 7230 Section 6.1, intermediaries MUST remove headers listed in the
/// `Connection` header value in addition to the static hop-by-hop list.
pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    // First, parse Connection header and remove any headers it names.
    if let Some(conn_value) = headers.get("connection").and_then(|v| v.to_str().ok()) {
        let named: Vec<String> = conn_value
            .split(',')
            .map(|token| token.trim().to_lowercase())
            .filter(|token| !token.is_empty())
            .collect();
        for name in &named {
            if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                headers.remove(header_name);
            }
        }
    }

    // Then remove the static hop-by-hop list.
    for name in HOP_BY_HOP_HEADERS {
        headers.remove(*name);
    }
}

/// Remove X-OAGW-* internal headers.
pub fn strip_internal_headers(headers: &mut HeaderMap) {
    let to_remove: Vec<HeaderName> = headers
        .keys()
        .filter(|k| k.as_str().starts_with("x-oagw-"))
        .cloned()
        .collect();
    for name in to_remove {
        headers.remove(&name);
    }
}

/// Extract `ErrorSource` from the `x-oagw-error-source` response header.
///
/// Must be called **before** [`sanitize_response_headers`] which strips all
/// `x-oagw-*` headers. Returns `ErrorSource::Upstream` when the header is
/// absent or has an unrecognised value (upstream responses never carry the
/// header, so absence ⇒ upstream).
pub fn extract_error_source(headers: &HeaderMap) -> ErrorSource {
    match headers
        .get("x-oagw-error-source")
        .and_then(|v| v.to_str().ok())
    {
        Some("gateway") => ErrorSource::Gateway,
        _ => ErrorSource::Upstream,
    }
}

/// Sanitize upstream response headers before forwarding to the client.
/// Strips hop-by-hop headers and `x-oagw-*` internal headers.
pub fn sanitize_response_headers(headers: &mut HeaderMap) {
    strip_hop_by_hop(headers);
    strip_internal_headers(headers);
}

/// Returns `true` if the request headers indicate a WebSocket upgrade.
///
/// Per RFC 6455 §4.1, requires both `Upgrade: websocket` and
/// `Connection: Upgrade` tokens (case-insensitive, handles multiple
/// header instances via `get_all()` per RFC 7230).
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let has_upgrade_websocket = headers
        .get_all(http::header::UPGRADE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("websocket"))
        });

    let has_connection_upgrade = headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });

    has_upgrade_websocket && has_connection_upgrade
}

/// Like [`strip_hop_by_hop`] but preserves `Upgrade` and `Connection` headers,
/// which are required for WebSocket upgrade negotiation (RFC 6455 §4.1).
pub fn strip_hop_by_hop_for_upgrade(headers: &mut HeaderMap) {
    // Parse Connection-nominated headers but skip "upgrade" itself.
    if let Some(conn_value) = headers.get("connection").and_then(|v| v.to_str().ok()) {
        let named: Vec<String> = conn_value
            .split(',')
            .map(|token| token.trim().to_lowercase())
            .filter(|token| !token.is_empty() && token != "upgrade")
            .collect();
        for name in &named {
            if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                headers.remove(header_name);
            }
        }
    }

    // Remove static hop-by-hop headers EXCEPT "connection" and "upgrade".
    for name in HOP_BY_HOP_HEADERS {
        if *name != "connection" && *name != "upgrade" {
            headers.remove(*name);
        }
    }
}

/// Like [`sanitize_response_headers`] but preserves `Upgrade` and `Connection`
/// headers needed for 101 Switching Protocols responses.
pub fn sanitize_response_headers_for_upgrade(headers: &mut HeaderMap) {
    strip_hop_by_hop_for_upgrade(headers);
    strip_internal_headers(headers);
}

trait HeaderRules {
    fn remove(&self) -> &[String];
    fn set(&self) -> &HashMap<String, String>;
    fn add(&self) -> &HashMap<String, String>;
}

impl HeaderRules for RequestHeaderRules {
    fn remove(&self) -> &[String] {
        &self.remove
    }
    fn set(&self) -> &HashMap<String, String> {
        &self.set
    }
    fn add(&self) -> &HashMap<String, String> {
        &self.add
    }
}

impl HeaderRules for ResponseHeaderRules {
    fn remove(&self) -> &[String] {
        &self.remove
    }
    fn set(&self) -> &HashMap<String, String> {
        &self.set
    }
    fn add(&self) -> &HashMap<String, String> {
        &self.add
    }
}

fn apply_rules(headers: &mut HeaderMap, rules: &impl HeaderRules) {
    // Remove first.
    for name in rules.remove() {
        if let Ok(n) = HeaderName::from_bytes(name.to_lowercase().as_bytes()) {
            headers.remove(n);
        }
    }
    // Set (overwrite).
    for (name, value) in rules.set() {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.to_lowercase().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(n, v);
        }
    }
    // Add (append).
    for (name, value) in rules.add() {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.to_lowercase().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.append(n, v);
        }
    }
}

/// Apply set/add/remove header rules from upstream config to outbound request headers.
pub fn apply_request_header_rules(headers: &mut HeaderMap, rules: &RequestHeaderRules) {
    apply_rules(headers, rules);
}

/// Apply set/add/remove header rules to upstream response headers.
pub fn apply_response_header_rules(headers: &mut HeaderMap, rules: &ResponseHeaderRules) {
    apply_rules(headers, rules);
}

/// Returns `true` if the Content-Type header (when present) is a valid MIME type.
/// Returns `false` if duplicates exist, the value is not valid UTF-8, or it
/// cannot be parsed as a MIME type. Returns `true` if the header is absent.
pub fn is_valid_content_type(headers: &HeaderMap) -> bool {
    let mut iter = headers.get_all(http::header::CONTENT_TYPE).iter();
    let Some(ct) = iter.next() else {
        return true;
    };
    // Reject duplicate Content-Type headers.
    if iter.next().is_some() {
        return false;
    }
    ct.to_str()
        .ok()
        .and_then(|v| v.parse::<mime::Mime>().ok())
        .is_some()
}

/// Returns `true` if the Transfer-Encoding header is absent or exactly `chunked`.
/// Returns `false` for duplicates or any encoding other than `chunked`.
pub fn is_valid_transfer_encoding(headers: &HeaderMap) -> bool {
    let mut iter = headers.get_all(http::header::TRANSFER_ENCODING).iter();
    let Some(val) = iter.next() else {
        return true;
    };
    // Reject duplicate Transfer-Encoding headers (HTTP smuggling vector).
    if iter.next().is_some() {
        return false;
    }
    val.to_str()
        .ok()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("chunked"))
}

/// Set the Host header to match the upstream endpoint.
pub fn set_host_header(headers: &mut HeaderMap, host: &str, port: u16) {
    let host_value = if port == 443 || port == 80 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    if let Ok(v) = HeaderValue::from_str(&host_value) {
        headers.insert(http::header::HOST, v);
    }
}

/// Convert an HTTP `HeaderMap` to a `HashMap<String, String>` for plugin contexts.
///
/// Non-UTF-8 header values are silently dropped (they cannot be represented as
/// `String` and are rare in practice).
pub fn header_map_to_hash_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect()
}

/// Convert an HTTP `HeaderMap` to a `Vec<(String, String)>` preserving multi-valued headers.
///
/// Non-UTF-8 header values are silently dropped (they cannot be represented as
/// `String` and are rare in practice).
pub fn header_map_to_vec(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect()
}

/// Convert a `Vec<(String, String)>` back to an HTTP `HeaderMap`, preserving multi-values.
///
/// Entries with invalid header names or values are logged at `debug` level and
/// dropped — this can happen when a plugin injects malformed headers.
pub fn vec_to_header_map(headers: &[(String, String)]) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in headers {
        match (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            (Ok(name), Ok(val)) => {
                out.append(name, val);
            }
            _ => {
                tracing::debug!(
                    header_name = %k,
                    "plugin-mutated header dropped: invalid name or value"
                );
            }
        }
    }
    out
}

/// Convert a `HashMap<String, String>` back to an HTTP `HeaderMap`.
///
/// Entries with invalid header names or values are logged at `debug` level and
/// dropped — this can happen when a plugin injects malformed headers.
pub fn hash_map_to_header_map(headers: &HashMap<String, String>) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in headers {
        match (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            (Ok(name), Ok(val)) => {
                out.insert(name, val);
            }
            _ => {
                tracing::debug!(
                    header_name = %k,
                    "plugin-mutated header dropped: invalid name or value"
                );
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "headers_tests.rs"]
mod headers_tests;
