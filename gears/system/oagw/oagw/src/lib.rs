// === PUBLIC API (from SDK) ===
pub use oagw_sdk::{
    CreateRouteRequest, CreateUpstreamRequest, Endpoint, Route, ServiceGatewayError,
    UpdateRouteRequest, UpdateUpstreamRequest, Upstream, api::ServiceGatewayClientV1,
};

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::OutboundApiGatewayGear;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
/// Domain layer: models, services, ports and repository traits.
pub(crate) mod domain;
/// Infrastructure layer: proxy, plugins, metrics and storage.
pub(crate) mod infra;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
