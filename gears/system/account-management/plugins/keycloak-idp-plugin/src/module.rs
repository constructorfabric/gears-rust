//! `toolkit` module wiring for the VHP `IdP` Plugin.

use std::sync::Arc;

use account_management_sdk::IdpPluginSpecV1;
use account_management_sdk::idp::IdpPluginClient;
use async_trait::async_trait;
use credstore_sdk::SecretRef;
use service_principal_sdk::ServicePrincipalClientV1;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit::{Gear, context::GearCtx};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::{VpIdpPluginConfig, VpIdpPluginEnabledProbe};
use crate::domain::credstore::{CredStoreReader, CredStoreWriter};
use crate::domain::kc::factory::KeycloakAdminClientFactory;
use crate::domain::metadata_codec::MetadataCodec;
use crate::domain::ports::metrics::{
    CredstoreMetricsPort, FailureMetricsPort, KcAdminMetricsPort, MetadataCodecMetricsPort,
    SpOpMetricsPort, TenantLifecycleMetricsPort, UserOpMetricsPort,
};
use crate::domain::service_principal_facade::ServicePrincipalFacade;
use crate::domain::system_actor::build_system_ctx;
use crate::domain::tenant_facade::TenantFacade;
use crate::domain::user_facade::UserFacade;
use crate::idp_impl::KeycloakIdpPlugin;
use crate::infra::kc_http::ReqwestKcTransport;
use crate::infra::metrics::build_default_adapter;

// `account-management-sdk` is a Rust SDK dep (Cargo.toml); the plugin
// publishes a `PluginV1<IdpPluginSpecV1>` instance to types-registry
// at init and registers `Arc<dyn IdpPluginClient>` in ClientHub under
// `register_scoped(scope = gts_id)`. AM's `Gear::init` resolves it
// via `choose_plugin_instance` + `get_scoped` against the configured
// `idp.vendor` (default `"cf"`, deploys override to `"virtuozzo"` to
// pick this plugin).
//
// NO toolkit runtime `deps` entry for `account-management` because
// the host process may legitimately not load that module (e.g. this
// dev/PoC build) and the plugin must still install. `types-registry`
// IS declared as a dep because the catalogue publish is a hard
// prerequisite of the AM-side scoped resolution.
#[toolkit::gear(name = "vp-idp-plugin", deps = [types_registry, credstore])]
#[derive(Default)]
pub struct VpIdpPluginGear;

#[async_trait]
impl Gear for VpIdpPluginGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Phase 1: read the `enabled` flag without requiring Keycloak fields.
        // Uses `config_or_default` so a missing module section → enabled = true
        // (production default). This mirrors the temporal-workflow-executor-plugin
        // pattern and allows test/CI configs to opt out with `enabled: false`.
        let probe: VpIdpPluginEnabledProbe = ctx
            .config_or_default()
            .map_err(|e| anyhow::anyhow!("vp-idp-plugin: failed to read enabled flag: {e}"))?;
        if !probe.enabled {
            tracing::info!(
                module = "vp-idp-plugin",
                "disabled via config; skipping initialization"
            );
            return Ok(());
        }

        // Phase 2: `config_expanded` runs toolkit's `${VAR}` substitution over
        // fields marked `#[expand_vars]` — the static admin secrets resolve here
        // (ADDENDUM §3.1 / §3.2), symmetric with the AM DB DSN's `${PGPASSWORD}`.
        let cfg: VpIdpPluginConfig = ctx
            .config_expanded()
            .map_err(|e| anyhow::anyhow!("vp-idp-plugin: failed to load config: {e}"))?;

        // DESIGN §4.0: validate the SecretRef template at init by interpolating
        // against a 64-char synthetic realm name and feeding the result through
        // SecretRef::new (which enforces [A-Za-z0-9_-]+, ≤255 chars).
        let synthetic_realm = "a".repeat(64);
        let probe = cfg
            .keycloak
            .realm_admin
            .secret_ref_template
            .replace("{realm_name}", &synthetic_realm);
        SecretRef::new(&probe).map_err(|e| {
            anyhow::anyhow!(
                "vp-idp-plugin: secret_ref_template '{}' produces invalid SecretRef \
                 for synthetic realm of length 64: {e}",
                cfg.keycloak.realm_admin.secret_ref_template,
            )
        })?;

        cfg.service_principal
            .validate()
            .map_err(|e| anyhow::anyhow!("vp-idp-plugin: {e}"))?;

        // ---- Plugin-owned system ctx (DESIGN §8.1). ----
        let system_ctx = build_system_ctx(cfg.security_context.platform_tenant_id);

        // ---- CredStore unified client (credstore-sdk). ----
        let cs_client: Arc<dyn credstore_sdk::CredStoreClientV1> = ctx
            .client_hub()
            .get::<dyn credstore_sdk::CredStoreClientV1>()
            .map_err(|e| anyhow::anyhow!("vp-idp-plugin: get CredStoreClientV1: {e}"))?;
        let cs_reader = CredStoreReader::new(Arc::clone(&cs_client), Arc::clone(&system_ctx));
        let cs_writer = CredStoreWriter::new(cs_client, Arc::clone(&system_ctx));

        // ---- Metrics adapter + metadata codec. ----
        // One OTel-backed adapter owns every plugin instrument and
        // implements every port trait; per-facade DI takes typed
        // `Arc<dyn ...Port>` views below.
        let metrics_adapter =
            build_default_adapter(cfg.keycloak.metrics.drop_realm_label_at_cardinality);
        let tenant_metrics: Arc<dyn TenantLifecycleMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let user_metrics: Arc<dyn UserOpMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let credstore_metrics: Arc<dyn CredstoreMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let metadata_metrics: Arc<dyn MetadataCodecMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let failure_metrics: Arc<dyn FailureMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let kc_metrics: Arc<dyn KcAdminMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let sp_metrics: Arc<dyn SpOpMetricsPort> = Arc::clone(&metrics_adapter) as _;
        let codec = Arc::new(MetadataCodec);

        // ---- KC transport (reqwest-backed) + factory. ----
        // Transport construction is async because the optional TLS CA bundle
        // resolution funnels through `CredStoreReader::get`.
        let transport = Arc::new(
            ReqwestKcTransport::from_config(&cfg.keycloak, &cs_reader)
                .await
                .map_err(|e| anyhow::anyhow!("vp-idp-plugin: KC transport init failed: {e}"))?,
        );
        let kc_factory = Arc::new(KeycloakAdminClientFactory::new(
            cfg.keycloak.clone(),
            transport,
            cs_reader.clone(),
            kc_metrics,
        ));

        // Pre-warm bootstrap admin token per DESIGN §4.0 init lifecycle.
        // ADDENDUM §4.4: init fails fast if the token endpoint is unreachable.
        // Note: a missing/empty `${VAR}` referenced from `client_secret` /
        // `default_shared_realm_secret` is caught earlier by
        // `ctx.config_expanded()` above.
        //
        // Bounded retry inside this init absorbs transient k8s-deploy-ordering
        // races (KC pod not yet `Ready`, DNS still propagating) without
        // bouncing the whole orchestrator-level `idp_wait_timeout: 15m` loop
        // and re-running every module's init. Budget is operator-tunable via
        // `keycloak.bootstrap_pre_warm.{max_attempts, backoff}` — defaults
        // (5 attempts × 3s backoff) give `~12s of pure backoff + up to ~25s of
        // attempt time = ~37s worst case` before init bails. Beyond that the
        // orchestrator's outer retry takes over.
        let pre_warm_cfg = &cfg.keycloak.bootstrap_pre_warm;
        pre_warm_bootstrap_admin_with_retry(
            &kc_factory,
            pre_warm_cfg.max_attempts,
            pre_warm_cfg.backoff,
        )
        .await?;

        // ---- Facades. ----
        let tenant_facade = Arc::new(TenantFacade::new(
            cfg.tenant_facade.clone(),
            cfg.keycloak.realm_admin.clone(),
            Arc::clone(&kc_factory),
            Arc::clone(&codec),
            cs_writer,
            Arc::clone(&tenant_metrics),
            Arc::clone(&credstore_metrics),
            Arc::clone(&metadata_metrics),
            Arc::clone(&failure_metrics),
        ));
        let user_facade = Arc::new(UserFacade::new(
            cfg.user_facade.clone(),
            cfg.keycloak.realm_admin.clone(),
            Arc::clone(&kc_factory),
            Arc::clone(&codec),
            Arc::clone(&user_metrics),
            Arc::clone(&metadata_metrics),
            Arc::clone(&failure_metrics),
        ));
        let sp_facade = Arc::new(ServicePrincipalFacade::new(
            cfg.service_principal.clone(),
            Arc::clone(&kc_factory),
            sp_metrics,
            Arc::clone(&failure_metrics),
        ));
        tenant_facade.set_service_principal_purge_facade(Arc::clone(&sp_facade));

        // ---- Plugin root + types-registry catalogue publish +
        //      scoped ClientHub registration. ----
        //
        // Build the `PluginV1<IdpPluginSpecV1>` instance carrying
        // (vendor, priority) so AM's `choose_plugin_instance` filter
        // can match against `account-management.idp.vendor`. The
        // instance segment `vz.virtuozzo.vp_idp.plugin.v1` is the
        // vendor-namespaced suffix that distinguishes this plugin
        // from the in-process `static-idp-plugin` (which publishes
        // as `cf.builtin.static_idp.plugin.v1`).
        let (instance_id, instance_json) = PluginV1::<IdpPluginSpecV1>::build_registration(
            "vz.virtuozzo.vp_idp.plugin.v1",
            cfg.vendor.clone(),
            cfg.priority,
        )
        .map_err(|e| anyhow::anyhow!("vp-idp-plugin: build_registration: {e}"))?;

        // Publish to types-registry. Idempotent restart: treat
        // `AlreadyExists` as success ONLY when the stored spec
        // matches our current serialised instance — otherwise fail
        // immediately so a stale catalogue entry surfaces as an
        // operator-actionable error rather than a silent drift.
        // Mirrors the pattern in `static-idp-plugin` and the AM-
        // owned TR plugin in `account-management::module`.
        let registry = ctx
            .client_hub()
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| anyhow::anyhow!("vp-idp-plugin: get TypesRegistryClient: {e}"))?;
        let results = registry
            .register(vec![instance_json.clone()])
            .await
            .map_err(|e| anyhow::anyhow!("vp-idp-plugin: types-registry register: {e}"))?;
        for result in &results {
            if let RegisterResult::Err { error, .. } = result {
                if matches!(
                    error,
                    toolkit::api::canonical_prelude::CanonicalError::AlreadyExists { .. }
                ) {
                    let existing =
                        registry
                            .get_instance(instance_id.as_ref())
                            .await
                            .map_err(|e| {
                                anyhow::anyhow!("vp-idp-plugin: verify existing instance: {e}")
                            })?;
                    if existing.object != instance_json {
                        return Err(anyhow::anyhow!(
                            "vp-idp-plugin: instance `{instance_id}` already registered \
                             with a different spec — refusing to silently drift",
                        ));
                    }
                } else {
                    return Err(anyhow::anyhow!(
                        "vp-idp-plugin: types-registry registration failed: {error}"
                    ));
                }
            }
        }

        let plugin = Arc::new(KeycloakIdpPlugin {
            tenant_facade,
            user_facade,
            codec: Arc::clone(&codec),
            sp_facade,
        });

        // Unscoped: service-principal consumers need no vendor selection.
        let sp_dyn: Arc<dyn ServicePrincipalClientV1> = Arc::clone(&plugin) as _;
        ctx.client_hub()
            .register::<dyn ServicePrincipalClientV1>(sp_dyn);

        let plugin_dyn: Arc<dyn IdpPluginClient> = plugin;
        // Scoped registration keyed on the catalogue instance id
        // (`ClientScope::gts_id`). AM resolves via
        // `ClientHub::get_scoped::<dyn IdpPluginClient>` after
        // selecting this instance through `choose_plugin_instance`
        // — see `account_management::module::resolve_idp_plugin`.
        ctx.client_hub()
            .register_scoped::<dyn IdpPluginClient>(ClientScope::gts_id(&instance_id), plugin_dyn);

        tracing::info!(
            module = "vp-idp-plugin",
            keycloak_base_url = %cfg.keycloak.base_url,
            vendor = %cfg.vendor,
            priority = cfg.priority,
            instance_id = %instance_id,
            "published PluginV1<IdpPluginSpecV1>, registered scoped IdpPluginClient and unscoped ServicePrincipalClientV1"
        );

        Ok(())
    }
}

/// Decide whether a pre-warm failure deserves another attempt.
///
/// The retry loop exists to absorb deploy-ordering transients (KC pod not
/// yet `Ready`, `OpenBao` still warming up). Errors that are guaranteed to
/// reproduce — bad `client_secret`, missing realm, broken base URL —
/// MUST fast-bail so init surfaces the real cause within milliseconds
/// instead of burning the full retry budget on a futile poll.
fn is_pre_warm_retryable(err: &crate::domain::error::PluginError) -> bool {
    use crate::domain::error::PluginError;
    match err {
        // KC token endpoint: 5xx + 429 transient, transport/timeout transient,
        // 4xx-non-429 permanent (invalid_client, unauthorized_client, etc.).
        PluginError::KcRest { status, .. } => status.is_retryable(),
        // OpenBao readiness lag is a real deploy-ordering transient — vp-idp
        // pods sometimes win the readiness race against credstore. Retry.
        PluginError::CredStoreRead { .. } => true,
        // Anything else reaching this path (`Config` from a malformed base URL,
        // etc.) is operator misconfiguration and will not heal on its own.
        _ => false,
    }
}

/// Pre-warm the bootstrap-admin token cache with bounded retry. Each
/// failure logs WARN with attempt count + the underlying error; the
/// final error (if budget exhausts) is annotated with the attempt
/// total so it's distinguishable from a single-shot failure in logs /
/// support tickets. Permanent failures (4xx-non-429, malformed base URL)
/// bail on the first attempt — see [`is_pre_warm_retryable`]. Budget is
/// caller-supplied (operator-tunable via
/// [`crate::config::BootstrapPreWarmConfig`]); tests pass scaled-down
/// values so they don't hold the runtime for the full production budget.
async fn pre_warm_bootstrap_admin_with_retry(
    kc_factory: &KeycloakAdminClientFactory,
    max_attempts: u32,
    backoff: std::time::Duration,
) -> anyhow::Result<()> {
    let mut last_err: Option<(u32, crate::domain::error::PluginError)> = None;
    for attempt in 1..=max_attempts {
        match kc_factory.bootstrap_client().await {
            Ok(_) => {
                if attempt > 1 {
                    tracing::info!(
                        target: "vp_idp_plugin.init",
                        attempt,
                        "bootstrap admin pre-warm succeeded after retry"
                    );
                }
                return Ok(());
            }
            Err(e) => {
                let retryable = is_pre_warm_retryable(&e);
                tracing::warn!(
                    target: "vp_idp_plugin.init",
                    attempt,
                    max_attempts,
                    error = %e,
                    retryable,
                    "bootstrap admin pre-warm failed"
                );
                last_err = Some((attempt, e));
                // Fast-bail on permanent failures: another sleep+attempt
                // against the same broken credential / missing realm cannot
                // recover. Surface the real error to the operator now.
                if !retryable {
                    break;
                }
                if attempt < max_attempts {
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
    let (attempts_used, final_err) = last_err.unwrap_or_else(|| {
        // Unreachable: the loop above only exits via `return Ok(())` on
        // success or by recording an error into `last_err`. The
        // `unwrap_or_else` here exists purely to satisfy clippy's
        // `expect_used` lint without changing program behaviour.
        (
            0,
            crate::domain::error::PluginError::Config {
                detail: "internal: pre_warm retry loop exhausted with no recorded error"
                    .to_string(),
            },
        )
    });
    Err(anyhow::anyhow!(
        "vp-idp-plugin: bootstrap admin pre-warm bailed after {attempts_used}/{max_attempts} attempt(s) ({backoff:?} backoff between attempts); last error: {final_err}"
    ))
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod module_tests;
