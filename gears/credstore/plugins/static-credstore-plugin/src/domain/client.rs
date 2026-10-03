//! SDK adapter for the static value store.
//!
//! Requests are already authorized and resolved by the host gear, so this
//! adapter keys values purely by `StoreKey` - see `credstore_sdk::plugin_api`.
use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV2, DestroySelector, SecretValue, StoreKey, ValueVersion,
};
use toolkit_security::SecurityContext;

use super::service::Service;

/// The static plugin ignores the security context (the gear has already
/// authorized the request) and keys purely on `(tenant_id, record_id)`.
#[async_trait]
impl CredStorePluginClientV2 for Service {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        Ok(self.put_value(key, value))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        Ok(self.get_value(key, version))
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        key: &StoreKey,
    ) -> Result<(), CredStoreError> {
        self.delete_key_value(key);
        Ok(())
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
        self.destroy_value(key, &selector);
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
