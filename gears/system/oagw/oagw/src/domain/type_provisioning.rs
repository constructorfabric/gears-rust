//! Trait for reading upstreams and routes from the Types Registry.
//!
//! During `post_init()`, OAGW reads GTS instances registered by other gears
//! and materializes them into the in-memory upstream/route repositories.

use async_trait::async_trait;
use toolkit_macros::domain_model;

use super::error::DomainError;
use super::model::{CreateRouteRequest, CreateUpstreamRequest};
use uuid::Uuid;

/// An upstream definition read from the types-registry.
///
/// The GTS instance UUID is carried inside `request.id` so that OAGW
/// uses the config-provided ID directly instead of generating a random one.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProvisionedUpstream {
    /// Tenant that owns the upstream, or `None` for the default/root tenant.
    pub tenant_id: Option<Uuid>,
    /// Create request materialized from the registry instance.
    pub request: CreateUpstreamRequest,
}

/// A route definition read from the types-registry.
///
/// The GTS instance UUID is carried inside `request.id` so that OAGW
/// uses the config-provided ID directly instead of generating a random one.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProvisionedRoute {
    /// Tenant that owns the route, or `None` for the default/root tenant.
    pub tenant_id: Option<Uuid>,
    /// Create request materialized from the registry instance.
    pub request: CreateRouteRequest,
}

/// Reads upstream and route GTS instances from the Types Registry.
///
/// Other gears register upstream/route instances during `init()`.
/// OAGW calls these methods during `post_init()` to discover and
/// materialize them into the in-memory repositories.
#[async_trait]
pub trait TypeProvisioningService: Send + Sync {
    /// List all upstream instances registered in the types-registry.
    async fn list_upstreams(&self) -> Result<Vec<ProvisionedUpstream>, DomainError>;

    /// List all route instances registered in the types-registry.
    async fn list_routes(&self) -> Result<Vec<ProvisionedRoute>, DomainError>;
}
