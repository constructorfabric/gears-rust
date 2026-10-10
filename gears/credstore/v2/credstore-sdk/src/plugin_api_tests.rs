// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for the default (optional) methods of [`CredStorePluginClientV2`].
//!
//! A third-party backend that implements only the three required methods
//! (`put`, `get`, `delete_key`) gets `supports_destroy` and `destroy` from the
//! trait defaults, so the defaults are that backend's production behaviour.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::{
    CredStoreError, CredStorePluginClientV2, DestroySelector, SecretValue, StoreKey, TenantId,
    ValueVersion,
};

/// A plugin that implements only the required methods and relies on the trait
/// defaults for everything optional.
struct RequiredOnlyPlugin;

#[async_trait]
impl CredStorePluginClientV2 for RequiredOnlyPlugin {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        Ok(ValueVersion::new("1"))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        Ok(None)
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
    ) -> Result<(), CredStoreError> {
        Ok(())
    }
}

fn store_key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::from_u128(1)), Uuid::from_u128(2))
}

#[test]
fn plugin_api_default_supports_destroy_is_false() {
    assert!(
        !RequiredOnlyPlugin.supports_destroy(),
        "a plugin that does not override supports_destroy must not claim destroy"
    );
}

#[tokio::test]
async fn plugin_api_default_destroy_returns_an_error() {
    let ctx = SecurityContext::anonymous();
    let key = store_key();
    let cases: Vec<(&str, DestroySelector)> = vec![
        ("Below", DestroySelector::Below(ValueVersion::new("3"))),
        ("Exactly", DestroySelector::Exactly(ValueVersion::new("3"))),
    ];
    for (name, selector) in cases {
        let err = RequiredOnlyPlugin
            .destroy(&ctx, &key, selector)
            .await
            .expect_err("the default destroy must fail, not pretend to remove versions");
        assert!(
            matches!(&err, CredStoreError::Internal(message)
                if message.contains("destroy is not supported")),
            "{name}: expected Internal(\"destroy is not supported ...\"), got: {err:?}"
        );
    }
}
