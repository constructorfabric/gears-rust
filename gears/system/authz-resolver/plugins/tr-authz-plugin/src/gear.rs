//! TR `AuthZ` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::config::TrAuthZPluginConfig;
use crate::domain::Service;

/// TR-based `AuthZ` resolver plugin gear.
///
/// Resolves tenant hierarchy via `TenantResolverClient` instead of
/// accessing Resource Group directly.
///
/// `init` reconciles through the host-linked database registry's local platform API
/// and binds only after admission; failure leaves nothing bound. The registry's
/// system-gear init runs first without a `deps` edge. Remote support is T41/T42.
#[toolkit::gear(name = "tr-authz-plugin", deps = [tenant_resolver])]
pub struct TrAuthZPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for TrAuthZPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for TrAuthZPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        if self.service.get().is_some() {
            anyhow::bail!("{} gear already initialized", Self::MODULE_NAME);
        }

        let cfg: TrAuthZPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded TR AuthZ plugin configuration"
        );

        // Resolve Tenant Resolver client from ClientHub first — if it's not
        // available we want to fail before leaving a dangling instance in
        // the types-registry.
        let tr: Arc<dyn TenantResolverClient> =
            ctx.client_hub().get::<dyn TenantResolverClient>()?;

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
                "cf.builtin.tr_authz_resolver.plugin.v1",
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

        // Create service with TR dependency
        let service = Arc::new(Service::new(tr));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
