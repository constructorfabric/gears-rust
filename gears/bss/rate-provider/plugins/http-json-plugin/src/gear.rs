//! http-json source-plugin gear: build the source, register a `PluginV1`
//! instance in the types-registry, and register the scoped `RateProviderV1`
//! client keyed by the GTS instance id.

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

use crate::config::{AuthKind, HttpJsonPluginConfig};
use crate::source::HttpJsonRateProvider;

/// Stable GTS instance segment appended to the source-plugin type id. Must be a
/// single valid GTS segment (`vendor.name.plugin.vN`, 5 dot-components).
const INSTANCE_SEGMENT: &str = "cf.bss.rate_provider_http_json.plugin.v1";

/// The http-json source-plugin gear.
#[toolkit::gear(name = "bss-rate-provider-http-json-plugin", deps = [types_registry])]
pub struct HttpJsonRateProviderPlugin;

impl Default for HttpJsonRateProviderPlugin {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Gear for HttpJsonRateProviderPlugin {
    async fn init(&self, ctx: &GearCtx) -> Result<()> {
        // Config gate: absent => inert; present-but-empty => defaults (which lack
        // a usable base_url/mapping and are rejected below); invalid => abort.
        let cfg: HttpJsonPluginConfig = match ctx.config::<HttpJsonPluginConfig>() {
            Ok(cfg) => cfg,
            Err(ConfigError::MissingConfigSection { .. }) => HttpJsonPluginConfig::default(),
            Err(ConfigError::GearNotFound { .. }) => {
                info!("bss-rate-provider-http-json-plugin: not configured; skipping registration");
                return Ok(());
            }
            Err(e) => return Err(e).context("bss-rate-provider-http-json-plugin: invalid config"),
        };

        // Required, source-specific config: a field mapping and an https endpoint.
        let mapping = cfg.mapping.clone().ok_or_else(|| {
            anyhow::anyhow!("bss-rate-provider-http-json-plugin: `mapping` is required")
        })?;
        // A configured auth mode with no key is a local misconfiguration (e.g. a
        // `${VAR}`/CredStore expansion that silently resolved empty) — without this
        // check, `source.rs` would silently send every request unauthenticated
        // instead of failing loud here.
        if cfg.auth != AuthKind::None && cfg.api_key.is_none() {
            anyhow::bail!(
                "bss-rate-provider-http-json-plugin: `api_key` is required when `auth` is not `none`"
            );
        }

        let client = build_source_http_client(&cfg.base_url, cfg.timeout_ms)
            .context("bss-rate-provider-http-json-plugin: build HTTP client")?;
        let metrics: SharedFetchMetrics = Arc::new(OtelFetchMetrics::from_global());
        let source: Arc<dyn RateProviderV1> = Arc::new(HttpJsonRateProvider::new(
            cfg.id.clone(),
            cfg.base_url.clone(),
            client,
            cfg.api_key.clone(),
            cfg.auth,
            mapping,
            metrics,
        ));

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
            "bss-rate-provider-http-json-plugin: registered http-json source plugin"
        );
        Ok(())
    }
}

#[cfg(test)]
mod gts_id_tests {
    use bss_rate_provider_sdk::RateProviderSourcePluginSpecV1;
    use bss_rate_provider_sdk::registration::assert_registration_builds_valid_gts_id;

    /// The built plugin instance id must parse as a valid GTS id.
    #[test]
    fn instance_id_parses_as_gts() {
        assert_registration_builds_valid_gts_id::<RateProviderSourcePluginSpecV1>(
            super::INSTANCE_SEGMENT,
            "cf.bss",
            200,
        );
    }
}
