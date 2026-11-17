use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::body::Body;
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
/// All fallible methods return `Result<_, CanonicalError>` — the platform's
/// canonical error type — so consumers can propagate or wrap failures
/// uniformly across SDKs.
///
/// For flat typed dispatch on common failure modes, project at the call
/// site via [`crate::ServiceGatewayError::from`]:
///
/// ```ignore
/// // Path A: propagate canonical (default; lossless, uniform across SDKs)
/// let upstream = gw.get_upstream(ctx, id).await?;
///
/// // Path B: project to typed for flat dispatch
/// let upstream = gw.get_upstream(ctx, id).await
///     .map_err(oagw_sdk::ServiceGatewayError::from)?;
/// ```
///
/// The projection is infallible (`From<CanonicalError>` lands in
/// [`crate::ServiceGatewayError::Other`] for unmodeled categories). See
/// [`crate::error`] for the disposition reference and the typed
/// sub-enums in [`crate::field`], [`crate::reason`], [`crate::gts`].
#[async_trait]
pub trait ServiceGatewayClientV1: Send + Sync {
    // -- Upstream CRUD --

    /// Create an upstream. Needs `create` on `gts.cf.core.oagw.upstream.v1~`
    /// in the caller's tenant; a refusal is `PermissionDenied` (403).
    async fn create_upstream(
        &self,
        ctx: SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, CanonicalError>;

    /// Get an upstream of the caller's tenant. Needs `read` on it; a
    /// refusal is `PermissionDenied` (403).
    async fn get_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
    ) -> Result<Upstream, CanonicalError>;

    /// List the caller's tenant's upstreams. Needs `read` on
    /// `gts.cf.core.oagw.upstream.v1~`; a refusal is `PermissionDenied` (403).
    async fn list_upstreams(
        &self,
        ctx: SecurityContext,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, CanonicalError>;

    /// Replace an upstream. Needs `override` on it (not `read`); a refusal is
    /// `PermissionDenied` (403).
    async fn update_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, CanonicalError>;

    /// Delete an upstream and its routes. Needs `delete` on it; a refusal is
    /// `PermissionDenied` (403).
    async fn delete_upstream(&self, ctx: SecurityContext, id: Uuid) -> Result<(), CanonicalError>;

    // -- Route CRUD --

    /// Create a route. Needs `create` on `gts.cf.core.oagw.route.v1~` in the
    /// caller's tenant; a refusal is `PermissionDenied` (403).
    async fn create_route(
        &self,
        ctx: SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, CanonicalError>;

    /// Get a route of the caller's tenant. Needs `read` on it; a refusal is
    /// `PermissionDenied` (403).
    async fn get_route(&self, ctx: SecurityContext, id: Uuid) -> Result<Route, CanonicalError>;

    /// List the caller's tenant's routes. Needs `read` on
    /// `gts.cf.core.oagw.route.v1~`; a refusal is `PermissionDenied` (403).
    async fn list_routes(
        &self,
        ctx: SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, CanonicalError>;

    /// Replace a route. Needs `override` on it (not `read`); a refusal is
    /// `PermissionDenied` (403).
    async fn update_route(
        &self,
        ctx: SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, CanonicalError>;

    /// Delete a route. Needs `delete` on it; a refusal is
    /// `PermissionDenied` (403).
    async fn delete_route(&self, ctx: SecurityContext, id: Uuid) -> Result<(), CanonicalError>;

    // -- Resolution --

    /// Resolve the effective (hierarchy-merged) upstream and matched route for
    /// the given alias, HTTP method, and path — without executing the proxy
    /// pipeline (no auth, rate-limiting, or forwarding).
    ///
    /// Applies alias shadowing and returns the merged configuration, for
    /// diagnostics and config preview. The path is matched in the form the
    /// proxy matches it in.
    ///
    /// The upstream's `rate_limit` is the tenant-merged upstream limit; the
    /// route's is the route's own. The proxy checks each on its own bucket.
    /// When the route shadows an ancestor's route for the same request, the
    /// proxy also checks that route's enforced rate limit on that route's
    /// bucket; it is not part of the answer. The enforced plugins of such a
    /// route are in the upstream's `plugins`.
    ///
    /// # Errors
    ///
    /// - `PermissionDenied` (403) without `read` on upstreams in the
    ///   caller's tenant (checked first), on the upstreams of every tenant
    ///   in its chain (any of them may contribute merged settings), on the
    ///   returned route, or on an ancestor route the returned route shadows
    ///   at least one of whose enforced plugins the answer carries.
    /// - `InvalidArgument` (400) for a path the proxy would reject: an
    ///   encoded dot-segment, a `;` path parameter, a stray `%`.
    /// - `NotFound` when no upstream or route matches.
    async fn resolve_proxy_target(
        &self,
        ctx: SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
    ) -> Result<(Upstream, Route), CanonicalError>;

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
    ) -> Result<http::Response<Body>, CanonicalError>;
}
