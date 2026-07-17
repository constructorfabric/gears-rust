// Updated: 2026-04-07 by Constructor Tech
use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretMetadata, SecretRef, SecretValue,
    SharingMode, TenantId,
};
use toolkit_security::SecurityContext;

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

    async fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        // Inherent `Service::put` takes precedence over the trait method here.
        Service::put(self, ctx, key, value, sharing);
        Ok(())
    }

    async fn delete(&self, ctx: &SecurityContext, key: &SecretRef) -> Result<(), CredStoreError> {
        Service::delete(self, ctx, key);
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
