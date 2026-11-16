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

/// Health-checked, DNS-refreshed endpoint selection.
pub(crate) mod endpoint_selector;
/// Adapter over the `toolkit-http-proxy` upstream engine.
pub(crate) mod engine;
/// Header filtering and internal header helpers.
pub(crate) mod headers;
/// Builder for outbound upstream requests.
pub(crate) mod request_builder;
/// Data-plane service implementation.
pub(crate) mod service;
/// Lifecycle management for streamed (SSE) response bodies.
pub(crate) mod streaming;
/// WebSocket tunnel relay.
pub(crate) mod websocket;

pub(crate) use service::DataPlaneServiceImpl;
