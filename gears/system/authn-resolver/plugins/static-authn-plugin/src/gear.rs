//! Static `AuthN` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;

use crate::config::StaticAuthNPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.static_authn_resolver.plugin.v1";

/// Static token-to-identity `AuthN` plugin.
///
/// The runtime publishes the instance built from the configuration
/// (`entities`) and calls `init` only once it is admitted; `init` binds the
/// scoped client to the admitted instance.
#[toolkit::gear(
    name = "static-authn-plugin",
    gts(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct StaticAuthNPlugin {
    service: OnceLock<Arc<dyn AuthNResolverPluginClient>>,
}

impl StaticAuthNPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let cfg: StaticAuthNPluginConfig = inputs.config_expanded_or_default()?;
        let (id, document) = PluginV1::<AuthNResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(id.as_ref().to_owned(), document)])
    }
}

#[async_trait]
impl Gear for StaticAuthNPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load configuration
        let cfg: StaticAuthNPluginConfig = ctx.config_expanded_or_default()?;
        if matches!(cfg.mode, crate::config::AuthNMode::AcceptAll) {
            tracing::warn!(
                "Static AuthN plugin is running in `accept_all` mode - \
                 all bearer tokens will be accepted with a hardcoded identity. \
                 Do NOT use this mode in production."
            );
        }

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            mode = ?cfg.mode,
            token_count = cfg.tokens.len(),
            "Loaded plugin configuration"
        );

        let service: Arc<dyn AuthNResolverPluginClient> = Arc::new(Service::from_config(&cfg));
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        bind_plugin_instances::<AuthNResolverPluginSpecV1, _>(
            &ctx.client_hub(),
            ctx.gts_publication_manifest()?.documents(),
            &service,
        )
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
