//! Trait for reading upstreams and routes from the Types Registry.
//!
//! During `post_init()`, OAGW reads GTS instances registered by other modules
//! and materializes them into the in-memory upstream/route repositories.

use async_trait::async_trait;
use modkit_macros::domain_model;

use super::error::DomainError;
use super::model::{CreateRouteRequest, CreateUpstreamRequest};
use uuid::Uuid;

/// An upstream definition read from the types-registry.
///
/// `gts_instance_id` is the UUID parsed from the GTS entity identifier
/// (e.g. the `<uuid>` part of `gts.x.core.oagw.upstream.v1~<uuid>`).
/// Used by `post_init()` to map GTS-level references in route entities
/// to OAGW-assigned upstream UUIDs.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProvisionedUpstream {
    pub tenant_id: Uuid,
    pub request: CreateUpstreamRequest,
    pub gts_instance_id: Option<Uuid>,
}

/// A route definition read from the types-registry.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProvisionedRoute {
    pub tenant_id: Uuid,
    pub request: CreateRouteRequest,
}

/// Reads upstream and route GTS instances from the Types Registry.
///
/// Other modules register upstream/route instances during `init()`.
/// OAGW calls these methods during `post_init()` to discover and
/// materialize them into the in-memory repositories.
#[async_trait]
pub trait TypeProvisioningService: Send + Sync {
    /// List all upstream instances registered in the types-registry.
    async fn list_upstreams(&self) -> Result<Vec<ProvisionedUpstream>, DomainError>;

    /// List all route instances registered in the types-registry.
    async fn list_routes(&self) -> Result<Vec<ProvisionedRoute>, DomainError>;
}
