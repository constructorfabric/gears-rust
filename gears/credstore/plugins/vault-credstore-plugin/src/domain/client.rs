//! SDK adapter: implements `credstore_sdk::plugin_api::CredStorePluginClientV2`
//! for [`Service`] by delegating to its HTTP methods.
//!
//! Requests are already authorized and resolved by the host gear, so this
//! adapter - like the static plugin's - ignores the security context and
//! keys purely on `(tenant_id, record_id)`.
use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV2, DestroySelector, SecretValue, StoreKey, ValueVersion,
};
use toolkit_security::SecurityContext;

use super::service::Service;

#[async_trait]
impl CredStorePluginClientV2 for Service {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        self.put_value(key, value).await
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        self.get_value(key, version).await
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
    ) -> Result<(), CredStoreError> {
        self.delete_key_value(key).await
    }

    fn supports_destroy(&self) -> bool {
        true
    }

    async fn destroy(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        selector: DestroySelector,
    ) -> Result<(), CredStoreError> {
        self.destroy_value(key, &selector).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
