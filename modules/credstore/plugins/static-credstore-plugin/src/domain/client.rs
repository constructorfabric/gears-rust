use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretMetadata, SecretRef, SecretValue,
    TenantId,
};
use modkit_security::SecurityContext;

use super::service::Service;

#[async_trait]
impl CredStorePluginClientV1 for Service {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<SecretMetadata>, CredStoreError> {
        let Some(entry) = self.get(ctx, key) else {
            return Ok(None);
        };

        // For Shared/Tenant entries the stored owner_id/owner_tenant_id are nil
        // placeholders — resolve them from the caller's security context.
        let owner_id = if entry.owner_id.is_nil() {
            OwnerId(ctx.subject_id())
        } else {
            entry.owner_id
        };
        let owner_tenant_id = if entry.owner_tenant_id.is_nil() {
            TenantId(ctx.subject_tenant_id())
        } else {
            entry.owner_tenant_id
        };

        Ok(Some(SecretMetadata {
            value: SecretValue::new(entry.value.as_bytes().to_vec()),
            owner_id,
            sharing: entry.sharing,
            owner_tenant_id,
        }))
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod tests;
