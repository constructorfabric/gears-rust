//! The read side of the migration: the old value store.

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::LegacyStoreError;

/// The OLD store, addressed the way the shipped gear addressed it:
/// `(tenant_id, reference, owner_id)`, where `owner_id` is `Some` only for a
/// private record (the owner's key class) and `None` for the tenant key class.
///
/// Implement it as a thin adapter over the pre-0.3 plugin code, so a plugin
/// that encrypts in-process decrypts through its own code. Implementations
/// must never put a value into an error message.
#[async_trait]
pub trait LegacyValueStore: Send + Sync {
    /// Reads the value at the legacy address; `Ok(None)` when absent.
    ///
    /// # Errors
    ///
    /// [`LegacyStoreError`] when the store cannot answer.
    async fn get(
        &self,
        tenant_id: Uuid,
        reference: &str,
        owner_id: Option<Uuid>,
    ) -> Result<Option<Vec<u8>>, LegacyStoreError>;

    /// Deletes the value at the legacy address. An absent entry is success.
    ///
    /// # Errors
    ///
    /// [`LegacyStoreError`] when the store cannot perform the delete.
    async fn delete(
        &self,
        tenant_id: Uuid,
        reference: &str,
        owner_id: Option<Uuid>,
    ) -> Result<(), LegacyStoreError>;
}
