//! RG tenant resolver plugin gear.

use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::config::RgTrPluginConfig;
use crate::domain::Service;

/// RG tenant resolver plugin gear.
///
/// Resolves tenant hierarchy via `ResourceGroupReadHierarchy`.
/// Does not seed the tenant RG type — it must be created externally.
///
/// `init` publishes through local `PlatformTypesRegistryApi` and awaits admission
/// before binding the scoped client. The host must link a database-backed registry;
/// its system-gear init runs first without a `deps` edge. Missing registry or failed
/// publication fails boot without binding. Remote publication remains T41/T42.
#[toolkit::gear(name = "rg-tr-plugin", deps = [resource_group])]
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
            "Loaded RG tenant resolver plugin configuration"
        );

        // Resolve RG hierarchy read contract from ClientHub
        let rg: Arc<dyn ResourceGroupReadHierarchy> =
            ctx.client_hub().get::<dyn ResourceGroupReadHierarchy>()?;

        // Generate plugin instance ID
        let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
            "cf.builtin.rg_tenant_resolver.plugin.v1",
        );

        let instance = PluginV1::<TenantResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: cfg.vendor.clone(),
            priority: cfg.priority,
            properties: TenantResolverPluginSpecV1,
        };
        let instance_json = serde_json::to_value(&instance)?;

        // Publish to types-registry; the backend is registered only once the instance is admitted.
        let registry = ctx
            .client_hub()
            .get::<dyn PlatformTypesRegistryApi>()
            .with_context(|| {
                format!(
                    "{}: no PlatformTypesRegistryApi to publish to; a database-backed \
                     types-registry must run in this process",
                    Self::MODULE_NAME
                )
            })?;
        let publisher = PublisherContext {
            name: Self::MODULE_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").parse()?,
        };
        let gts_id = GtsId::try_new(instance_id.as_ref())?;
        publish_startup_entities(
            registry.as_ref(),
            &publisher,
            &[(gts_id, instance_json)],
            ctx.cancellation_token(),
        )
        .await?;

        // Create service
        let service = Arc::new(Service::new(rg));
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
