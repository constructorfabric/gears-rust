//! Header names the gateway itself sets or strips on an outbound request.
//!
//! The proxy's strip steps (`infra::proxy::headers`) and the write-time check
//! of an API-key auth `header` both read these lists, so the set of names an
//! API key may not go in cannot drift from what the gateway actually strips.

/// Hop-by-hop headers that must not be forwarded by proxies (RFC 7230 §6.1).
pub(crate) const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Prefix of the gateway's internal `X-OAGW-*` headers, stripped from every
/// outbound request.
pub(crate) const INTERNAL_HEADER_PREFIX: &str = "x-oagw-";

/// Headers the gateway sets on the outbound request itself.
pub(crate) const GATEWAY_SET_HEADERS: &[&str] = &["host", "content-length"];

/// Headers a client could use to spoof its address or origin, or to override
/// the path, host or method, at the upstream (F0008 SSRF rules): `Forwarded`
/// (RFC 7239), the `X-Forwarded*` family, every `Client-IP` variant, the
/// proxy and CDN client-address headers, and the URL, host and method
/// override headers some frameworks honour after OAGW matched the route.
/// `name` must be lowercase, as `http::HeaderName` stores it.
#[must_use]
pub(crate) fn is_forwarding_header(name: &str) -> bool {
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

/// Whether the gateway sets or strips a header of this name itself, so an API
/// key may not go in it: the headers it sets (`Host`, `Content-Length`), the
/// hop-by-hop headers, the forwarding headers and the `X-OAGW-*` namespace.
/// The gateway would overwrite or strip the key, or, for a `request_ref` key
/// injected after its own header setup (ADR-0022 guard 6), the key would land
/// in a header the gateway otherwise never forwards. Case-insensitive.
#[must_use]
pub(crate) fn is_gateway_owned_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.as_str();
    name.starts_with(INTERNAL_HEADER_PREFIX)
        || HOP_BY_HOP_HEADERS.contains(&name)
        || GATEWAY_SET_HEADERS.contains(&name)
        || is_forwarding_header(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_owned_covers_every_list_case_insensitively() {
        for name in HOP_BY_HOP_HEADERS.iter().chain(GATEWAY_SET_HEADERS) {
            assert!(is_gateway_owned_header(name), "{name}");
            assert!(
                is_gateway_owned_header(&name.to_ascii_uppercase()),
                "{name}"
            );
        }
        for name in [
            "X-OAGW-Target-Host",
            "x-oagw-anything",
            "X-Forwarded-For",
            "forwarded",
            "X-Real-IP",
            "x-envoy-original-path",
            "True-Client-IP",
            "x-http-method-override",
        ] {
            assert!(is_gateway_owned_header(name), "{name}");
        }
        for name in ["authorization", "x-api-key", "api-key", "x-goog-api-key"] {
            assert!(!is_gateway_owned_header(name), "{name}");
        }
    }
}
