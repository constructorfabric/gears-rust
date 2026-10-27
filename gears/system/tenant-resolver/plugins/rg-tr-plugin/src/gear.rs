//! RG tenant resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::RgTrPluginConfig;
use crate::domain::Service;

/// RG tenant resolver plugin gear.
///
/// Resolves tenant hierarchy via `ResourceGroupReadHierarchy`.
/// Does not seed the tenant RG type — it must be created externally.
#[toolkit::gear(
    name = "rg-tr-plugin",
    deps = [types_registry, resource_group]
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
impl Gear for RgTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: RgTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            tenant_types = ?cfg.tenant_types,
            "Loaded RG tenant resolver plugin configuration"
        );

        if cfg.tenant_types.is_empty() {
            anyhow::bail!("{}: tenant_types must not be empty", Self::MODULE_NAME);
        }

        // Resolve RG hierarchy read contract from ClientHub
        let rg: Arc<dyn ResourceGroupReadHierarchy> =
            ctx.client_hub().get::<dyn ResourceGroupReadHierarchy>()?;

        // Generate plugin instance ID
        let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
            "cf.builtin.rg_tenant_resolver.plugin.v1",
        );

        // Register plugin instance in types-registry
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance = PluginV1::<TenantResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: TenantResolverPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Create service
        let service = Arc::new(Service::new(rg, cfg.tenant_types));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

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
