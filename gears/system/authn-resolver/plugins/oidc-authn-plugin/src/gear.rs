//! Oidc `AuthN` resolver plugin gear registration.
//!
//! This gear integrates the `OidcAuthNPlugin` with `ToolKit` runtime by:
//! 1. Reading plugin runtime configuration from `server.yaml` (`init`).
//! 2. Declaring its plugin instance, built from the configuration, for GTS
//!    publication (`gts_publication(...)`); the runtime publishes it until admitted, before
//!    `init`.
//! 3. Registering the scoped plugin client in `ClientHub` in `init`, for the
//!    admitted instance.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverPluginSpecV1;
use opentelemetry::global;
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use tracing::{info, warn};
use types_registry_sdk::GtsId;

use crate::config::{INSTANCE_SUFFIX, OidcAuthNGearConfig};
use crate::domain::authenticate::OidcAuthNPlugin;
use crate::domain::metrics::AuthNMetrics;
use crate::infra::idp_http::IdpHttpClient;
use crate::infra::runtime::build_oidc_authn_plugin;

/// `ToolKit` gear responsible for wiring oidc-authn plugin into runtime.
///
/// `AuthN` Resolver publishes the shared plugin schema from its SDK declarations;
/// this gear publishes only its configured instance.
#[toolkit::gear(
    name = "oidc-authn-plugin",
    gts_publication(entities = Self::plugin_instance),
)]
#[derive(Default)]
pub struct OidcAuthNPluginGear {
    plugin: OnceLock<Arc<OidcAuthNPlugin>>,
}

impl OidcAuthNPluginGear {
    /// The resolved configuration of `gears.oidc-authn-plugin.config`.
    fn resolved(
        cfg: anyhow::Result<OidcAuthNGearConfig>,
    ) -> anyhow::Result<crate::config::ResolvedGearConfig> {
        cfg.map_err(|error| {
            anyhow::anyhow!(
                "failed to load oidc-authn-plugin configuration from gears.oidc-authn-plugin.config: {error}"
            )
        })?
        .resolve()
    }

    /// The plugin instance for `vendor` and the configured priority.
    fn registration(vendor: &str, priority: u32) -> anyhow::Result<(GtsId, serde_json::Value)> {
        let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
        let instance = PluginV1::<AuthNResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: vendor.to_owned(),
            priority: normalize_priority(priority),
            properties: AuthNResolverPluginSpecV1,
        };
        Ok((
            GtsId::try_new(instance_id.as_ref())?,
            serde_json::to_value(&instance)?,
        ))
    }

    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg = Self::resolved(inputs.config().map_err(anyhow::Error::from))?;
        Ok(vec![Self::registration(
            &cfg.plugin.vendor,
            cfg.plugin.priority,
        )?])
    }
}

#[async_trait]
impl Gear for OidcAuthNPluginGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let meter = global::meter(Self::MODULE_NAME);
        let metrics = Arc::new(AuthNMetrics::new(&meter));

        let cfg = Self::resolved(ctx.config().map_err(anyhow::Error::from))?;
        let plugin_config = cfg.plugin;

        let http_client = IdpHttpClient::from_settings(
            cfg.request_timeout,
            cfg.custom_ca_certificate_paths.as_slice(),
        )?;

        let plugin = Arc::new(build_oidc_authn_plugin(
            cfg.jwt_validation,
            cfg.issuer_trust,
            plugin_config,
            http_client,
            metrics,
        )?);

        self.plugin
            .set(Arc::clone(&plugin))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The runtime admitted the instance (`plugin_instance`) before `init`:
        // register the scoped plugin client under the same canonical GTS
        // instance-id scope, the one authn-resolver looks up.
        plugin
            .register(&ctx.client_hub())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        info!("oidc AuthN plugin admitted and bound");
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

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
