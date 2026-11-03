//! TR `AuthZ` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;

use crate::config::TrAuthZPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.tr_authz_resolver.plugin.v1";

/// TR-based `AuthZ` resolver plugin gear.
///
/// Resolves tenant hierarchy via `TenantResolverClient` instead of
/// accessing Resource Group directly.
///
/// The runtime publishes the instance built from the configuration
/// (`entities`) — locally or to a remote registry — and calls `init` only once
/// it is admitted; `init` binds the scoped client to the admitted instance.
#[toolkit::gear(
    name = "tr-authz-plugin",
    deps = [tenant_resolver],
    gts_publication(entities = Self::plugin_instance),
)]
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

impl TrAuthZPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let cfg: TrAuthZPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(id.as_ref().to_owned(), document)])
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

        // Without the tenant resolver the plugin could not evaluate anything:
        // fail before binding a client.
        let tr: Arc<dyn TenantResolverClient> =
            ctx.client_hub().get::<dyn TenantResolverClient>()?;

        // Create service with TR dependency
        let service = Arc::new(Service::new(tr));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        let api: Arc<dyn AuthZResolverPluginClient> = service;
        bind_plugin_instances::<AuthZResolverPluginSpecV1, _>(
            &ctx.client_hub(),
            ctx.gts_publication_manifest()?.documents(),
            &api,
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
