//! `BssRateProviderGear` — the core gear.
//!
//! Registers a [`crate::infra::discovery::DiscoveringRateProvider`] (which composes the
//! discovered source plugins) as a `PluginV1<RateProviderPluginSpecV1>` instance
//! in the types-registry plus a scoped `RateProviderV1` client keyed by the GTS
//! instance id. The ledger discovers this instance each rate-sync tick. Absent
//! from the `gears:` config block => inert (the ledger keeps its fail-safe
//! `UnconfiguredRateProviderV1` default, no alarm).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bss_ledger_sdk::{RateProviderPluginSpecV1, RateProviderV1};
use bss_rate_provider_sdk::registration::register_rate_provider_plugin;
use toolkit::Gear;
use toolkit::config::ConfigError;
use toolkit::context::GearCtx;
use tracing::info;

use crate::config::RateProviderConfig;
use crate::infra::discovery::DiscoveringRateProvider;

/// Stable GTS instance segment appended to the composite plugin type id. Must be
/// a single valid GTS segment (`vendor.name.plugin.vN`, 5 dot-components).
const INSTANCE_SEGMENT: &str = "cf.bss.rate_provider_composite.plugin.v1";

/// The core FX rate-provider gear.
#[toolkit::gear(name = "bss-rate-provider", deps = [types_registry])]
pub struct BssRateProviderGear;

impl Default for BssRateProviderGear {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Gear for BssRateProviderGear {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        // Config gate: absent => inert (fail-safe, the ledger uses its default and
        // does not alarm); present-but-empty => defaults; invalid => abort loudly.
        let cfg: RateProviderConfig = match ctx.config::<RateProviderConfig>() {
            Ok(cfg) => cfg,
            Err(ConfigError::MissingConfigSection { .. }) => RateProviderConfig::default(),
            Err(ConfigError::GearNotFound { .. }) => {
                info!("bss-rate-provider: not configured; the ledger keeps its fail-safe default");
                return Ok(());
            }
            Err(e) => return Err(e).context("bss-rate-provider: invalid config"),
        };

        // The composite discovers + composes the source plugins lazily on first
        // fetch (serve phase — all plugins registered), sharing the process hub.
        let provider: Arc<dyn RateProviderV1> = Arc::new(DiscoveringRateProvider::new(
            ctx.client_hub(),
            cfg.source_vendor.clone(),
            cfg.id.clone(),
        ));

        // Publish the composite as a plugin instance the ledger discovers.
        let instance_id = register_rate_provider_plugin::<RateProviderPluginSpecV1>(
            ctx,
            INSTANCE_SEGMENT,
            cfg.vendor.clone(),
            cfg.priority,
            provider,
        )
        .await?;

        info!(
            vendor = %cfg.vendor,
            instance_id = %instance_id,
            "bss-rate-provider: registered composite rate-provider plugin"
        );
        Ok(())
    }
}

#[cfg(test)]
mod gts_id_tests {
    use bss_ledger_sdk::RateProviderPluginSpecV1;
    use bss_rate_provider_sdk::registration::assert_registration_builds_valid_gts_id;

    /// The built composite plugin instance id must parse as a valid GTS id.
    #[test]
    fn instance_id_parses_as_gts() {
        assert_registration_builds_valid_gts_id::<RateProviderPluginSpecV1>(
            super::INSTANCE_SEGMENT,
            "cf.bss",
            100,
        );
    }
}
