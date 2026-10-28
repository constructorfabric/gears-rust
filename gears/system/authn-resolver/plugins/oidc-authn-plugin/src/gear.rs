//! Oidc `AuthN` resolver plugin gear registration.
//!
//! This gear integrates the `OidcAuthNPlugin` with `ToolKit` runtime by:
//! 1. Reading plugin runtime configuration from `server.yaml` (`init`).
//! 2. Publishing a plugin instance to types-registry and awaiting its admission
//!    (`post_init`, over the consumed `PlatformTypesRegistryApi`).
//! 3. Only then registering the scoped plugin client in `ClientHub`.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverPluginSpecV1;
use opentelemetry::global;
use tokio_util::sync::CancellationToken;
use toolkit::Gear;
use toolkit::client_hub::ClientHub;
use toolkit::context::GearCtx;
use toolkit::contracts::SystemCapability;
use toolkit::gts::PluginV1;
use tracing::{info, warn};
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::config::{INSTANCE_SUFFIX, OidcAuthNGearConfig};
use crate::domain::authenticate::OidcAuthNPlugin;
use crate::domain::metrics::AuthNMetrics;
use crate::infra::runtime::build_oidc_authn_plugin;

/// `ToolKit` gear responsible for wiring oidc-authn plugin into runtime.
///
/// The plugin schema reaches types-registry through the `toolkit-gts` inventory the
/// registry seeds from; this gear publishes only its instance. It is a system gear so its
/// `post_init` runs: after proxy wiring, when the registry's platform API is in the hub
/// wherever the registry runs, and before any request can reach authn-resolver.
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::gear(name = "oidc-authn-plugin", capabilities = [system])]
#[derive(Default)]
pub struct OidcAuthNPluginGear {
    pending: OnceLock<Publication>,
}

/// What `init` prepares and `post_init` publishes.
struct Publication {
    hub: Arc<ClientHub>,
    cancel: CancellationToken,
    plugin: Arc<OidcAuthNPlugin>,
    instance_id: GtsId,
    instance: serde_json::Value,
    vendor: String,
    priority: i16,
}

#[async_trait]
impl Gear for OidcAuthNPluginGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let meter = global::meter(Self::MODULE_NAME);
        let metrics = Arc::new(AuthNMetrics::new(&meter));

        let cfg: OidcAuthNGearConfig = ctx.config().map_err(|error| {
            anyhow::anyhow!(
                "failed to load oidc-authn-plugin configuration from gears.oidc-authn-plugin.config: {error}"
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

        let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
        let instance = PluginV1::<AuthNResolverPluginSpecV1> {
            id: instance_id.clone(),
            vendor: vendor.clone(),
            priority: gts_priority,
            properties: AuthNResolverPluginSpecV1,
        };

        let publication = Publication {
            hub: ctx.client_hub(),
            cancel: ctx.cancellation_token().clone(),
            plugin,
            instance_id: GtsId::try_new(instance_id.as_ref())?,
            instance: serde_json::to_value(&instance)?,
            vendor,
            priority: gts_priority,
        };
        self.pending
            .set(publication)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        Ok(())
    }
}

#[async_trait]
impl SystemCapability for OidcAuthNPluginGear {
    async fn post_init(&self, _sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        let p = self
            .pending
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))?;

        let registry = p.hub.get::<dyn PlatformTypesRegistryApi>().map_err(|e| {
            anyhow::anyhow!("oidc-authn-plugin: no PlatformTypesRegistryApi to publish to: {e}")
        })?;
        publish_startup_entities(
            registry.as_ref(),
            &PublisherContext {
                name: Self::MODULE_NAME.to_owned(),
                version: env!("CARGO_PKG_VERSION").parse()?,
            },
            &[(p.instance_id.clone(), p.instance.clone())],
            &p.cancel,
        )
        .await?;

        // Register plugin in ClientHub using the canonical GTS instance-id
        // scope consumed by authn-resolver runtime lookup, only past admission:
        // a failed publication leaves no hub entry behind.
        p.plugin
            .register(&p.hub)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        info!(
            instance_id = %p.instance_id,
            vendor = %p.vendor,
            priority = p.priority,
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

    /// The publication boundary: the scoped client is bound only once the registry has
    /// admitted the instance.
    mod publication {
        use std::sync::Arc;

        use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
        use serde_json::{Value, json};
        use tokio_util::sync::CancellationToken;
        use toolkit::client_hub::{ClientHub, ClientScope};
        use toolkit::contracts::{Gear, SystemCapability};
        use toolkit::runtime::{GearManager, GrpcInstallerStore, SystemContext};
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

        async fn initialized(
            registry: &Arc<MockTypesRegistry>,
        ) -> (OidcAuthNPluginGear, Arc<ClientHub>) {
            initialized_with(Some(registry), CancellationToken::new()).await
        }

        /// Runs `init` with `registry` installed, if any, under `cancel`.
        async fn initialized_with(
            registry: Option<&Arc<MockTypesRegistry>>,
            cancel: CancellationToken,
        ) -> (OidcAuthNPluginGear, Arc<ClientHub>) {
            let hub = Arc::new(ClientHub::new());
            if let Some(registry) = registry {
                registry.install(&hub);
            }
            let config = json!({ "config": {
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
            } });
            let ctx = GearCtx::new(
                "oidc-authn-plugin",
                uuid::Uuid::new_v4(),
                Arc::new(FixedConfig(config)),
                Arc::clone(&hub),
                cancel,
            );
            let gear = OidcAuthNPluginGear::default();
            gear.init(&ctx).await.expect("init");
            (gear, hub)
        }

        fn system_context() -> SystemContext {
            SystemContext::new(
                uuid::Uuid::new_v4(),
                Arc::new(GearManager::new()),
                Arc::new(GrpcInstallerStore::new()),
            )
        }

        #[tokio::test]
        async fn init_neither_publishes_nor_binds_and_admission_binds() {
            let registry = Arc::new(MockTypesRegistry::new());
            let (gear, hub) = initialized(&registry).await;
            assert!(!bound(&hub));
            assert_eq!(registry.calls(Call::Register), 0);

            gear.post_init(&system_context()).await.expect("published");

            assert!(bound(&hub));
            let content = registry.content(&instance_id()).expect("admitted");
            assert_eq!(
                (&content["vendor"], &content["priority"]),
                (&json!("cf"), &json!(5))
            );
        }

        #[tokio::test]
        async fn a_refused_instance_fails_boot_and_binds_nothing() {
            let registry = Arc::new(MockTypesRegistry::new());
            registry.reject(
                &instance_id(),
                AdmissionFailureReason::IncompatibleWithBaseline,
            );
            let (gear, hub) = initialized(&registry).await;

            let err = gear
                .post_init(&system_context())
                .await
                .expect_err("refused");

            assert!(err.to_string().contains("publishing instance"), "{err:#}");
            assert!(!bound(&hub));
        }

        /// An admission that never completes runs out the shared startup deadline rather
        /// than binding early.
        #[tokio::test(start_paused = true)]
        async fn a_pending_instance_fails_boot_at_the_deadline_and_binds_nothing() {
            let registry = Arc::new(MockTypesRegistry::new().completing_after(u32::MAX));
            let (gear, hub) = initialized(&registry).await;

            let started = tokio::time::Instant::now();
            gear.post_init(&system_context())
                .await
                .expect_err("pending past the deadline");

            assert_eq!(
                started.elapsed(),
                types_registry_sdk::STARTUP_PUBLICATION_DEADLINE,
                "gives up at the shared deadline, neither earlier nor later"
            );
            assert!(registry.calls(Call::Register) > 0);
            assert!(!bound(&hub));
        }

        /// The token captured by `init` still governs the publication in `post_init`.
        #[tokio::test]
        async fn a_cancelled_boot_binds_nothing() {
            let registry = Arc::new(MockTypesRegistry::new());
            let cancel = CancellationToken::new();
            let (gear, hub) = initialized_with(Some(&registry), cancel.clone()).await;
            cancel.cancel();

            gear.post_init(&system_context())
                .await
                .expect_err("cancelled");

            assert!(!bound(&hub));
        }

        /// No registry API in the hub: boot fails rather than binding an unpublished plugin.
        #[tokio::test]
        async fn without_the_platform_api_boot_fails_and_binds_nothing() {
            let (gear, hub) = initialized_with(None, CancellationToken::new()).await;

            let err = gear.post_init(&system_context()).await.expect_err("no API");

            assert!(
                err.to_string().contains("PlatformTypesRegistryApi"),
                "{err:#}"
            );
            assert!(!bound(&hub));
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
