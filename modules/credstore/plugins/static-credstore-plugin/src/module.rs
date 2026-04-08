use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV1, CredStorePluginSpecV1};
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::StaticCredStorePluginConfig;
use crate::domain::Service;

/// Static credstore plugin module.
///
/// Serves pre-configured secrets from YAML configuration for development and testing.
#[modkit::module(
    name = "static-credstore-plugin",
    deps = ["types-registry"]
)]
pub struct StaticCredStorePlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticCredStorePlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for StaticCredStorePlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        // Load configuration
        let cfg: StaticCredStorePluginConfig = ctx.config_expanded()?;

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            secret_count = cfg.secrets.len(),
            "Loaded plugin configuration"
        );

        // Generate plugin instance ID
        let instance_id =
            CredStorePluginSpecV1::gts_make_instance_id("x.core._.static_credstore.v1");

        // Create service from config (validate early, before registration)
        let service = Arc::new(Service::from_config(&cfg)?);

        // Register plugin instance in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<CredStorePluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: CredStorePluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // All fallible steps done — commit service to shared state
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub
        let api: Arc<dyn CredStorePluginClientV1> = service;
        ctx.client_hub()
            .register_scoped::<dyn CredStorePluginClientV1>(ClientScope::gts_id(&instance_id), api);

        info!(instance_id = %instance_id);
        Ok(())
    }
}
