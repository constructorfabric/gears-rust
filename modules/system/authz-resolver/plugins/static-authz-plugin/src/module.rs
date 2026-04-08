//! Static `AuthZ` resolver plugin module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::StaticAuthZPluginConfig;
use crate::domain::Service;

/// Static `AuthZ` resolver plugin module.
#[modkit::module(
    name = "static-authz-plugin",
    deps = ["types-registry"]
)]
pub struct StaticAuthZPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticAuthZPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for StaticAuthZPlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: StaticAuthZPluginConfig = ctx.config()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );

        // Generate plugin instance ID
        let instance_id = AuthZResolverPluginSpecV1::gts_make_instance_id(
            "hyperspot.builtin.static_authz_resolver.plugin.v1",
        );

        // Register plugin instance in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<AuthZResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: AuthZResolverPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Create service
        let service = Arc::new(Service::new());
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub
        let api: Arc<dyn AuthZResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn AuthZResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id);
        Ok(())
    }
}
