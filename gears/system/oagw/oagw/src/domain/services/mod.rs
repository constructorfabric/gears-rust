pub(crate) mod client;
pub(crate) mod management;

pub(crate) use client::ServiceGatewayClientV1Facade;
pub(crate) use management::ControlPlaneServiceImpl;

use async_trait::async_trait;
use oagw_sdk::Body;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use std::net::SocketAddr;

use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, Endpoint, ListQuery, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};

/// Internal Control Plane service trait — configuration management and resolution.
#[async_trait]
pub(crate) trait ControlPlaneService: Send + Sync {
    // -- Upstream CRUD --

    async fn create_upstream(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, DomainError>;

    async fn get_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<Upstream, DomainError>;

    async fn list_upstreams(
        &self,
        ctx: &SecurityContext,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, DomainError>;

    async fn update_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, DomainError>;

    /// Delete an upstream and cascade-delete its routes.
    /// Returns the IDs of deleted routes so callers can clean up route-scoped
    /// rate-limit keys.
    async fn delete_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError>;

    // -- Route CRUD --

    async fn create_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, DomainError>;

    async fn get_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<Route, DomainError>;

    async fn list_routes(
        &self,
        ctx: &SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError>;

    async fn update_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, DomainError>;

    async fn delete_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError>;

    // -- Resolution --

    /// Combined upstream + route resolution for the proxy hot path.
    ///
    /// Single `get_ancestors` call, correct multi-ID route matching across
    /// ancestor upstreams, and full effective config merge including route
    /// overrides.
    async fn resolve_proxy_target(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
    ) -> Result<(Upstream, Route), DomainError>;
}

/// Outcome of beginning an interactive OAuth authorization.
#[domain_model]
#[derive(Debug, Clone)]
pub(crate) struct OAuthBeginOutcome {
    pub authorization_url: String,
    pub state: String,
}

/// Per-user OAuth connection status for an upstream.
#[domain_model]
#[derive(Debug, Clone)]
pub(crate) struct OAuthConnectionStatus {
    pub connected: bool,
    pub expires_at_unix: Option<i64>,
}

/// Internal service trait for interactive per-user OAuth authorization-code
/// enrollment (out-of-band browser flow). Implemented in the infra layer
/// against credstore (token store) and an HTTP client (discovery / DCR /
/// token exchange).
#[async_trait]
pub(crate) trait OAuthEnrollmentService: Send + Sync {
    /// Discover metadata, register a client, generate PKCE, persist pending
    /// state, and return the browser authorization URL + CSRF state.
    ///
    /// `scopes` and the `redirect_uri` are NOT caller-supplied: scopes come
    /// from the upstream's stored auth config and the `redirect_uri` is the
    /// deployment-configured OAGW callback URL. `return_to` is the
    /// consumer-supplied, allowlisted URL the browser is sent to once the
    /// callback completes.
    async fn begin(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
        return_to: String,
        client_name: String,
    ) -> Result<OAuthBeginOutcome, DomainError>;

    /// Exchange the authorization code and persist the per-user token record,
    /// returning the allowlisted `return_to` URL captured at `begin` so the
    /// callback can redirect the browser there.
    ///
    /// Invoked from the unauthenticated browser callback, so it carries no
    /// `SecurityContext`: the acting identity is recovered from the pending
    /// state resolved by the unguessable CSRF `state`.
    async fn complete(&self, state: String, code: String) -> Result<String, DomainError>;

    /// Discard the pending authorization for `state` without exchanging a code
    /// (used when the authorization server returns an error redirect), and
    /// return its allowlisted `return_to` URL if the entry existed so the
    /// callback can still redirect the browser back to the app.
    async fn abort(&self, state: String) -> Option<String>;

    /// Delete the caller's stored token for an upstream.
    async fn revoke(&self, ctx: &SecurityContext, upstream_id: Uuid) -> Result<(), DomainError>;

    /// Report whether the caller has a stored token for an upstream.
    async fn status(
        &self,
        ctx: &SecurityContext,
        upstream_id: Uuid,
    ) -> Result<OAuthConnectionStatus, DomainError>;
}

/// Internal Data Plane service trait — proxy orchestration and plugin execution.
#[async_trait]
pub(crate) trait DataPlaneService: Send + Sync {
    async fn proxy_request(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, DomainError>;

    /// Remove all rate-limit buckets associated with an upstream (all scope variants).
    fn remove_rate_limit_keys_for_upstream(&self, upstream_id: Uuid);

    /// Remove all rate-limit buckets associated with a route.
    fn remove_rate_limit_keys_for_route(&self, route_id: Uuid);
}

/// Why endpoint selection failed (multi-endpoint LB path).
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub(crate) enum SelectionError {
    /// All resolved backends failed health checks.
    #[error("all backends are unhealthy")]
    AllBackendsUnhealthy,
    /// DNS resolution produced no usable addresses (DNS failure, empty result, or SSRF-filtered).
    #[error("no backend addresses could be resolved")]
    NoBackendsResolved,
}

/// Result of endpoint selection: the domain endpoint plus an optional
/// pre-resolved socket address from the load balancer's DNS cache.
#[domain_model]
#[derive(Debug, Clone)]
pub(crate) struct SelectedEndpoint {
    pub endpoint: Endpoint,
    /// When set, `upstream_peer` can skip DNS and connect directly.
    pub resolved_addr: Option<SocketAddr>,
}

/// Endpoint selection abstraction for multi-endpoint load balancing.
///
/// Implementations select the next healthy endpoint for a given upstream.
#[async_trait]
pub(crate) trait EndpointSelector: Send + Sync {
    /// Select the next healthy endpoint for the given upstream.
    /// Returns a [`SelectionError`] explaining *why* selection failed.
    async fn select(
        &self,
        upstream_id: Uuid,
        endpoints: &[Endpoint],
    ) -> Result<SelectedEndpoint, SelectionError>;

    /// Invalidate cached state for the given upstream (called on CRUD).
    fn invalidate(&self, upstream_id: Uuid);
}
