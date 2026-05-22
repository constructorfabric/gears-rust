//! Oidc `AuthN` resolver plugin module registration.
//!
//! This module integrates the `OidcAuthNPlugin` with `ModKit` runtime by:
//! 1. Reading plugin runtime configuration from `server.yaml`.
//! 2. Registering a plugin instance in types-registry (GTS metadata).
//! 3. Registering the scoped plugin client in `ClientHub`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverPluginSpecV1;
use modkit::Module;
use modkit::context::ModuleCtx;
use modkit::gts::PluginV1;
use opentelemetry::global;
use tracing::{info, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::{INSTANCE_SUFFIX, OidcAuthNModuleConfig};
use crate::domain::metrics::AuthNMetrics;
use crate::infra::runtime::build_oidc_authn_plugin;

/// `ModKit` module responsible for wiring oidc-authn plugin into runtime.
///
/// `authn-resolver` dependency is required so plugin schema is already registered
/// in types-registry before this module registers plugin instance metadata.
#[modkit::module(
    name = "oidc-authn-plugin",
    deps = ["types-registry", "authn-resolver"]
)]
#[derive(Default)]
pub struct OidcAuthNPluginModule;

#[async_trait]
impl Module for OidcAuthNPluginModule {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let meter = global::meter(Self::MODULE_NAME);
        let metrics = Arc::new(AuthNMetrics::new(&meter));

        let cfg: OidcAuthNModuleConfig = ctx.config().map_err(|error| {
            anyhow::anyhow!(
                "failed to load oidc-authn-plugin configuration from modules.oidc-authn-plugin.config: {error}"
            )
        })?;
        let cfg = cfg.resolve()?;
        let plugin_config = cfg.plugin;
        let vendor = plugin_config.vendor.clone();
        let gts_priority = normalize_priority(plugin_config.priority);

        let http_client = build_http_client(
            cfg.request_timeout,
            cfg.custom_ca_certificate_paths.as_slice(),
        )?;

        let issuer_trust = cfg.issuer_trust;

        let plugin = Arc::new(build_oidc_authn_plugin(
            cfg.jwt_validation,
            issuer_trust,
            plugin_config,
            http_client,
            metrics,
        ));

        // GTS types-registry registration for metadata discovery.
        // Performed before ClientHub registration so that a GTS failure (the
        // more likely error path — network I/O, serialization) does not leave
        // partially-applied side effects in the in-memory ClientHub.
        let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
        let instance = PluginV1::<AuthNResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: vendor.clone(),
            priority: gts_priority,
            properties: AuthNResolverPluginSpecV1,
        };

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let instance_json = serde_json::to_value(&instance)?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Register plugin in ClientHub using the canonical GTS instance-id
        // scope consumed by authn-resolver runtime lookup. Placed after GTS
        // registration to avoid orphaned hub entries if GTS fails.
        plugin
            .register(&ctx.client_hub())
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        info!(
            instance_id = %instance_id,
            vendor = %vendor,
            priority = gts_priority,
        );
        Ok(())
    }
}

/// Convert YAML `u32` priority into GTS `i16` priority with safe clamping.
fn normalize_priority(priority: u32) -> i16 {
    if let Ok(value) = i16::try_from(priority) {
        value
    } else {
        warn!(
            configured_priority = priority,
            clamped_priority = i16::MAX,
            "oidc plugin priority exceeds i16 range; clamping"
        );
        i16::MAX
    }
}

fn build_http_client(
    request_timeout: Duration,
    custom_ca_certificate_paths: &[String],
) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(request_timeout);
    let mut custom_ca_certificates = Vec::new();

    for path in custom_ca_certificate_paths {
        let pem_bundle = std::fs::read(path).map_err(|error| {
            anyhow::anyhow!("failed to read custom CA certificate bundle {path:?}: {error}")
        })?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem_bundle).map_err(|error| {
            anyhow::anyhow!("failed to parse custom CA certificate bundle {path:?}: {error}")
        })?;
        if certificates.is_empty() {
            anyhow::bail!("custom CA certificate bundle {path:?} did not contain any certificates");
        }
        custom_ca_certificates.extend(certificates);
    }

    if !custom_ca_certificates.is_empty() {
        builder = builder.tls_certs_merge(custom_ca_certificates);
    }

    builder
        .build()
        .map_err(|error| anyhow::anyhow!("failed to build HTTP client: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_priority_passes_through_small_values() {
        assert_eq!(normalize_priority(0), 0);
        assert_eq!(normalize_priority(100), 100);
    }

    #[test]
    fn normalize_priority_passes_through_i16_max_boundary() {
        assert_eq!(normalize_priority(i16::MAX as u32), i16::MAX);
    }

    #[test]
    fn normalize_priority_clamps_values_exceeding_i16_max() {
        assert_eq!(normalize_priority(i16::MAX as u32 + 1), i16::MAX);
        assert_eq!(normalize_priority(u32::MAX), i16::MAX);
    }

    #[test]
    fn build_http_client_reports_missing_custom_ca_bundle() {
        let missing_bundle_path = std::env::temp_dir()
            .join(format!(
                "oidc-authn-plugin-missing-ca-dir-{}",
                std::process::id()
            ))
            .join("custom-root-ca.pem")
            .to_string_lossy()
            .into_owned();
        let error = build_http_client(Duration::from_secs(5), &[missing_bundle_path])
            .expect_err("missing custom CA bundle should fail client construction");

        assert!(
            error
                .to_string()
                .contains("failed to read custom CA certificate bundle"),
            "error should identify missing custom CA bundle: {error}"
        );
    }
}
