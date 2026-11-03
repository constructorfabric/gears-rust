//! RG tenant resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;

use crate::config::RgTrPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.rg_tenant_resolver.plugin.v1";

/// RG tenant resolver plugin gear.
///
/// Resolves tenant hierarchy via `ResourceGroupReadHierarchy`.
/// Does not seed the tenant RG type — it must be created externally.
///
/// The runtime publishes the instance built from the configuration
/// (`entities`) — locally or to a remote registry, independently of Resource
/// Group's state — and calls `init` only once it is admitted; `init` binds the
/// scoped client to the admitted instance.
#[toolkit::gear(
    name = "rg-tr-plugin",
    deps = [resource_group],
    gts_publication(entities = Self::plugin_instance),
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

impl RgTrPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let cfg: RgTrPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<TenantResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(id.as_ref().to_owned(), document)])
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

        cfg.validate()?;

        // Resolve RG hierarchy read contract from ClientHub
        let rg: Arc<dyn ResourceGroupReadHierarchy> =
            ctx.client_hub().get::<dyn ResourceGroupReadHierarchy>()?;

        // Create service
        let service = Arc::new(Service::new(rg, cfg.tenant_types));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        let api: Arc<dyn TenantResolverPluginClient> = service;
        bind_plugin_instances::<TenantResolverPluginSpecV1, _>(
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
