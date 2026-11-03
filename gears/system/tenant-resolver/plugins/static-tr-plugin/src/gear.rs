//! Static tenant resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;

use crate::config::StaticTrPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.static_tenant_resolver.plugin.v1";

/// Static tenant resolver plugin gear: tenant data from configuration, with
/// hierarchical support.
///
/// The runtime publishes the plugin instance built from the configuration
/// (`entities`) and calls `init` only once it is admitted; `init` binds the
/// scoped client to the admitted instance, before anything can select it.
#[toolkit::gear(
    name = "static-tr-plugin",
    gts_publication(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct StaticTrPlugin {
    service: OnceLock<Arc<dyn TenantResolverPluginClient>>,
}

impl StaticTrPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let cfg: StaticTrPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<TenantResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(id.as_ref().to_owned(), document)])
    }
}

#[async_trait]
impl Gear for StaticTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: StaticTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            tenant_count = cfg.tenants.len(),
            "Loaded plugin configuration"
        );
        let service: Arc<dyn TenantResolverPluginClient> = Arc::new(Service::from_config(&cfg)?);
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        bind_plugin_instances::<TenantResolverPluginSpecV1, _>(
            &ctx.client_hub(),
            ctx.gts_publication_manifest()?.documents(),
            &service,
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
