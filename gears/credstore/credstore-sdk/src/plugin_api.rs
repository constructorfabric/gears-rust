use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{OwnerId, SecretRef, SecretValue, TenantId};

/// Pure per-tenant value store. `owner_id=Some` selects the owner's private
/// key class; `None` selects the tenant key class. No sharing/hierarchy/policy.
#[async_trait]
pub trait CredStorePluginClientV1: Send + Sync {
    /// Retrieves a secret from the backend.
    async fn get(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<SecretValue>, CredStoreError>;

    /// Stores a secret in the backend.
    async fn put(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        value: SecretValue,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError>;

    /// Deletes a secret from the backend.
    async fn delete(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError>;
}
