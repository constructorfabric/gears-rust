use authz_resolver_sdk::pep::ResourceType;

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

/// Header filtering and internal header helpers.
pub(crate) mod headers;
/// Pingora `ProxyHttp` implementation and endpoint selector.
pub(crate) mod pingora_proxy;
/// Builder for outbound upstream requests.
pub(crate) mod request_builder;
/// Data-plane service implementation.
pub(crate) mod service;
/// Bridge between Axum sessions and the Pingora proxy.
pub(crate) mod session_bridge;
/// WebSocket tunnel relay.
pub(crate) mod websocket;

pub(crate) use service::DataPlaneServiceImpl;

/// Authorization resource types used by the proxy.
pub(crate) mod resources {
    use super::ResourceType;
    use oagw_sdk::PROXY_SCHEMA;
    use toolkit_security::pep_properties;

    /// Resource type identifying a proxied upstream target.
    pub const PROXY: ResourceType =
        ResourceType::from_static(PROXY_SCHEMA, &[pep_properties::OWNER_TENANT_ID]);
}

/// Authorization action names used by the proxy.
pub(crate) mod actions {
    /// Action name for invoking (proxying a request to) an upstream.
    pub const INVOKE: &str = "invoke";
}
