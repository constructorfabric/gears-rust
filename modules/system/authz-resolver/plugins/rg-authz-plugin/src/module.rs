//! RG `AuthZ` resolver plugin module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::RgAuthZPluginConfig;
use crate::domain::Service;

/// RG `AuthZ` resolver plugin module.
#[modkit::module(
    name = "rg-authz-plugin",
    deps = ["types-registry", "resource-group"]
)]
pub struct RgAuthZPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for RgAuthZPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for RgAuthZPlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: RgAuthZPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded RG AuthZ plugin configuration"
        );

        // Generate plugin instance ID
        let instance_id = AuthZResolverPluginSpecV1::gts_make_instance_id(
            "hyperspot.builtin.rg_authz_resolver.plugin.v1",
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

        // Resolve RG hierarchy read contract from ClientHub
        let rg: Arc<dyn ResourceGroupReadHierarchy> =
            ctx.client_hub().get::<dyn ResourceGroupReadHierarchy>()?;

        // Create service with RG dependency
        let service = Arc::new(Service::new(rg));
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
