//! Consumer contract for reading a registry SKU.
use crate::models::Sku;
use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Tenant-scoped registry SKU reads.
#[async_trait]
pub trait ProductsClient: Send + Sync {
    /// Read a SKU head in any lifecycle within the caller's authorized scope.
    /// The tenant argument narrows the lookup; it never grants access.
    /// Published-only suggestions use pricing's kept catalog contract.
    ///
    /// # Errors
    /// A canonical authorization, not-found, or infrastructure error.
    async fn get_sku(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        sku_id: Uuid,
    ) -> Result<Sku, CanonicalError>;
}
