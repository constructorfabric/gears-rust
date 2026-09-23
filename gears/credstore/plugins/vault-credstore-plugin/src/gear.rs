//! `ToolKit` gear registration for the Vault / `OpenBao` credential backend.
//!
//! Loads and validates configuration, registers its GTS plugin instance, and
//! publishes a scoped `CredStorePluginClientV1` through `ClientHub`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV1, CredStorePluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::VaultCredStorePluginConfig;
use crate::domain::Service;

/// Vault / `OpenBao` credstore plugin gear.
///
/// Stores secret bytes in a Vault / `OpenBao` KV v2 secrets engine over HTTP.
/// Prototype-quality: see the crate README for scope and caveats.
#[toolkit::gear(
    name = "vault-credstore-plugin",
    deps = [types_registry]
)]
pub struct VaultCredStorePlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for VaultCredStorePlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for VaultCredStorePlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load configuration (`${VAR}` placeholders in `token` are expanded
        // from the process environment here).
        let cfg: VaultCredStorePluginConfig = ctx.config_expanded_or_default()?;

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            address = %cfg.address,
            mount = %cfg.mount,
            path_prefix = %cfg.path_prefix,
            "Loaded plugin configuration"
        );

        // Create service from config (validate early, before registration).
        let service = Arc::new(Service::from_config(&cfg)?);

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) = PluginV1::<CredStorePluginSpecV1>::build_registration(
            "cf.core._.vault_credstore.v1",
            cfg.vendor.clone(),
            cfg.priority,
        )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // All fallible steps done — commit service to shared state
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub
        let api: Arc<dyn CredStorePluginClientV1> = service;
        ctx.client_hub()
            .register_scoped::<dyn CredStorePluginClientV1>(ClientScope::gts_id(&instance_id), api);

        info!(instance_id = %instance_id);
        Ok(())
    }
}
