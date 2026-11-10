//! Tenant hierarchy resolution boundary used by inherited-secret lookup.

use async_trait::async_trait;
use credstore_sdk::TenantId;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Resolves a tenant's ancestor chain (self first, root last).
#[async_trait]
pub trait TenantDirectory: Send + Sync {
    /// Returns `[req, parent, …, root]` (req first), self included.
    async fn ancestor_chain(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
    ) -> Result<Vec<Uuid>, DomainError>;

    /// True iff `ancestor` is a strict ancestor of `descendant` in the
    /// tenant hierarchy, isolation barriers ignored (the hierarchy
    /// inheritance walks). A `descendant` tenant-resolver does not know
    /// (deleted, rows left behind) is simply not a descendant.
    async fn is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor: TenantId,
        descendant: TenantId,
    ) -> Result<bool, DomainError>;
}
