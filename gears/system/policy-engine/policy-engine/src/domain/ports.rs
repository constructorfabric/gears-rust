//! Outbound ports of the domain: tenant hierarchy, types registry and the
//! active-content read of the decision path.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::BarrierMode;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::model::{Assignment, BundleVersion};

/// Why a dependency could not answer.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortError {
    /// The dependency did not answer in time.
    #[error("dependency timed out")]
    Timeout,
    /// The dependency failed or answered unusably.
    #[error("dependency unavailable: {0}")]
    Unavailable(String),
    /// The dependency does not know (or will not show) the subject.
    #[error("not found: {0}")]
    NotFound(String),
}

/// Tenant hierarchy reads, always under the caller's context.
#[async_trait]
pub trait HierarchyPort: Send + Sync {
    /// The tenant and its ancestors, nearest first, stopping at the first
    /// self-managed barrier. Every tenant status counts.
    async fn ancestors(&self, ctx: &SecurityContext, tenant: Uuid) -> Result<Vec<Uuid>, PortError>;

    /// Whether `from_tenant` is `target_tenant` or an ancestor of it.
    async fn is_reachable(
        &self,
        ctx: &SecurityContext,
        from_tenant: Uuid,
        target_tenant: Uuid,
        barrier: BarrierMode,
    ) -> Result<bool, PortError>;
}

/// Lookup of concrete type identifiers, used at validation time only.
#[async_trait]
pub trait TypeCatalogPort: Send + Sync {
    /// The subset of `ids` the types registry knows.
    async fn known_types(&self, ids: &[String]) -> Result<HashSet<String>, PortError>;
}

/// An assignment with the active version of its bundle.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveBinding {
    /// The assignment.
    pub assignment: Assignment,
    /// The bundle's active version, with its documents.
    pub version: Arc<BundleVersion>,
}

/// Reads the active bindings of a tenant chain.
#[async_trait]
pub trait BindingSource: Send + Sync {
    /// Assignments at any of `tenants` whose bundle has an active version.
    async fn active_bindings(&self, tenants: &[Uuid]) -> Result<Vec<ActiveBinding>, PortError>;
}
