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

use crate::config::HttpJsonPluginConfig;
use crate::infra::source::{HttpJsonRateProvider, HttpJsonSourceSettings};

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
        //
        // `config_expanded` (not `config`) so a `${VAR}` credential is resolved
        // from the environment here — that is what lets the raw key stay out of
        // the config file. An unresolvable variable lands in the catch-all arm
        // below and aborts init, rather than the source coming up and sending the
        // literal placeholder as its credential.
        let cfg: HttpJsonPluginConfig = match ctx.config_expanded::<HttpJsonPluginConfig>() {
            Ok(cfg) => cfg,
            Err(ConfigError::MissingConfigSection { .. }) => HttpJsonPluginConfig::default(),
            Err(ConfigError::GearNotFound { .. }) => {
                info!("bss-rate-provider-http-json-plugin: not configured; skipping registration");
                return Ok(());
            }
            Err(e) => return Err(e).context("bss-rate-provider-http-json-plugin: invalid config"),
        };

        // Required, source-specific config: a field mapping and an https endpoint.
        // (An auth kind that needs a credential without one is impossible to
        // express — `config::Auth` carries the key in the variant, so serde
        // rejects it at config load; no runtime check belongs here.)
        let mapping = cfg.mapping.clone().ok_or_else(|| {
            anyhow::anyhow!("bss-rate-provider-http-json-plugin: `mapping` is required")
        })?;

        let client = build_source_http_client(&cfg.base_url, cfg.timeout_ms)
            .context("bss-rate-provider-http-json-plugin: build HTTP client")?;
        let metrics: SharedFetchMetrics = Arc::new(OtelFetchMetrics::from_global());
        let source: Arc<dyn RateProviderV1> = Arc::new(HttpJsonRateProvider::new(
            HttpJsonSourceSettings {
                id: cfg.id.clone(),
                base_url: cfg.base_url.clone(),
                auth: cfg.auth.clone(),
                mapping,
            },
            client,
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

#[cfg(test)]
mod init_tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;
    use toolkit::config::ConfigProvider;
    use toolkit::{ClientHub, Gear, GearCtx};
    use uuid::Uuid;

    use super::HttpJsonRateProviderPlugin;

    /// Serves one fixed gear-config JSON tree — the same minimal
    /// `ConfigProvider` shape peer gears use for `init()` tests (e.g.
    /// event-broker's `module_tests.rs`).
    struct StaticConfigProvider {
        root: serde_json::Value,
    }

    impl ConfigProvider for StaticConfigProvider {
        fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
            self.root.get(gear)
        }
    }

    /// `mapping` is required for this plugin: a configured feed with no field
    /// mapping can map no document, so `init()` must abort loudly rather than
    /// register a source that fails on every fetch. The config here is
    /// otherwise valid (an https `base_url`), so the only thing that can fail
    /// is the `mapping` check — and the error must name the field. The auth
    /// half of the old runtime validation is covered at the config-parse level
    /// in `config_tests.rs` (invalid combinations are unrepresentable).
    #[tokio::test]
    async fn init_without_mapping_fails_naming_the_field() {
        let root = serde_json::json!({
            "bss-rate-provider-http-json-plugin": {
                "config": {
                    "id": "bank-x",
                    "vendor": "cf.bss",
                    "priority": 210,
                    "base_url": "https://feed.example/rates",
                }
            }
        });
        let ctx = GearCtx::new(
            HttpJsonRateProviderPlugin::MODULE_NAME,
            Uuid::nil(),
            Arc::new(StaticConfigProvider { root }),
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
        );

        let err = HttpJsonRateProviderPlugin
            .init(&ctx)
            .await
            .expect_err("a present config without `mapping` must abort init");
        assert!(
            err.to_string().contains("mapping"),
            "the error must name the missing field, got: {err}"
        );
    }
}
