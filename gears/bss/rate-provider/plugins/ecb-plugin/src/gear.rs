//! ECB source-plugin gear: build the source, register a `PluginV1` instance in
//! the types-registry, and register the scoped `RateProviderV1` client keyed by
//! the GTS instance id (the pattern the core `bss-rate-provider` gear discovers).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bss_ledger_sdk::RateProviderV1;
use bss_rate_provider_sdk::RateProviderSourcePluginSpecV1;
use bss_rate_provider_sdk::http_client::build_source_http_client;
use bss_rate_provider_sdk::metrics::{OtelFetchMetrics, SharedFetchMetrics};
use bss_rate_provider_sdk::registration::register_rate_provider_plugin;
use toolkit::Gear;
use toolkit::config::ConfigError;
use toolkit::context::GearCtx;
use tracing::info;

use crate::config::EcbPluginConfig;
use crate::infra::source::EcbRateProvider;

/// Stable GTS instance segment appended to the source-plugin type id. Must be a
/// single valid GTS segment (`vendor.name.plugin.vN`, 5 dot-components) — a
/// 6-component id fails `GtsId` parsing and would be rejected by the registry.
const INSTANCE_SEGMENT: &str = "cf.bss.rate_provider_ecb.plugin.v1";

/// The ECB source-plugin gear.
#[toolkit::gear(name = "bss-rate-provider-ecb-plugin", deps = [types_registry])]
pub struct EcbRateProviderPlugin;

impl Default for EcbRateProviderPlugin {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Gear for EcbRateProviderPlugin {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        // Config gate (bss subsystem convention): absent from `gears:` => inert;
        // present-but-empty => defaults; present-but-invalid => abort loudly.
        let cfg: EcbPluginConfig = match ctx.config::<EcbPluginConfig>() {
            Ok(cfg) => cfg,
            Err(ConfigError::MissingConfigSection { .. }) => EcbPluginConfig::default(),
            Err(ConfigError::GearNotFound { .. }) => {
                info!("bss-rate-provider-ecb-plugin: not configured; skipping registration");
                return Ok(());
            }
            Err(e) => return Err(e).context("bss-rate-provider-ecb-plugin: invalid config"),
        };

        let client = build_source_http_client(&cfg.base_url, cfg.timeout_ms)
            .context("bss-rate-provider-ecb-plugin: build HTTP client")?;
        let metrics: SharedFetchMetrics = Arc::new(OtelFetchMetrics::from_global());
        let source: Arc<dyn RateProviderV1> = Arc::new(EcbRateProvider::new(
            cfg.id.clone(),
            cfg.base_url.clone(),
            client,
            metrics,
        ));

        // Publish the plugin instance (priority = fallback order) to types-registry
        // and register the scoped client the core gear resolves via the instance id.
        let instance_id = register_rate_provider_plugin::<RateProviderSourcePluginSpecV1>(
            ctx,
            INSTANCE_SEGMENT,
            cfg.vendor.clone(),
            cfg.priority,
            source,
        )
        .await?;

        info!(
            provider = %cfg.id,
            instance_id = %instance_id,
            priority = cfg.priority,
            "bss-rate-provider-ecb-plugin: registered ECB source plugin"
        );
        Ok(())
    }
}

#[cfg(test)]
mod gts_id_tests {
    use bss_rate_provider_sdk::RateProviderSourcePluginSpecV1;
    use bss_rate_provider_sdk::registration::assert_registration_builds_valid_gts_id;

    /// The built plugin instance id must parse as a valid GTS id — a segment with
    /// too many dot-components fails `GtsId` parsing and the registry rejects it.
    #[test]
    fn instance_id_parses_as_gts() {
        assert_registration_builds_valid_gts_id::<RateProviderSourcePluginSpecV1>(
            super::INSTANCE_SEGMENT,
            "cf.bss",
            100,
        );
    }
}
