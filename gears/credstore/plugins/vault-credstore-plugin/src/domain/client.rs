//! SDK adapter: implements `credstore_sdk::plugin_api::CredStorePluginClientV1`
//! for [`Service`] by delegating to its HTTP methods.
//!
//! Requests are already authorized and resolved by the host gear, so this
//! adapter — like the static plugin's — ignores the security context and
//! keys purely on `(tenant_id, value_id)`.
use async_trait::async_trait;
use credstore_sdk::{CredStoreError, CredStorePluginClientV1, SecretValue, TenantId, ValueId};
use toolkit_security::SecurityContext;

use super::service::Service;

#[async_trait]
impl CredStorePluginClientV1 for Service {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        value_id: &ValueId,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        self.get_value(tenant_id, value_id).await
    }

    async fn put(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        value_id: &ValueId,
        value: SecretValue,
    ) -> Result<(), CredStoreError> {
        self.put_value(tenant_id, value_id, value).await
    }

    async fn delete(
        &self,
        _ctx: &SecurityContext,
        tenant_id: &TenantId,
        value_id: &ValueId,
    ) -> Result<(), CredStoreError> {
        self.delete_value(tenant_id, value_id).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
