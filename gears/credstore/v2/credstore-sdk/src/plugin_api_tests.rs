// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Unit tests for the optional destroy capability of [`CredStorePluginClientV2`].
//!
//! A third-party backend that implements only the three required methods
//! (`put`, `get`, `delete_key`) gets `destroyer() == None` from the trait
//! default, so that is its production behaviour.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use std::error::Error as _;
use std::sync::Mutex;
use std::time::Duration;

use crate::{
    CredStorePluginClientV2, CredStorePluginDestroyerV2, CredStorePluginError, DestroySelector,
    SecretValue, StoreKey, TenantId, ValueVersion,
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
    ) -> Result<ValueVersion, CredStorePluginError> {
        Ok(ValueVersion::new("1"))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStorePluginError> {
        Ok(None)
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
    ) -> Result<(), CredStorePluginError> {
        Ok(())
    }
}

fn store_key() -> StoreKey {
    StoreKey::new(TenantId(Uuid::from_u128(1)), Uuid::from_u128(2))
}

/// A plugin that implements the optional destroy capability and records what
/// it was asked to destroy.
#[derive(Default)]
struct DestroyingPlugin {
    destroyed: Mutex<Vec<DestroySelector>>,
}

#[async_trait]
impl CredStorePluginClientV2 for DestroyingPlugin {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _value: SecretValue,
    ) -> Result<ValueVersion, CredStorePluginError> {
        Ok(ValueVersion::new("1"))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStorePluginError> {
        Ok(None)
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
    ) -> Result<(), CredStorePluginError> {
        Ok(())
    }

    fn destroyer(&self) -> Option<&dyn CredStorePluginDestroyerV2> {
        Some(self)
    }
}

#[async_trait]
impl CredStorePluginDestroyerV2 for DestroyingPlugin {
    async fn destroy(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        selector: DestroySelector,
    ) -> Result<(), CredStorePluginError> {
        self.destroyed.lock().expect("lock").push(selector);
        Ok(())
    }
}

#[test]
fn plugin_without_destroyer_returns_none() {
    assert!(
        RequiredOnlyPlugin.destroyer().is_none(),
        "a plugin that does not override destroyer() must not claim destroy"
    );
}

#[tokio::test]
async fn plugin_with_destroyer_exposes_destroy_through_the_trait_object() {
    let plugin = DestroyingPlugin::default();
    let client: &dyn CredStorePluginClientV2 = &plugin;
    let destroyer = client.destroyer().expect("the plugin declares destroy");
    let ctx = SecurityContext::anonymous();
    let key = store_key();
    for selector in [
        DestroySelector::Below(ValueVersion::new("3")),
        DestroySelector::Exactly(ValueVersion::new("3")),
    ] {
        destroyer
            .destroy(&ctx, &key, selector)
            .await
            .expect("destroy succeeds");
    }
    assert_eq!(
        *plugin.destroyed.lock().expect("lock"),
        vec![
            DestroySelector::Below(ValueVersion::new("3")),
            DestroySelector::Exactly(ValueVersion::new("3")),
        ]
    );
}

#[test]
fn plugin_error_constructors_build_expected_variants() {
    assert!(matches!(
        CredStorePluginError::unavailable("down", None),
        CredStorePluginError::Unavailable {
            retry_after: None,
            source: None,
            ..
        }
    ));
    assert!(matches!(
        CredStorePluginError::unavailable("busy", Some(Duration::from_secs(5))),
        CredStorePluginError::Unavailable {
            retry_after: Some(_),
            ..
        }
    ));
    assert!(matches!(
        CredStorePluginError::credentials_rejected("403"),
        CredStorePluginError::CredentialsRejected { source: None, .. }
    ));
    assert!(matches!(
        CredStorePluginError::internal("boom"),
        CredStorePluginError::Internal { source: None, .. }
    ));
}

#[test]
fn plugin_error_display_names_the_detail() {
    assert_eq!(
        CredStorePluginError::unavailable("down", None).to_string(),
        "store unavailable: down"
    );
    assert_eq!(
        CredStorePluginError::credentials_rejected("403").to_string(),
        "store rejected the plugin's credentials: 403"
    );
    assert_eq!(
        CredStorePluginError::internal("boom").to_string(),
        "store error: boom"
    );
}

#[test]
fn with_source_attaches_a_reachable_cause_on_every_variant() {
    let variants = [
        CredStorePluginError::unavailable("a", None),
        CredStorePluginError::credentials_rejected("b"),
        CredStorePluginError::internal("c"),
    ];
    for err in variants {
        assert!(err.source().is_none());
        let err = err.with_source(std::io::Error::other("root cause"));
        let source = err.source().expect("the cause is reachable");
        assert_eq!(source.to_string(), "root cause");
    }
}

#[test]
fn retry_after_is_only_carried_by_unavailable() {
    assert_eq!(
        CredStorePluginError::unavailable("x", Some(Duration::from_secs(7))).retry_after(),
        Some(Duration::from_secs(7))
    );
    assert_eq!(
        CredStorePluginError::unavailable("x", None).retry_after(),
        None
    );
    assert_eq!(
        CredStorePluginError::credentials_rejected("x").retry_after(),
        None
    );
    assert_eq!(CredStorePluginError::internal("x").retry_after(), None);
}
