//! Oidc `AuthN` resolver plugin gear registration.
//!
//! This gear integrates the `OidcAuthNPlugin` with `ToolKit` runtime by:
//! 1. Reading plugin runtime configuration from `server.yaml` (`init`).
//! 2. Declaring its plugin instance, built from the configuration, for GTS
//!    publication (`gts(...)`); the runtime publishes it until admitted, before
//!    `init`.
//! 3. Registering the scoped plugin client in `ClientHub` in `init`, for the
//!    admitted instance.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverPluginSpecV1;
use opentelemetry::global;
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::contracts::SystemCapability;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use tracing::{info, warn};

use crate::config::{INSTANCE_SUFFIX, OidcAuthNGearConfig};
use crate::domain::authenticate::OidcAuthNPlugin;
use crate::domain::metrics::AuthNMetrics;
use crate::infra::runtime::build_oidc_authn_plugin;

/// `ToolKit` gear responsible for wiring oidc-authn plugin into runtime.
///
/// The plugin schema reaches types-registry through the `toolkit-gts` inventory the
/// registry seeds from; this gear publishes only its instance.
#[toolkit::gear(
    name = "oidc-authn-plugin",
    capabilities = [system],
    gts(entities = Self::plugin_instance),
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
    fn registration(vendor: &str, priority: u32) -> anyhow::Result<(String, serde_json::Value)> {
        let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
        let instance = PluginV1::<AuthNResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: vendor.to_owned(),
            priority: normalize_priority(priority),
            properties: AuthNResolverPluginSpecV1,
        };
        Ok((
            instance_id.as_ref().to_owned(),
            serde_json::to_value(&instance)?,
        ))
    }

    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
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

        let http_client = build_http_client(
            cfg.request_timeout,
            cfg.custom_ca_certificate_paths.as_slice(),
        )?;

        let plugin = Arc::new(build_oidc_authn_plugin(
            cfg.jwt_validation,
            cfg.issuer_trust,
            plugin_config,
            http_client,
            metrics,
        ));

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

// System priority only: no `pre_init` work.
#[async_trait]
impl SystemCapability for OidcAuthNPluginGear {}

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

    /// The publication boundary: the runtime publishes the configured instance
    /// before `init`; `init` binds the scoped client to the admitted manifest.
    mod publication {
        use std::sync::Arc;

        use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
        use serde_json::{Value, json};
        use tokio_util::sync::CancellationToken;
        use toolkit::client_hub::{ClientHub, ClientScope};
        use toolkit::contracts::Gear;
        use toolkit::gts_publication::{
            GtsManifest, GtsPublicationCapability as _, GtsPublicationError, PreparedGtsPublication,
        };
        use toolkit::{ConfigProvider, GearCtx};
        use types_registry_sdk::AdmissionFailureReason;
        use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};

        use crate::config::INSTANCE_SUFFIX;
        use crate::gear::OidcAuthNPluginGear;

        struct FixedConfig(Value);

        impl ConfigProvider for FixedConfig {
            fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
                Some(&self.0)
            }
        }

        fn config() -> FixedConfig {
            FixedConfig(json!({ "config": {
                "vendor": "cf",
                "priority": 5,
                "jwt": {
                    "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                    "claim_mapping": { "subject_tenant_id": "tenant_id" },
                },
                "s2s_oauth": {
                    "discovery_url": "https://oidc/realms/platform",
                    "default_subject_type": toolkit_gts::gts_id!("cf.core.security.subject_user.v1~"),
                },
            } }))
        }

        fn instance_id() -> String {
            AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX)
                .as_ref()
                .to_owned()
        }

        fn bound(hub: &ClientHub) -> bool {
            hub.try_get_scoped::<dyn AuthNResolverPluginClient>(
                &ClientScope::gts_id(&instance_id()),
            )
            .is_some()
        }

        fn manifest() -> GtsManifest {
            OidcAuthNPluginGear::default()
                .descriptor()
                .build_gts_manifest(&config())
                .unwrap()
        }

        /// What the runtime does before `init`: publish the configured manifest.
        async fn published(
            registry: Option<&Arc<MockTypesRegistry>>,
            cancel: &CancellationToken,
        ) -> Result<(), GtsPublicationError> {
            let hub = ClientHub::new();
            if let Some(registry) = registry {
                registry.install(&hub);
            }
            PreparedGtsPublication::prepare(
                OidcAuthNPluginGear::default().descriptor(),
                &config(),
                &hub,
            )?
            .publish(cancel, Arc::new(|_: &str| {}))
            .await
        }

        fn ctx() -> GearCtx {
            GearCtx::new(
                "oidc-authn-plugin",
                uuid::Uuid::new_v4(),
                Arc::new(config()),
                Arc::new(ClientHub::new()),
                CancellationToken::new(),
            )
        }

        #[tokio::test]
        async fn init_after_admission_binds_the_instance() {
            let registry = Arc::new(MockTypesRegistry::new());
            published(Some(&registry), &CancellationToken::new())
                .await
                .expect("admitted");
            let content = registry.content(&instance_id()).expect("admitted");
            assert_eq!(
                (&content["vendor"], &content["priority"]),
                (&json!("cf"), &json!(5))
            );
            let ctx = ctx();

            OidcAuthNPluginGear::default()
                .init(&ctx)
                .await
                .expect("init");

            assert!(bound(&ctx.client_hub()));
        }

        /// The scope `init` registers the client under is the instance ID the
        /// manifest publishes: what authn-resolver selects from the registry
        /// resolves to this client.
        #[tokio::test]
        async fn the_registered_scope_is_the_published_instance() {
            let ctx = ctx();

            OidcAuthNPluginGear::default()
                .init(&ctx)
                .await
                .expect("init");

            let manifest = manifest();
            let [(published, _)] = manifest.documents() else {
                panic!("one instance: {manifest:?}");
            };
            assert!(
                ctx.client_hub()
                    .try_get_scoped::<dyn AuthNResolverPluginClient>(&ClientScope::gts_id(
                        published
                    ))
                    .is_some(),
                "bound under {published}"
            );
        }

        /// A terminal refusal fails the publication: `init` never runs.
        #[tokio::test]
        async fn a_refused_instance_fails_the_publication() {
            let registry = Arc::new(MockTypesRegistry::new());
            registry.reject(
                &instance_id(),
                AdmissionFailureReason::IncompatibleWithBaseline,
            );

            let err = published(Some(&registry), &CancellationToken::new())
                .await
                .unwrap_err();

            assert!(matches!(err, GtsPublicationError::Refused { .. }), "{err}");
            assert!(err.to_string().contains("publishing instance"), "{err}");
        }

        /// An admission that never completes keeps the publication pending past the
        /// startup deadline, retrying, rather than failing; cancellation ends it.
        #[tokio::test(start_paused = true)]
        async fn a_pending_instance_keeps_retrying_until_cancelled() {
            let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
            let cancel = CancellationToken::new();

            let running = published(Some(&registry), &cancel);
            tokio::pin!(running);
            tokio::select! {
                _ = &mut running => panic!("never admitted"),
                () = tokio::time::sleep(types_registry_sdk::STARTUP_PUBLICATION_DEADLINE * 3) => {}
            }

            assert!(registry.calls(Call::Register) > 1, "retried");
            cancel.cancel();
            assert!(matches!(
                running.await,
                Err(GtsPublicationError::Cancelled { .. })
            ));
        }

        /// No registry API in the hub: the publication cannot start.
        #[tokio::test]
        async fn without_the_platform_api_the_publication_fails() {
            let err = published(None, &CancellationToken::new())
                .await
                .unwrap_err();

            assert!(matches!(err, GtsPublicationError::Backend { .. }), "{err}");
        }

        /// The manifest needs only the configuration, not `init`.
        #[test]
        fn the_manifest_is_the_configured_instance() {
            let manifest = manifest();
            let [(id, content)] = manifest.documents() else {
                panic!("one instance: {manifest:?}");
            };
            assert_eq!(id, &instance_id());
            assert_eq!(
                (&content["vendor"], &content["priority"]),
                (&json!("cf"), &json!(5))
            );
        }
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
