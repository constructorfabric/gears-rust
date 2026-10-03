//! Skeleton of an operator binary. Copy it into your own crate, replace the
//! placeholders with your old plugin (read side) and your new V2 plugin
//! (write side), and build it.
//!
//! ```text
//! cargo run --example migrate -- --results r.jsonl --database-url sqlite://db.sqlite copy
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStoreError, CredStorePluginClientV2, SecretValue, StoreKey, ValueVersion};
use credstore_value_migration::{LegacyStoreError, LegacyValueStore, run_cli};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Placeholder for your pre-0.3 plugin (the code that decrypts in-process).
struct OldPlugin;

/// Adapter: addresses the old plugin exactly as the shipped gear did.
struct MyLegacy(OldPlugin);

#[async_trait]
impl LegacyValueStore for MyLegacy {
    async fn get(
        &self,
        _tenant_id: Uuid,
        _reference: &str,
        _owner_id: Option<Uuid>,
    ) -> Result<Option<Vec<u8>>, LegacyStoreError> {
        let _ = &self.0;
        Err(LegacyStoreError::new(
            "placeholder: call the old plugin's get",
        ))
    }

    async fn delete(
        &self,
        _tenant_id: Uuid,
        _reference: &str,
        _owner_id: Option<Uuid>,
    ) -> Result<(), LegacyStoreError> {
        Err(LegacyStoreError::new(
            "placeholder: call the old plugin's delete",
        ))
    }
}

/// Placeholder for your new `CredStorePluginClientV2` implementation,
/// constructed directly (outside the `ClientHub`) with the deployment's own
/// configuration.
struct NewPlugin;

#[async_trait]
impl CredStorePluginClientV2 for NewPlugin {
    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        Err(CredStoreError::internal("placeholder: your new plugin"))
    }

    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        Err(CredStoreError::internal("placeholder: your new plugin"))
    }

    async fn delete_key(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
    ) -> Result<(), CredStoreError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    run_cli(Arc::new(MyLegacy(OldPlugin)), Arc::new(NewPlugin)).await
}
