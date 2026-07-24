// Updated: 2026-06-06 — implements the per-tenant value-store `CredStorePluginClientV1`.
use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretRef, SecretValue, TenantId,
};
use toolkit_security::SecurityContext;

use super::service::Service;

/// The static plugin is a pure per-tenant value store: it ignores the
/// security context (the gear has already authorized the request and
/// resolved tenant/owner) and keys purely on `(tenant_id, key, owner_id)`.
#[async_trait]
impl CredStorePluginClientV1 for Service {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        Ok(self.get_value(tenant_id, key, owner_id))
    }

    async fn put(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        value: SecretValue,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        self.put_value(tenant_id, key, value, owner_id);
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<(), CredStoreError> {
        self.delete_value(tenant_id, key, owner_id);
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
