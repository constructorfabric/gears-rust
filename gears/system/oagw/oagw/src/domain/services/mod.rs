/// Facade implementing the public SDK client on top of the internal services.
pub(crate) mod client;
/// Control-plane (configuration management) service implementation.
pub(crate) mod management;
pub(crate) mod registry_reconcile;

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
use crate::domain::repo::{RowKey, Tags};

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

    /// Delete an upstream and its routes.
    async fn delete_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError>;

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
    /// overrides. The proxy passes [`Tags::Skip`]: it never reads tags.
    async fn resolve_proxy_target(
        &self,
        ctx: &SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<(Upstream, Route), DomainError>;
}

/// Writes of the startup types-registry reconcile
/// (`cpt-cf-oagw-flow-domain-registry-provisioning`). Rows written here are
/// registry-managed: [`ControlPlaneService`] writes reject them, and these
/// writes reject rows the Management API created. Kept off
/// [`ControlPlaneService`], which the Management API and the SDK hold, so
/// they cannot reach it. Validation is the same as for API writes.
#[async_trait]
pub(crate) trait RegistryProvisioner: ControlPlaneService {
    async fn create_registry_upstream(
        &self,
        ctx: &SecurityContext,
        req: CreateUpstreamRequest,
    ) -> Result<Upstream, DomainError>;

    async fn update_registry_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateUpstreamRequest,
    ) -> Result<Upstream, DomainError>;

    /// Delete a registry upstream and every route on it.
    async fn delete_registry_upstream(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError>;

    async fn create_registry_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
    ) -> Result<Route, DomainError>;

    async fn update_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        req: UpdateRouteRequest,
    ) -> Result<Route, DomainError>;

    async fn delete_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError>;

    /// Every registry-managed upstream, across all tenants.
    async fn registry_upstream_keys(&self) -> Result<Vec<RowKey>, DomainError>;

    /// Every registry-managed route, across all tenants.
    async fn registry_route_keys(&self) -> Result<Vec<RowKey>, DomainError>;
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
    /// The selected endpoint.
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
