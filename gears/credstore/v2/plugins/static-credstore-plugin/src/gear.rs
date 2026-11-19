// Updated: 2026-10-06 by Constructor Tech
//! `ToolKit` gear registration for the static credential backend.
//!
//! The runtime publishes the GTS plugin instance built from the configuration
//! before `init`; `init` loads and validates configuration and binds a scoped
//! `CredStorePluginClientV2` to the admitted instance through `ClientHub`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV2, CredStorePluginSpecV2};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::{info, warn};
use types_registry_sdk::GtsId;

use crate::config::StaticCredStorePluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.core._.static_credstore.v1";

/// Static credstore plugin gear.
///
/// Serves pre-configured secrets from YAML configuration for development and testing.
#[toolkit::gear(
    name = "static-credstore-plugin-v2",
    gts_publication(entities = Self::plugin_instance),
)]
pub struct StaticCredStorePlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticCredStorePlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl StaticCredStorePlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: StaticCredStorePluginConfig = inputs.config_expanded_or_default()?;
        let (id, document) = PluginV1::<CredStorePluginSpecV2>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }
}

#[async_trait]
impl Gear for StaticCredStorePlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load configuration
        let cfg: StaticCredStorePluginConfig = ctx.config_expanded_or_default()?;

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );

        warn!(
            "static credstore plugin is a non-durable in-memory value store for development \
             and tests only: stored values do not survive a restart; do not use it in production"
        );

        // Create service from config (validate early, before binding).
        let service = Arc::new(Service::from_config(&cfg)?);

        // All fallible steps done: commit service to shared state.
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        let api: Arc<dyn CredStorePluginClientV2> = service;
        bind_plugin_instances::<CredStorePluginSpecV2, _>(
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
