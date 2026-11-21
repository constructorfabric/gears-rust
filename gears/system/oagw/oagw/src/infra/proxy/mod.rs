/// Hop-by-hop headers that must not be forwarded by proxies (RFC 7230 §6.1).
/// Defined in the domain so the write-time auth header check reads the same
/// list.
pub(crate) use crate::domain::gateway_headers::HOP_BY_HOP_HEADERS;

#[cfg(test)]
mod aws_sigv4_tests;
#[cfg(test)]
pub(crate) mod credstore_spy;
/// Health-checked, DNS-refreshed endpoint selection.
pub(crate) mod endpoint_selector;
/// Adapter over the `toolkit-http-proxy` upstream engine.
pub(crate) mod engine;
/// Header filtering and internal header helpers.
pub(crate) mod headers;
/// Builder for outbound upstream requests.
pub(crate) mod request_builder;
/// Per-request secret references (ADR-0022).
pub(crate) mod secret_ref;
#[cfg(test)]
mod secret_ref_tests;
/// Data-plane service implementation.
pub(crate) mod service;
/// The auth plugin's signing phase (ADR-0023).
pub(crate) mod signing;
#[cfg(test)]
mod signing_tests;
/// Lifecycle management for streamed (SSE) response bodies.
pub(crate) mod streaming;
/// WebSocket tunnel relay.
pub(crate) mod websocket;

pub(crate) use service::DataPlaneServiceImpl;
