//! RG tenant resolver plugin module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use modkit::Module;
use modkit::client_hub::ClientScope;
use modkit::context::ModuleCtx;
use modkit::gts::BaseModkitPluginV1;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::RgTrPluginConfig;
use crate::domain::Service;

/// RG tenant resolver plugin module.
///
/// Resolves tenant hierarchy via `ResourceGroupReadHierarchy`.
/// Does not seed the tenant RG type — it must be created externally.
#[modkit::module(
    name = "rg-tr-plugin",
    deps = ["types-registry", "resource-group"]
)]
pub struct RgTrPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for RgTrPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for RgTrPlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: RgTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded RG tenant resolver plugin configuration"
        );

        // Resolve RG hierarchy read contract from ClientHub
        let rg: Arc<dyn ResourceGroupReadHierarchy> =
            ctx.client_hub().get::<dyn ResourceGroupReadHierarchy>()?;

        // Generate plugin instance ID
        let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
            "cf.builtin.rg_tenant_resolver.plugin.v1",
        );

        // Register plugin instance in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = BaseModkitPluginV1::<TenantResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: TenantResolverPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Create service
        let service = Arc::new(Service::new(rg));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub
        let api: Arc<dyn TenantResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn TenantResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id);
        Ok(())
    }
}
