use std::collections::HashMap;

use crate::domain::model::{PassthroughMode, RequestHeaderRules};
use http::{HeaderMap, HeaderName, HeaderValue};
use oagw_sdk::api::ErrorSource;

const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

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

/// Apply set/add/remove header rules from upstream config.
pub fn apply_header_rules(headers: &mut HeaderMap, rules: &RequestHeaderRules) {
    // Remove first.
    for name in &rules.remove {
        if let Ok(n) = HeaderName::from_bytes(name.to_lowercase().as_bytes()) {
            headers.remove(n);
        }
    }
    // Set (overwrite).
    for (name, value) in &rules.set {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.to_lowercase().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(n, v);
        }
    }
    // Add (append).
    for (name, value) in &rules.add {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.to_lowercase().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.append(n, v);
        }
    }
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
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("x-custom", "keep-me".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("connection").is_none());
        assert!(headers.get("transfer-encoding").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep-me");
    }

    #[test]
    fn hop_by_hop_strips_connection_listed_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive, X-Custom-Hop".parse().unwrap());
        headers.insert("x-custom-hop", "secret".parse().unwrap());
        headers.insert("x-safe", "keep-me".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("connection").is_none());
        assert!(headers.get("x-custom-hop").is_none());
        assert_eq!(headers.get("x-safe").unwrap(), "keep-me");
    }

    #[test]
    fn hop_by_hop_connection_whitespace_handling() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive , X-Foo , X-Bar".parse().unwrap());
        headers.insert("x-foo", "val1".parse().unwrap());
        headers.insert("x-bar", "val2".parse().unwrap());
        headers.insert("x-safe", "keep".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("x-foo").is_none());
        assert!(headers.get("x-bar").is_none());
        assert_eq!(headers.get("x-safe").unwrap(), "keep");
    }

    #[test]
    fn hop_by_hop_no_connection_header() {
        let mut headers = HeaderMap::new();
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("x-custom", "keep-me".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("transfer-encoding").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep-me");
    }

    #[test]
    fn hop_by_hop_connection_empty_and_invalid_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", ",,,".parse().unwrap());
        headers.insert("x-custom", "keep-me".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        assert!(headers.get("connection").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep-me");
    }

    #[test]
    fn host_replaced() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, "original.com".parse().unwrap());

        set_host_header(&mut headers, "api.openai.com", 443);

        assert_eq!(headers.get(http::header::HOST).unwrap(), "api.openai.com");
    }

    #[test]
    fn host_nonstandard_port() {
        let mut headers = HeaderMap::new();
        set_host_header(&mut headers, "api.openai.com", 8443);

        assert_eq!(
            headers.get(http::header::HOST).unwrap(),
            "api.openai.com:8443"
        );
    }

    #[test]
    fn internal_headers_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", "evil.com".parse().unwrap());
        headers.insert("x-oagw-trace-id", "abc".parse().unwrap());
        headers.insert("x-custom", "keep".parse().unwrap());

        strip_internal_headers(&mut headers);

        assert!(headers.get("x-oagw-target-host").is_none());
        assert!(headers.get("x-oagw-trace-id").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep");
    }

    /// Client-supplied internal headers (including x-oagw-internal-resolved-addr)
    /// must be stripped before service.rs injects its own values.
    /// This prevents a malicious client from influencing upstream routing.
    #[test]
    fn strip_removes_spoofed_internal_context_headers() {
        let mut headers = HeaderMap::new();
        // Simulate a malicious client injecting all internal context headers.
        headers.insert("x-oagw-internal-endpoint-host", "evil.com".parse().unwrap());
        headers.insert("x-oagw-internal-endpoint-port", "9999".parse().unwrap());
        headers.insert("x-oagw-internal-endpoint-scheme", "http".parse().unwrap());
        headers.insert(
            "x-oagw-internal-resolved-addr",
            "1.2.3.4:443".parse().unwrap(),
        );
        headers.insert("x-oagw-internal-instance-uri", "/pwned".parse().unwrap());
        headers.insert(
            "x-oagw-internal-upstream-id",
            "00000000-0000-0000-0000-000000000000".parse().unwrap(),
        );
        // Legitimate header that should survive.
        headers.insert("authorization", "Bearer token".parse().unwrap());

        strip_internal_headers(&mut headers);

        assert!(headers.get("x-oagw-internal-endpoint-host").is_none());
        assert!(headers.get("x-oagw-internal-endpoint-port").is_none());
        assert!(headers.get("x-oagw-internal-endpoint-scheme").is_none());
        assert!(headers.get("x-oagw-internal-resolved-addr").is_none());
        assert!(headers.get("x-oagw-internal-instance-uri").is_none());
        assert!(headers.get("x-oagw-internal-upstream-id").is_none());
        assert_eq!(headers.get("authorization").unwrap(), "Bearer token");
    }

    #[test]
    fn set_overwrites_existing() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-version", "v1".parse().unwrap());

        let rules = RequestHeaderRules {
            set: {
                let mut m = HashMap::new();
                m.insert("x-api-version".into(), "v2".into());
                m
            },
            add: HashMap::new(),
            remove: vec![],
            passthrough: PassthroughMode::None,
            passthrough_allowlist: vec![],
        };

        apply_header_rules(&mut headers, &rules);
        assert_eq!(headers.get("x-api-version").unwrap(), "v2");
    }

    #[test]
    fn add_appends() {
        let mut headers = HeaderMap::new();
        headers.insert("x-tag", "a".parse().unwrap());

        let rules = RequestHeaderRules {
            set: HashMap::new(),
            add: {
                let mut m = HashMap::new();
                m.insert("x-tag".into(), "b".into());
                m
            },
            remove: vec![],
            passthrough: PassthroughMode::None,
            passthrough_allowlist: vec![],
        };

        apply_header_rules(&mut headers, &rules);
        let values: Vec<&str> = headers
            .get_all("x-tag")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert!(values.contains(&"a"));
        assert!(values.contains(&"b"));
    }

    #[test]
    fn remove_deletes() {
        let mut headers = HeaderMap::new();
        headers.insert("x-remove-me", "gone".parse().unwrap());
        headers.insert("x-keep-me", "stay".parse().unwrap());

        let rules = RequestHeaderRules {
            set: HashMap::new(),
            add: HashMap::new(),
            remove: vec!["x-remove-me".into()],
            passthrough: PassthroughMode::None,
            passthrough_allowlist: vec![],
        };

        apply_header_rules(&mut headers, &rules);
        assert!(headers.get("x-remove-me").is_none());
        assert_eq!(headers.get("x-keep-me").unwrap(), "stay");
    }

    #[test]
    fn passthrough_none_starts_empty_but_keeps_content_type() {
        let mut inbound = HeaderMap::new();
        inbound.insert("x-custom", "val".parse().unwrap());
        inbound.insert(
            http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );

        let out = apply_passthrough(&inbound, &PassthroughMode::None, &[]);

        assert!(out.get("x-custom").is_none());
        assert_eq!(
            out.get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn passthrough_all_copies_everything() {
        let mut inbound = HeaderMap::new();
        inbound.insert("x-custom", "val".parse().unwrap());
        inbound.insert("x-other", "val2".parse().unwrap());

        let out = apply_passthrough(&inbound, &PassthroughMode::All, &[]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn passthrough_allowlist_filters() {
        let mut inbound = HeaderMap::new();
        inbound.insert("x-allowed", "yes".parse().unwrap());
        inbound.insert("x-blocked", "no".parse().unwrap());

        let out = apply_passthrough(&inbound, &PassthroughMode::Allowlist, &["x-allowed".into()]);

        assert_eq!(out.get("x-allowed").unwrap(), "yes");
        assert!(out.get("x-blocked").is_none());
    }

    #[test]
    fn passthrough_all_strips_authorization() {
        let mut inbound = HeaderMap::new();
        inbound.insert(
            http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        inbound.insert("cookie", "session=abc".parse().unwrap());
        inbound.insert("x-custom", "keep".parse().unwrap());

        let out = apply_passthrough(&inbound, &PassthroughMode::All, &[]);

        assert!(out.get(http::header::AUTHORIZATION).is_none());
        assert!(out.get("cookie").is_none());
        assert_eq!(out.get("x-custom").unwrap(), "keep");
    }

    #[test]
    fn extract_error_source_gateway() {
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-error-source", "gateway".parse().unwrap());
        assert_eq!(extract_error_source(&headers), ErrorSource::Gateway);
    }

    #[test]
    fn extract_error_source_absent_defaults_to_upstream() {
        let headers = HeaderMap::new();
        assert_eq!(extract_error_source(&headers), ErrorSource::Upstream);
    }

    #[test]
    fn extract_error_source_unrecognised_defaults_to_upstream() {
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-error-source", "unknown".parse().unwrap());
        assert_eq!(extract_error_source(&headers), ErrorSource::Upstream);
    }

    #[test]
    fn extract_error_source_upstream_explicit() {
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-error-source", "upstream".parse().unwrap());
        assert_eq!(extract_error_source(&headers), ErrorSource::Upstream);
    }

    #[test]
    fn sanitize_response_strips_transfer_encoding() {
        let mut headers = HeaderMap::new();
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        sanitize_response_headers(&mut headers);

        assert!(headers.get("transfer-encoding").is_none());
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn sanitize_response_strips_x_oagw_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-debug", "true".parse().unwrap());
        headers.insert("x-oagw-trace-id", "abc123".parse().unwrap());
        headers.insert("x-custom", "keep".parse().unwrap());

        sanitize_response_headers(&mut headers);

        assert!(headers.get("x-oagw-debug").is_none());
        assert!(headers.get("x-oagw-trace-id").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep");
    }
}
