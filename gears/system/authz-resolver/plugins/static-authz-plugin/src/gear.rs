//! Static `AuthZ` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;
use types_registry_sdk::GtsId;

use crate::config::StaticAuthZPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.static_authz_resolver.plugin.v1";

/// Static `AuthZ` resolver plugin gear.
///
/// The runtime publishes the plugin instance built from the configuration
/// (`entities`) and calls `init` only once it is admitted; `init` binds the
/// scoped client to the admitted instance, before anything can select it.
#[toolkit::gear(
    name = "static-authz-plugin",
    gts_publication(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct StaticAuthZPlugin {
    service: OnceLock<Arc<dyn AuthZResolverPluginClient>>,
}

impl StaticAuthZPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: StaticAuthZPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }
}

#[async_trait]
impl Gear for StaticAuthZPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: StaticAuthZPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );
        let service: Arc<dyn AuthZResolverPluginClient> = Arc::new(Service::new());
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        bind_plugin_instances::<AuthZResolverPluginSpecV1, _>(
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
