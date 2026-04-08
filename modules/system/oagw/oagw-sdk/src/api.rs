use async_trait::async_trait;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::body::Body;
use crate::error::ServiceGatewayError;
use crate::{
    CreateRouteRequest, CreateUpstreamRequest, ListQuery, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};

// ---------------------------------------------------------------------------
// Proxy types
// ---------------------------------------------------------------------------

/// Distinguishes gateway-originated errors from upstream-originated errors.
///
/// Available on proxy responses via `resp.extensions().get::<ErrorSource>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    Gateway,
    Upstream,
}

impl ErrorSource {
    /// Returns a lowercase string representation for use in headers.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Upstream => "upstream",
        }
    }
}

// ---------------------------------------------------------------------------
// Service trait
// ---------------------------------------------------------------------------

/// Public API trait for the Outbound API Gateway (Version 1).
///
/// This trait is registered in `ClientHub` by the OAGW module:
/// ```ignore
/// let gw = hub.get::<dyn ServiceGatewayClientV1>()?;
/// ```
#[async_trait]
pub trait ServiceGatewayClientV1: Send + Sync {
    // -- Upstream CRUD --

    async fn create_upstream(
        &self,
        ctx: SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, ServiceGatewayError>;

    async fn get_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
    ) -> Result<Upstream, ServiceGatewayError>;

    async fn list_upstreams(
        &self,
        ctx: SecurityContext,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, ServiceGatewayError>;

    async fn update_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, ServiceGatewayError>;

    async fn delete_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
    ) -> Result<(), ServiceGatewayError>;

    // -- Route CRUD --

    async fn create_route(
        &self,
        ctx: SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, ServiceGatewayError>;

    async fn get_route(&self, ctx: SecurityContext, id: Uuid)
    -> Result<Route, ServiceGatewayError>;

    async fn list_routes(
        &self,
        ctx: SecurityContext,
        upstream_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Route>, ServiceGatewayError>;

    async fn update_route(
        &self,
        ctx: SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, ServiceGatewayError>;

    async fn delete_route(&self, ctx: SecurityContext, id: Uuid)
    -> Result<(), ServiceGatewayError>;

    // -- Resolution --

    /// Resolve the effective (hierarchy-merged) upstream and matched route for
    /// the given alias, HTTP method, and path — without executing the proxy
    /// pipeline (no auth, rate-limiting, or forwarding).
    ///
    /// Performs a single tenant hierarchy walk, applies alias shadowing, and
    /// returns the merged configuration. Useful for startup validation,
    /// diagnostics, and config preview.
    async fn resolve_proxy_target(
        &self,
        ctx: SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
    ) -> Result<(Upstream, Route), ServiceGatewayError>;

    // -- Proxy --

    /// Execute the full proxy pipeline: resolve -> auth -> rate-limit -> forward -> respond.
    ///
    /// The request URI must follow `/{alias}/{path_suffix}?query` convention.
    /// `ErrorSource` is available on the response via `resp.extensions().get::<ErrorSource>()`.
    ///
    /// # Protocol mapping
    ///
    /// All three protocols map to `Request<Body> → Response<Body>`:
    ///
    /// | Protocol  | Request Body          | Response Body          |
    /// |-----------|-----------------------|------------------------|
    /// | HTTP      | `Body::Bytes`/`Empty` | `Body::Bytes`          |
    /// | SSE       | `Body::Bytes`/`Empty` | `Body::Stream`         |
    /// | WebSocket | `Body::Stream`        | `Body::Stream`         |
    async fn proxy_request(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, ServiceGatewayError>;
}
