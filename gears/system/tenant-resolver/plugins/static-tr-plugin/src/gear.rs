//! Static tenant resolver plugin gear.

use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::config::StaticTrPluginConfig;
use crate::domain::Service;

/// Static tenant resolver plugin gear.
///
/// Provides tenant data from configuration with hierarchical support.
///
/// **Plugin registration pattern:**
/// - Gateway registers the plugin schema (GTS type definition)
/// - This plugin registers its instance (implementation metadata)
/// - This plugin registers its scoped client (implementation in `ClientHub`)
///
/// `init` publishes through local `PlatformTypesRegistryApi` and awaits admission
/// before binding the scoped client. The host must link a database-backed registry;
/// its system-gear init runs first without a `deps` edge. Missing registry or failed
/// publication fails boot without binding. Remote publication remains T41/T42.
#[toolkit::gear(name = "static-tr-plugin")]
pub struct StaticTrPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticTrPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for StaticTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        if self.service.get().is_some() {
            anyhow::bail!("{} gear already initialized", Self::MODULE_NAME);
        }

        // Load configuration
        let cfg: StaticTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            tenant_count = cfg.tenants.len(),
            "Loaded plugin configuration"
        );

        let service = Arc::new(Service::from_config(&cfg)?);

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<TenantResolverPluginSpecV1>::build_registration(
                "cf.builtin.static_tenant_resolver.plugin.v1",
                cfg.vendor.clone(),
                cfg.priority,
            )?;

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
