//! Glue wiring the echo Service to `IdpPluginClient` and publishing the GTS instance — see crate root (lib.rs) for behaviour and prod-safety warnings.

use std::sync::{Arc, OnceLock};

use account_management_sdk::{IdpPluginClient, IdpPluginSpecV1};
use async_trait::async_trait;
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::{info, warn};

use crate::config::StaticIdpPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.static_idp.plugin.v1";

/// Static `IdP` plugin for bootstrap and tenant lifecycle without an external `IdP`.
///
/// The runtime publishes the plugin instance built from the configuration
/// (`entities`) and calls `init` only once it is admitted; `init` binds the
/// scoped client to the admitted instance, before anything can select it.
#[toolkit::gear(
    name = "static-idp-plugin",
    gts(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct StaticIdpPlugin {
    service: OnceLock<Arc<dyn IdpPluginClient>>,
}

impl StaticIdpPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let cfg: StaticIdpPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<IdpPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(id.as_ref().to_owned(), document)])
    }
}

#[async_trait]
impl Gear for StaticIdpPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        warn!(
            "Static IdP plugin is running in echo mode - every provision/deprovision \
             succeeds without contacting a real IdP. Do NOT use this plugin in production."
        );

        let cfg: StaticIdpPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );
        let service: Arc<dyn IdpPluginClient> = Arc::new(Service::new());
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        bind_plugin_instances::<IdpPluginSpecV1, _>(
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
