/// Facade implementing the public SDK client on top of the internal services.
pub(crate) mod client;
/// Control-plane (configuration management) service implementation.
pub(crate) mod management;
pub(crate) mod registry_job;
pub(crate) mod registry_reconcile;
pub(crate) mod registry_state;
pub(crate) mod system_identity;

pub(crate) use client::ServiceGatewayClientV1Facade;
pub(crate) use management::{ControlPlaneServiceImpl, RegistryProvisionerImpl};

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
use crate::domain::repo::{DeleteOutcome, Tags};

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

/// Whether a registry route write reads its upstream first.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamCheck {
    /// Read it: it must exist in the tenant and be registry-managed.
    Read,
    /// The reconcile applied this registry upstream in this run; skip the
    /// read. The database's foreign key still rejects a missing upstream.
    KnownRegistry,
}

/// The types-registry reconcile's access to the Control Plane
/// (`cpt-cf-oagw-flow-domain-registry-provisioning`). Rows written here are
/// registry-managed: [`ControlPlaneService`] writes reject them, and these
/// writes reject rows the Management API created. Validation is the same as
/// for API writes. A separate object from the [`ControlPlaneService`] the
/// Management API and the SDK hold ([`management::RegistryProvisionerImpl`]),
/// and the only holder of the registry rows across tenants, so no request
/// path can reach these writes or that read.
#[async_trait]
pub(crate) trait RegistryProvisioner: Send + Sync {
    /// The upstream `id` of the context's tenant, whoever manages it.
    async fn get_upstream(&self, ctx: &SecurityContext, id: Uuid) -> Result<Upstream, DomainError>;

    /// The route `id` of the context's tenant, whoever manages it.
    async fn get_route(&self, ctx: &SecurityContext, id: Uuid) -> Result<Route, DomainError>;

    /// Routes of the context's tenant, optionally on one upstream.
    async fn list_routes(
        &self,
        ctx: &SecurityContext,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, DomainError>;

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

    /// The checks of [`Self::create_registry_upstream`] that read nothing;
    /// returns the alias it would store.
    fn check_registry_upstream(&self, req: &CreateUpstreamRequest) -> Result<String, DomainError>;

    async fn create_registry_route(
        &self,
        ctx: &SecurityContext,
        req: CreateRouteRequest,
        upstream: UpstreamCheck,
    ) -> Result<Route, DomainError>;

    /// Full replacement of a registry route, on `upstream_id`: a registry
    /// route moves to another upstream in place (the Management API cannot
    /// move a route).
    async fn update_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        upstream_id: Uuid,
        req: UpdateRouteRequest,
        upstream: UpstreamCheck,
    ) -> Result<Route, DomainError>;

    async fn delete_registry_route(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), DomainError>;

    /// Delete a registry upstream only if no route uses it, checked and
    /// deleted as one step ([`crate::domain::repo::RegistryRowRepository::delete_upstream_if_unused`]).
    async fn delete_registry_upstream_if_unused(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<DeleteOutcome, DomainError>;

    /// Every registry-managed upstream in full, across all tenants, in key order.
    async fn registry_upstreams(&self) -> Result<Vec<Upstream>, DomainError>;

    /// Every registry-managed route in full, across all tenants, in key order.
    async fn registry_routes(&self) -> Result<Vec<Route>, DomainError>;
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
