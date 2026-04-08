//! `CredStore` module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, CredStorePluginSpecV1};
use modkit::contracts::SystemCapability;
use modkit::{Module, ModuleCtx};
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::CredStoreConfig;
use crate::domain::{CredStoreLocalClient, Service};

/// `CredStore` gateway module.
///
/// This module:
/// 1. Registers the `CredStorePluginSpecV1` schema in types-registry
/// 2. Discovers plugin instances via types-registry (lazy, first-use)
/// 3. Routes secret operations through the selected plugin
/// 4. Registers `Arc<dyn CredStoreClientV1>` in `ClientHub` for consumers
#[modkit::module(
    name = "credstore",
    deps = ["types-registry"],
    capabilities = [system]
)]
pub struct CredStoreModule {
    service: OnceLock<Arc<Service>>,
}

impl Default for CredStoreModule {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for CredStoreModule {
    #[tracing::instrument(skip_all, fields(vendor))]
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: CredStoreConfig = ctx.config()?;
        tracing::Span::current().record("vendor", cfg.vendor.as_str());
        info!(vendor = %cfg.vendor);

        // Register plugin schema in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let schema_str = CredStorePluginSpecV1::gts_schema_with_refs_as_string();
        let schema_json: serde_json::Value = serde_json::from_str(&schema_str)?;
        let results = registry.register(vec![schema_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;
        info!(
            schema_id = %CredStorePluginSpecV1::gts_schema_id(),
            "Registered CredStore plugin schema in types-registry"
        );

        // Create domain service
        let hub = ctx.client_hub();
        let svc = Arc::new(Service::new(hub, cfg.vendor));
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Register local client in ClientHub
        let api: Arc<dyn CredStoreClientV1> = Arc::new(CredStoreLocalClient::new(svc));
        ctx.client_hub().register::<dyn CredStoreClientV1>(api);

        Ok(())
    }
}

#[async_trait]
impl SystemCapability for CredStoreModule {}
