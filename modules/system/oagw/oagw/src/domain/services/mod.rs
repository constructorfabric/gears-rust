pub(crate) mod client;
pub(crate) mod management;

pub(crate) use client::ServiceGatewayClientV1Facade;
pub(crate) use management::ControlPlaneServiceImpl;

use async_trait::async_trait;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use oagw_sdk::Body;
use uuid::Uuid;

use std::net::SocketAddr;

use crate::domain::error::DomainError;
use crate::domain::model::{
    CreateRouteRequest, CreateUpstreamRequest, Endpoint, ListQuery, Route, UpdateRouteRequest,
    UpdateUpstreamRequest, Upstream,
};

/// Result of endpoint selection: the domain endpoint plus an optional
/// pre-resolved socket address from the load balancer's DNS cache.
#[domain_model]
#[derive(Debug, Clone)]
pub(crate) struct SelectedEndpoint {
    pub endpoint: Endpoint,
    /// When set, `upstream_peer` can skip DNS and connect directly.
    pub resolved_addr: Option<SocketAddr>,
}

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
        upstream_id: Uuid,
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

/// Internal Data Plane service trait — proxy orchestration and plugin execution.
#[async_trait]
pub(crate) trait DataPlaneService: Send + Sync {
    async fn proxy_request(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, DomainError>;

    /// Remove a rate-limit bucket by key (e.g. `"upstream:{id}"` or `"route:{id}"`).
    fn remove_rate_limit_key(&self, key: &str);
}

/// Endpoint selection abstraction for multi-endpoint load balancing.
///
/// Implementations select the next healthy endpoint for a given upstream.
#[async_trait]
pub(crate) trait EndpointSelector: Send + Sync {
    /// Select the next healthy endpoint for the given upstream.
    /// Returns `None` if all backends are unhealthy or the endpoint list is empty.
    async fn select(&self, upstream_id: Uuid, endpoints: &[Endpoint]) -> Option<SelectedEndpoint>;

    /// Invalidate cached state for the given upstream (called on CRUD).
    fn invalidate(&self, upstream_id: Uuid);
}
