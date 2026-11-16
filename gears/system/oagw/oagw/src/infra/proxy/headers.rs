use std::collections::HashMap;

use crate::domain::error::DomainError;
use crate::domain::model::{PassthroughMode, RequestHeaderRules, ResponseHeaderRules};
use http::{HeaderMap, HeaderName, HeaderValue};
use oagw_sdk::api::ErrorSource;
use oagw_sdk::field;

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
        // `all` forwards what the client sent, but not its forwarding and
        // override headers: only an allowlist that names one forwards it.
        PassthroughMode::All => {
            let mut all = inbound.clone();
            strip_forwarding_headers(&mut all);
            all
        }
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
    // First, remove the headers every Connection field line names.
    for name in connection_nominated(headers) {
        headers.remove(name);
    }

    // Then remove the static hop-by-hop list.
    for name in HOP_BY_HOP_HEADERS {
        headers.remove(*name);
    }
}

/// The header names nominated by every `Connection` field line (RFC 9110
/// §7.6.1). A request may repeat the field; reading only the first line
/// would forward the names the others nominate.
fn connection_nominated(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .filter_map(|token| HeaderName::from_bytes(token.as_bytes()).ok())
        .collect()
}

/// Headers a client could use to spoof its address or origin, or to override
/// the path, host or method, at the upstream (F0008 SSRF rules): `Forwarded`
/// (RFC 7239), the `X-Forwarded*` family, every `Client-IP` variant, the
/// proxy and CDN client-address headers, and the URL, host and method
/// override headers some frameworks honour after OAGW matched the route.
fn is_forwarding_header(name: &HeaderName) -> bool {
    let name = name.as_str();
    name.starts_with("x-forwarded")
        || name.starts_with("x-envoy-")
        || name.ends_with("client-ip")
        || matches!(
            name,
            "forwarded"
                | "via"
                | "x-proxyuser-ip"
                | "x-real-ip"
                | "x-originating-ip"
                | "x-remote-ip"
                | "x-remote-addr"
                | "cf-connecting-ip"
                | "x-original-forwarded-for"
                | "x-original-url"
                | "x-original-uri"
                | "x-rewrite-url"
                | "x-original-host"
                | "x-host"
                | "x-http-method-override"
                | "x-http-method"
                | "x-method-override"
        )
}

/// Remove forwarding headers ([`is_forwarding_header`]) from an outbound
/// request. Requests only: upstream responses keep them.
fn strip_forwarding_headers(headers: &mut HeaderMap) {
    let to_remove: Vec<HeaderName> = headers
        .keys()
        .filter(|name| is_forwarding_header(name))
        .cloned()
        .collect();
    for name in to_remove {
        headers.remove(&name);
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
    // Remove Connection-nominated headers but keep "upgrade" itself.
    for name in connection_nominated(headers) {
        if name != http::header::UPGRADE {
            headers.remove(name);
        }
    }

    // Remove static hop-by-hop headers EXCEPT "connection" and "upgrade".
    for name in HOP_BY_HOP_HEADERS {
        if *name != "connection" && *name != "upgrade" {
            headers.remove(*name);
        }
    }
}

/// Remove from `outbound` every header the client's `Connection` lines in
/// `inbound` nominate. Passthrough copies `Connection` only when the mode
/// forwards it, so stripping `outbound` alone would forward an allowlisted
/// header the client marked hop-by-hop. An upgrade keeps `Upgrade`.
pub fn strip_inbound_nominated(outbound: &mut HeaderMap, inbound: &HeaderMap, is_upgrade: bool) {
    for name in connection_nominated(inbound) {
        if !(is_upgrade && name == http::header::UPGRADE) {
            outbound.remove(name);
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

/// Validates that the `Content-Type` header (when present) is a valid MIME type.
/// Returns `Ok(())` if absent or valid; returns a `DomainError::Validation` if
/// duplicates exist, the value is not valid UTF-8, or it cannot be parsed.
pub fn validate_content_type(headers: &HeaderMap) -> Result<(), DomainError> {
    let mut iter = headers.get_all(http::header::CONTENT_TYPE).iter();
    let Some(ct) = iter.next() else {
        return Ok(());
    };
    // Reject duplicate Content-Type headers.
    if iter.next().is_some() {
        return Err(DomainError::validation_for(
            "content-type",
            field::DUPLICATE_HEADER,
            "duplicate Content-Type headers",
        ));
    }
    ct.to_str()
        .ok()
        .and_then(|v| v.parse::<mime::Mime>().ok())
        .map(|_| ())
        .ok_or_else(|| {
            DomainError::validation_for(
                "content-type",
                field::INVALID_MIME_TYPE,
                "Content-Type header is not a recognized MIME type",
            )
        })
}

/// Rejects duplicate or malformed `Content-Length` / `Transfer-Encoding`
/// headers and CL+TE co-existence.
pub fn validate_smuggling_headers(headers: &HeaderMap) -> Result<(), DomainError> {
    let mut cl_iter = headers.get_all(http::header::CONTENT_LENGTH).iter();
    let has_cl = if let Some(val) = cl_iter.next() {
        if cl_iter.next().is_some() {
            return Err(DomainError::validation_for(
                "content-length",
                field::DUPLICATE_HEADER,
                "duplicate Content-Length headers",
            ));
        }
        let Ok(s) = val.to_str() else {
            return Err(DomainError::validation_for(
                "content-length",
                field::INVALID_VALUE,
                "Content-Length is not a valid integer",
            ));
        };
        if s.trim().parse::<u64>().is_err() {
            return Err(DomainError::validation_for(
                "content-length",
                field::INVALID_VALUE,
                "Content-Length is not a valid integer",
            ));
        }
        true
    } else {
        false
    };

    let mut te_iter = headers.get_all(http::header::TRANSFER_ENCODING).iter();
    let has_te = if let Some(val) = te_iter.next() {
        if te_iter.next().is_some() {
            return Err(DomainError::validation_for(
                "transfer-encoding",
                field::DUPLICATE_HEADER,
                "duplicate Transfer-Encoding headers",
            ));
        }
        let ok = val
            .to_str()
            .ok()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("chunked"));
        if !ok {
            return Err(DomainError::validation_for(
                "transfer-encoding",
                "UNSUPPORTED_VALUE",
                "unsupported Transfer-Encoding; only chunked is accepted",
            ));
        }
        true
    } else {
        false
    };

    if has_cl && has_te {
        return Err(DomainError::validation_for(
            "transfer-encoding",
            "CL_TE_COEXISTENCE",
            "request contains both Content-Length and Transfer-Encoding",
        ));
    }

    Ok(())
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

/// Extract client IP from request headers for IP-scoped rate limiting.
///
/// Checks `X-Forwarded-For` (first IP in the chain) then `X-Real-IP`.
/// Returns `None` if neither header is present or parseable.
pub fn extract_client_ip(headers: &HeaderMap) -> Option<String> {
    // Parse and validate as a real IP address to prevent arbitrary token injection.
    // Only trusted when headers originate from a trusted proxy.
    if let Some(xff) = headers.get("x-forwarded-for")
        && let Ok(val) = xff.to_str()
        && let Some(candidate) = val
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        && let Ok(ip) = candidate.parse::<std::net::IpAddr>()
    {
        return Some(ip.to_string());
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .map(|ip| ip.to_string())
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

    /// A request may repeat `Connection`; the names every line nominates go
    /// (#4862 item 6).
    #[test]
    fn hop_by_hop_reads_every_connection_line() {
        let mut headers = HeaderMap::new();
        headers.append("connection", "x-first".parse().unwrap());
        headers.append("connection", "x-second, X-Third".parse().unwrap());
        headers.insert("x-first", "1".parse().unwrap());
        headers.insert("x-second", "2".parse().unwrap());
        headers.insert("x-third", "3".parse().unwrap());
        headers.insert("x-custom", "keep".parse().unwrap());

        strip_hop_by_hop(&mut headers);

        for name in ["connection", "x-first", "x-second", "x-third"] {
            assert!(headers.get(name).is_none(), "{name} was forwarded");
        }
        assert_eq!(headers.get("x-custom").unwrap(), "keep");
    }

    #[test]
    fn upgrade_strip_reads_every_connection_line() {
        let mut headers = HeaderMap::new();
        headers.append("connection", "Upgrade".parse().unwrap());
        headers.append("connection", "x-second".parse().unwrap());
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("x-second", "2".parse().unwrap());

        strip_hop_by_hop_for_upgrade(&mut headers);

        assert!(headers.get("x-second").is_none());
        assert_eq!(headers.get("upgrade").unwrap(), "websocket");
        assert_eq!(headers.get_all("connection").iter().count(), 2);
    }

    /// An allowlist that names a header but not `Connection` still drops
    /// the header when the client nominates it on any `Connection` line.
    #[test]
    fn allowlist_passthrough_drops_connection_nominated_headers() {
        let mut inbound = HeaderMap::new();
        inbound.append("connection", "keep-alive".parse().unwrap());
        inbound.append("connection", "x-hop".parse().unwrap());
        inbound.insert("x-hop", "1".parse().unwrap());
        inbound.insert("x-keep", "2".parse().unwrap());
        let allowlist = vec!["x-hop".to_string(), "x-keep".to_string()];

        let mut out = apply_passthrough(&inbound, &PassthroughMode::Allowlist, &allowlist);
        strip_inbound_nominated(&mut out, &inbound, false);
        strip_hop_by_hop(&mut out);

        assert!(out.get("x-hop").is_none(), "nominated header was forwarded");
        assert_eq!(out.get("x-keep").unwrap(), "2");
    }

    /// On an upgrade the nominated `Upgrade` header stays.
    #[test]
    fn upgrade_keeps_nominated_upgrade_header() {
        let mut inbound = HeaderMap::new();
        inbound.insert("connection", "Upgrade, x-hop".parse().unwrap());
        inbound.insert("upgrade", "websocket".parse().unwrap());
        inbound.insert("x-hop", "1".parse().unwrap());
        let allowlist = vec!["upgrade".to_string(), "x-hop".to_string()];

        let mut out = apply_passthrough(&inbound, &PassthroughMode::Allowlist, &allowlist);
        strip_inbound_nominated(&mut out, &inbound, true);
        strip_hop_by_hop_for_upgrade(&mut out);

        assert!(out.get("x-hop").is_none());
        assert_eq!(out.get("upgrade").unwrap(), "websocket");
    }

    #[test]
    fn forwarding_headers_stripped_unless_allowlisted() {
        let build = || {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", "10.0.0.1".parse().unwrap());
            headers.insert("x-forwarded-host", "evil.example".parse().unwrap());
            headers.insert("x-forwarded-proto", "http".parse().unwrap());
            headers.insert("x-real-ip", "10.0.0.2".parse().unwrap());
            headers.insert("forwarded", "for=10.0.0.3;host=internal".parse().unwrap());
            headers.insert("x-forwarded", "for=10.0.0.4".parse().unwrap());
            headers.insert("x-client-ip", "10.0.0.5".parse().unwrap());
            headers.insert("true-client-ip", "10.0.0.6".parse().unwrap());
            headers.insert("cf-connecting-ip", "10.0.0.7".parse().unwrap());
            headers.insert("fastly-client-ip", "10.0.0.8".parse().unwrap());
            headers.insert("x-original-url", "/admin".parse().unwrap());
            headers.insert("x-rewrite-url", "/admin".parse().unwrap());
            headers.insert("client-ip", "10.0.0.9".parse().unwrap());
            headers.insert("x-originating-ip", "10.0.0.10".parse().unwrap());
            headers.insert("x-remote-addr", "10.0.0.11".parse().unwrap());
            headers.insert("x-original-host", "internal".parse().unwrap());
            headers.insert("x-http-method-override", "DELETE".parse().unwrap());
            headers.insert("x-remote-ip", "10.0.0.12".parse().unwrap());
            headers.insert("x-original-forwarded-for", "10.0.0.13".parse().unwrap());
            headers.insert("x-original-uri", "/admin".parse().unwrap());
            headers.insert("x-host", "internal".parse().unwrap());
            headers.insert("x-http-method", "DELETE".parse().unwrap());
            headers.insert("x-method-override", "DELETE".parse().unwrap());
            headers.insert("x-envoy-original-path", "/admin".parse().unwrap());
            headers.insert("via", "1.1 internal".parse().unwrap());
            headers.insert("x-proxyuser-ip", "10.0.0.14".parse().unwrap());
            headers.insert("x-custom", "keep".parse().unwrap());
            headers
        };

        let headers = apply_passthrough(&build(), &PassthroughMode::All, &[]);
        for name in [
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
            "x-real-ip",
            "forwarded",
            "x-forwarded",
            "x-client-ip",
            "true-client-ip",
            "cf-connecting-ip",
            "fastly-client-ip",
            "x-original-url",
            "x-rewrite-url",
            "client-ip",
            "x-originating-ip",
            "x-remote-addr",
            "x-original-host",
            "x-http-method-override",
            "x-remote-ip",
            "x-original-forwarded-for",
            "x-original-uri",
            "x-host",
            "x-http-method",
            "x-method-override",
            "x-envoy-original-path",
            "via",
            "x-proxyuser-ip",
        ] {
            assert!(headers.get(name).is_none(), "{name} was forwarded");
        }
        assert_eq!(headers.get("x-custom").unwrap(), "keep");

        let headers = apply_passthrough(
            &build(),
            &PassthroughMode::Allowlist,
            &["X-Forwarded-For".to_owned(), "x-custom".to_owned()],
        );
        assert_eq!(headers.get("x-forwarded-for").unwrap(), "10.0.0.1");
        assert!(headers.get("x-forwarded-host").is_none());
        assert!(headers.get("x-real-ip").is_none());
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

        apply_request_header_rules(&mut headers, &rules);
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

        apply_request_header_rules(&mut headers, &rules);
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

        apply_request_header_rules(&mut headers, &rules);
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

    // -- is_websocket_upgrade tests --

    #[test]
    fn websocket_upgrade_detected() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("connection", "Upgrade".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "WebSocket".parse().unwrap());
        headers.insert("connection", "upgrade".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_multi_value() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "h2c, websocket".parse().unwrap());
        headers.insert("connection", "keep-alive, Upgrade".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_absent() {
        let headers = HeaderMap::new();
        assert!(!is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_non_websocket() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "h2c".parse().unwrap());
        headers.insert("connection", "Upgrade".parse().unwrap());
        assert!(!is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_missing_connection() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "websocket".parse().unwrap());
        assert!(!is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_connection_without_upgrade_token() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("connection", "keep-alive".parse().unwrap());
        assert!(!is_websocket_upgrade(&headers));
    }

    #[test]
    fn websocket_upgrade_multiple_header_instances() {
        let mut headers = HeaderMap::new();
        headers.append("upgrade", "h2c".parse().unwrap());
        headers.append("upgrade", "websocket".parse().unwrap());
        headers.append("connection", "keep-alive".parse().unwrap());
        headers.append("connection", "Upgrade".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));
    }

    // -- strip_hop_by_hop_for_upgrade tests --

    #[test]
    fn upgrade_strip_preserves_upgrade_and_connection() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("connection", "Upgrade".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("x-custom", "keep".parse().unwrap());

        strip_hop_by_hop_for_upgrade(&mut headers);

        assert_eq!(headers.get("upgrade").unwrap(), "websocket");
        assert_eq!(headers.get("connection").unwrap(), "Upgrade");
        assert!(headers.get("transfer-encoding").is_none());
        assert!(headers.get("keep-alive").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep");
    }

    #[test]
    fn upgrade_strip_removes_connection_nominated_except_upgrade() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "Upgrade, X-Custom-Hop".parse().unwrap());
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("x-custom-hop", "secret".parse().unwrap());
        headers.insert("x-safe", "keep".parse().unwrap());

        strip_hop_by_hop_for_upgrade(&mut headers);

        assert!(headers.get("x-custom-hop").is_none());
        assert_eq!(headers.get("upgrade").unwrap(), "websocket");
        assert!(headers.get("connection").is_some());
        assert_eq!(headers.get("x-safe").unwrap(), "keep");
    }

    // -- sanitize_response_headers_for_upgrade tests --

    #[test]
    fn sanitize_upgrade_response_preserves_upgrade_strips_internal() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "websocket".parse().unwrap());
        headers.insert("connection", "Upgrade".parse().unwrap());
        headers.insert("x-oagw-debug", "true".parse().unwrap());
        headers.insert("x-custom", "keep".parse().unwrap());

        sanitize_response_headers_for_upgrade(&mut headers);

        assert_eq!(headers.get("upgrade").unwrap(), "websocket");
        assert_eq!(headers.get("connection").unwrap(), "Upgrade");
        assert!(headers.get("x-oagw-debug").is_none());
        assert_eq!(headers.get("x-custom").unwrap(), "keep");
    }

    #[test]
    fn response_header_rules_set_add_remove() {
        let mut headers = HeaderMap::new();
        headers.insert("x-remove-me", "gone".parse().unwrap());
        headers.insert("x-overwrite", "old".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let rules = ResponseHeaderRules {
            set: [("x-overwrite".into(), "new".into())].into_iter().collect(),
            add: [("x-extra".into(), "added".into())].into_iter().collect(),
            remove: vec!["x-remove-me".into()],
        };

        apply_response_header_rules(&mut headers, &rules);

        assert!(headers.get("x-remove-me").is_none());
        assert_eq!(headers.get("x-overwrite").unwrap(), "new");
        assert_eq!(headers.get("x-extra").unwrap(), "added");
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn response_header_rules_empty_is_noop() {
        let mut headers = HeaderMap::new();
        headers.insert("x-keep", "value".parse().unwrap());

        let rules = ResponseHeaderRules::default();
        apply_response_header_rules(&mut headers, &rules);

        assert_eq!(headers.get("x-keep").unwrap(), "value");
    }

    #[test]
    fn valid_content_type_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        assert!(validate_content_type(&headers).is_ok());
    }

    #[test]
    fn valid_content_type_with_charset_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
        assert!(validate_content_type(&headers).is_ok());
    }

    #[test]
    fn missing_content_type_accepted() {
        let headers = HeaderMap::new();
        assert!(validate_content_type(&headers).is_ok());
    }

    #[test]
    fn invalid_content_type_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "not a valid mime type!!!".parse().unwrap());
        match validate_content_type(&headers).unwrap_err() {
            DomainError::Validation { field, reason, .. } => {
                assert_eq!(field, "content-type");
                assert_eq!(reason, "INVALID_MIME_TYPE");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn duplicate_content_type_rejected() {
        let mut headers = HeaderMap::new();
        headers.append("content-type", "application/json".parse().unwrap());
        headers.append("content-type", "text/html".parse().unwrap());
        match validate_content_type(&headers).unwrap_err() {
            DomainError::Validation { field, reason, .. } => {
                assert_eq!(field, "content-type");
                assert_eq!(reason, "DUPLICATE_HEADER");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn extract_client_ip_from_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        assert_eq!(extract_client_ip(&headers), Some("1.2.3.4".to_string()));
    }

    #[test]
    fn extract_client_ip_from_xff_single() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1".parse().unwrap());
        assert_eq!(extract_client_ip(&headers), Some("10.0.0.1".to_string()));
    }

    #[test]
    fn extract_client_ip_from_real_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "192.168.1.1".parse().unwrap());
        assert_eq!(extract_client_ip(&headers), Some("192.168.1.1".to_string()));
    }

    #[test]
    fn extract_client_ip_xff_takes_precedence() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        headers.insert("x-real-ip", "5.6.7.8".parse().unwrap());
        assert_eq!(extract_client_ip(&headers), Some("1.2.3.4".to_string()));
    }

    #[test]
    fn extract_client_ip_none() {
        let headers = HeaderMap::new();
        assert_eq!(extract_client_ip(&headers), None);
    }

    // -- validate_smuggling_headers tests --

    #[test]
    fn smuggling_safe_no_cl_no_te() {
        let headers = HeaderMap::new();
        assert!(validate_smuggling_headers(&headers).is_ok());
    }

    #[test]
    fn smuggling_safe_only_content_length() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "42".parse().unwrap());
        assert!(validate_smuggling_headers(&headers).is_ok());
    }

    #[test]
    fn smuggling_safe_content_length_zero() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "0".parse().unwrap());
        assert!(validate_smuggling_headers(&headers).is_ok());
    }

    #[test]
    fn smuggling_safe_content_length_with_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", " 42 ".parse().unwrap());
        assert!(validate_smuggling_headers(&headers).is_ok());
    }

    #[test]
    fn smuggling_safe_only_transfer_encoding() {
        let mut headers = HeaderMap::new();
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        assert!(validate_smuggling_headers(&headers).is_ok());
    }

    #[test]
    fn smuggling_safe_te_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("transfer-encoding", "Chunked".parse().unwrap());
        assert!(validate_smuggling_headers(&headers).is_ok());
    }

    #[test]
    fn smuggling_reject_both_cl_and_te() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "42".parse().unwrap());
        headers.insert("transfer-encoding", "chunked".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "transfer-encoding");
                assert_eq!(reason, "CL_TE_COEXISTENCE");
                assert_eq!(
                    detail,
                    "request contains both Content-Length and Transfer-Encoding"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_duplicate_content_length() {
        let mut headers = HeaderMap::new();
        headers.append("content-length", "42".parse().unwrap());
        headers.append("content-length", "99".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "content-length");
                assert_eq!(reason, "DUPLICATE_HEADER");
                assert_eq!(detail, "duplicate Content-Length headers");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_duplicate_content_length_same_value() {
        let mut headers = HeaderMap::new();
        headers.append("content-length", "42".parse().unwrap());
        headers.append("content-length", "42".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "content-length");
                assert_eq!(reason, "DUPLICATE_HEADER");
                assert_eq!(detail, "duplicate Content-Length headers");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_negative_content_length() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "-1".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "content-length");
                assert_eq!(reason, "INVALID_VALUE");
                assert_eq!(detail, "Content-Length is not a valid integer");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_non_integer_content_length() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "abc".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "content-length");
                assert_eq!(reason, "INVALID_VALUE");
                assert_eq!(detail, "Content-Length is not a valid integer");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_te_gzip() {
        let mut headers = HeaderMap::new();
        headers.insert("transfer-encoding", "gzip".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "transfer-encoding");
                assert_eq!(reason, "UNSUPPORTED_VALUE");
                assert_eq!(
                    detail,
                    "unsupported Transfer-Encoding; only chunked is accepted"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_te_mixed() {
        let mut headers = HeaderMap::new();
        headers.insert("transfer-encoding", "gzip, chunked".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "transfer-encoding");
                assert_eq!(reason, "UNSUPPORTED_VALUE");
                assert_eq!(
                    detail,
                    "unsupported Transfer-Encoding; only chunked is accepted"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn smuggling_reject_duplicate_te() {
        let mut headers = HeaderMap::new();
        headers.append("transfer-encoding", "chunked".parse().unwrap());
        headers.append("transfer-encoding", "chunked".parse().unwrap());
        match validate_smuggling_headers(&headers).unwrap_err() {
            DomainError::Validation {
                field,
                reason,
                detail,
                ..
            } => {
                assert_eq!(field, "transfer-encoding");
                assert_eq!(reason, "DUPLICATE_HEADER");
                assert_eq!(detail, "duplicate Transfer-Encoding headers");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
