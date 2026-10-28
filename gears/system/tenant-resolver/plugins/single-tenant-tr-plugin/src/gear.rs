//! Single-tenant resolver plugin gear.

use std::sync::Arc;

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

use crate::config::SingleTenantTrPluginConfig;
use crate::domain::Service;

/// Single-tenant resolver plugin gear.
///
/// Zero-configuration plugin for single-tenant deployments.
/// Returns the tenant from security context as the only accessible tenant.
///
/// `init` publishes through local `PlatformTypesRegistryApi` and awaits admission
/// before binding the scoped client. The host must link a database-backed registry;
/// its system-gear init runs first without a `deps` edge. Missing registry or failed
/// publication fails boot without binding. Remote publication remains T41/T42.
#[toolkit::gear(name = "single-tenant-tr-plugin")]
pub struct SingleTenantTrPlugin;

impl Default for SingleTenantTrPlugin {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Gear for SingleTenantTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: SingleTenantTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded single-tenant resolver plugin configuration"
        );

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<TenantResolverPluginSpecV1>::build_registration(
                "cf.builtin.single_tenant_resolver.plugin.v1",
                &cfg.vendor,
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

        // Create service and register scoped client in ClientHub
        let service = Arc::new(Service);
        let api: Arc<dyn TenantResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn TenantResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id, vendor = %cfg.vendor, priority = cfg.priority, "registered");
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
