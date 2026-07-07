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
}
