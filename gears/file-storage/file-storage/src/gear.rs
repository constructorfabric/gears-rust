//! Gear entry point and capability wiring.
//!
//! @cpt-cf-file-storage-component-http-gateway

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sea_orm_migration::MigrationTrait;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::RunnableCapability;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::{DBProvider, DbError};
use tracing::{debug, info};

use crate::api::rest::routes;
use crate::config::FileStorageConfig;
use crate::domain::authz::Authorizer;
use crate::domain::cleanup::{CleanupConfig, CleanupEngine};
use crate::domain::local_client::FileStorageLocalClient;
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::ports::{CleanupStore, FileStorageMetricsPort, MultipartStore, PolicyStore};
use crate::domain::service::{FileService, ServiceConfig};
use crate::infra::authz::PolicyEnforcerAuthorizer;
use crate::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, S3Backend, StorageBackend,
};
use crate::infra::metrics::FileStorageMetricsMeter;
use crate::infra::signed_url::Issuer;
use crate::infra::storage::Store;

/// Default + in-memory backend ids configured in P1 (static).
const LOCAL_FS_ID: &str = "local-fs";
const MEMORY_ID: &str = "memory";

/// `FileStorage` control-plane gear.
///
/// `capabilities = [db, rest, stateful]`: owns the metadata DB (P1 migration), the
/// control-plane REST surface (`/api/file-storage/v1`). Content never transits
/// this gear — it moves over signed URLs against the sidecar. Stateful lifecycle
/// is used only for the cooperative background cleanup sweep.
#[toolkit::gear(
    name = "file-storage",
    deps = [authz_resolver],
    capabilities = [db, rest, stateful]
)]
pub struct FileStorageGear {
    service: OnceLock<Arc<FileService>>,
    multipart_service: OnceLock<Arc<MultipartService>>,
    policy_service: OnceLock<Arc<PolicyService>>,
    cleanup_deferred: OnceLock<Option<CleanupDeferred>>,
    cleanup_cancel: Mutex<Option<CancellationToken>>,
    cleanup_handle: Mutex<Option<JoinHandle<()>>>,
    /// P2 0.1 remaining: interim gear-local shared-secret credential for the
    /// s2s finalize/report-part callback routes — see
    /// `crate::api::rest::handlers::FinalizeAuth`.
    finalize_auth: OnceLock<Arc<crate::api::rest::handlers::FinalizeAuth>>,
}

struct CleanupDeferred {
    engine: Arc<CleanupEngine>,
    metrics: Arc<dyn FileStorageMetricsPort>,
    sweep_interval_secs: u64,
    orphan_grace_secs: u64,
}

impl Default for FileStorageGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            multipart_service: OnceLock::new(),
            policy_service: OnceLock::new(),
            cleanup_deferred: OnceLock::new(),
            cleanup_cancel: Mutex::new(None),
            cleanup_handle: Mutex::new(None),
            finalize_auth: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for FileStorageGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: FileStorageConfig = ctx.config_or_default()?;
        cfg.validate()?;
        debug!(
            sidecar = %cfg.sidecar_base_url,
            storage_root = %cfg.storage_root,
            "Loaded file-storage config"
        );

        // P2 0.1 remaining: interim gear-local shared-secret credential for
        // the s2s finalize/report-part callback routes (`None` preserves the
        // pre-0.1 token-only trust model). `cfg.validate()` above already
        // rejected an absent secret when `require_finalize_internal_secret`
        // is set, so this is a plain construction.
        let finalize_auth = Arc::new(crate::api::rest::handlers::FinalizeAuth::new(
            cfg.finalize_internal_secret
                .as_ref()
                .map(|s| s.expose().to_owned()),
        ));
        self.finalize_auth
            .set(Arc::clone(&finalize_auth))
            .map_err(|_| {
                anyhow::anyhow!("{} finalize auth already initialized", Self::MODULE_NAME)
            })?;

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // P1 static backends: a local filesystem backend (always present)
        // plus an optional in-memory backend, satisfying the "≥2 backend
        // types" target for dev/test without shipping a non-durable backend
        // to every deployment by default.
        let backends =
            build_backend_registry(&cfg).map_err(|e| anyhow::anyhow!("backend registry: {e}"))?;

        // URL-signing key. A configured seed yields a keypair that is stable
        // across restarts (so the sidecar's public key keeps verifying issued
        // URLs); without one we fall back to an ephemeral key for local dev.
        let max_ttl = i64::try_from(cfg.max_url_ttl_secs).unwrap_or(i64::MAX);
        let issuer = Arc::new(if let Some(seed_b64) = &cfg.signing_key_seed {
            let seed = URL_SAFE_NO_PAD
                .decode(seed_b64.expose().trim())
                .map_err(|e| anyhow::anyhow!("invalid file-storage signing_key_seed: {e}"))?;
            Issuer::from_seed(&seed, max_ttl).map_err(|e| anyhow::anyhow!("signing key: {e}"))?
        } else {
            info!(
                "file-storage: no signing_key_seed configured - generating an EPHEMERAL \
                 URL-signing key. Signed URLs will not survive a restart and the sidecar must \
                 be reconfigured with the matching public key. Set signing_key_seed for \
                 production."
            );
            Issuer::generate(max_ttl).map_err(|e| anyhow::anyhow!("signing key: {e}"))?
        });
        info!(
            sidecar_public_key = %URL_SAFE_NO_PAD.encode(issuer.public_key()),
            "file-storage URL-signing public key (configure FS_SIDECAR_PUBLIC_KEY with this)"
        );

        // Per-type access decisions via the platform Authorization Service
        // (`cpt-cf-file-storage-fr-authorization`). Tenant-boundary enforcement
        // is independent of the PDP (point ops prefetch within the tenant;
        // listing applies the tenant scope).
        let authz = ctx
            .client_hub()
            .get::<dyn authz_resolver_sdk::AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to resolve AuthZ resolver: {e}"))?;
        let authorizer: Arc<dyn Authorizer> = Arc::new(PolicyEnforcerAuthorizer::new(authz));

        let svc_cfg = ServiceConfig {
            default_url_ttl_secs: i64::try_from(cfg.default_url_ttl_secs).unwrap_or(i64::MAX),
            sidecar_base_url: cfg.sidecar_base_url,
            default_page_size: cfg.default_page_size,
            max_page_size: cfg.max_page_size,
            idempotency_ttl_secs: cfg.idempotency_ttl_secs,
        };

        // P2 1.8 remediation: OTel Meter obtained via meter_with_scope, mirroring
        // mini-chat's `infra::metrics::MiniChatMetricsMeter` wiring pattern
        // (gears/mini-chat/mini-chat/src/gear.rs).
        let metrics_scope =
            opentelemetry::InstrumentationScope::builder(Self::MODULE_NAME.to_owned()).build();
        let metrics: Arc<dyn FileStorageMetricsPort> = Arc::new(FileStorageMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(metrics_scope),
            "file_storage",
        ));

        let store = Store::new(Arc::clone(&db));

        // Upcast to the narrow capability traits before distributing.
        // `Store` is Clone, so each consumer gets its own clone wrapped in Arc.
        let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
        let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
        let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
        let sweep_backends = backends.clone();

        // Extract values needed by both services before moving svc_cfg.
        let sidecar_base_url = svc_cfg.sidecar_base_url.clone();
        let url_ttl_secs = svc_cfg.default_url_ttl_secs;

        // TODO(P2): wire the quota-enforcement client once the Quota Enforcement
        // gear exposes an SDK crate. For now, no quota checks are performed.
        //
        // TODO(P2 1.12 remediation): wire the usage reporter off `None`.
        // `usage-collector-sdk`'s `UsageCollectorClientV1` (resolved the same
        // way `authz_resolver_sdk::AuthZResolverClient` is resolved just
        // above) is mechanically reachable via `ctx.client_hub().get::<...>()`,
        // but an adapter from this gear's simple `UsageDelta{bytes_delta,
        // file_count_delta}` shape to the collector's actual wire model is a
        // non-trivial design decision, not a mechanical wiring step:
        // `UsageRecord` requires a registered `UsageTypeGtsId` (a `create_usage_type`
        // call this gear would need to own/idempotently ensure), a per-call
        // `idempotency_key`, a `resource_ref`, and -- critically -- negative
        // deltas are modeled as *compensations* (`corrects_id` pointing back
        // at the specific prior credit record's `uuid`), which this gear does
        // not currently track anywhere. Emitting bare negative-value counter
        // rows without that lineage would violate the collector's L1
        // referential rule. Symmetry of the deltas themselves (this
        // remediation's actual bug) is fixed below and is independent of this
        // follow-up.
        let service = Arc::new(
            FileService::new(
                store,
                backends.clone(),
                Arc::clone(&issuer),
                Arc::clone(&authorizer),
                svc_cfg,
                None, // quota_client
                None, // usage_reporter -- see TODO above
            )
            .with_metrics(Arc::clone(&metrics)),
        );
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let multipart_svc = Arc::new(
            MultipartService::new(
                multipart_store,
                backends,
                Arc::clone(&authorizer),
                None, // quota_client
                Arc::clone(&issuer),
                sidecar_base_url,
                url_ttl_secs,
            )
            .with_metrics(Arc::clone(&metrics))
            .with_usage_reporter(None), // see TODO above `service`
        );
        self.multipart_service.set(multipart_svc).map_err(|_| {
            anyhow::anyhow!(
                "{} multipart service already initialized",
                Self::MODULE_NAME
            )
        })?;

        let policy_svc = Arc::new(PolicyService::new(policy_store, authorizer));
        self.policy_service.set(policy_svc).map_err(|_| {
            anyhow::anyhow!("{} policy service already initialized", Self::MODULE_NAME)
        })?;

        let cleanup_deferred = if cfg.enable_background_sweep {
            Some(CleanupDeferred {
                engine: Arc::new(
                    CleanupEngine::new(
                        sweep_store,
                        sweep_backends,
                        CleanupConfig {
                            orphan_grace_secs: cfg.orphan_grace_secs,
                        },
                    )
                    .with_usage_reporter(None), // see TODO above `service`
                ),
                metrics: Arc::clone(&metrics),
                sweep_interval_secs: cfg.sweep_interval_secs,
                orphan_grace_secs: cfg.orphan_grace_secs,
            })
        } else {
            None
        };
        self.cleanup_deferred
            .set(cleanup_deferred)
            .map_err(|_| anyhow::anyhow!("{} cleanup already initialized", Self::MODULE_NAME))?;

        ctx.client_hub()
            .register::<dyn file_storage_sdk::FileStorageClientV1>(Arc::new(
                FileStorageLocalClient::new(),
            ));

        info!("{} gear initialized", Self::MODULE_NAME);
        Ok(())
    }
}

#[async_trait]
impl RunnableCapability for FileStorageGear {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let Some(cleanup) = self.cleanup_deferred.get().ok_or_else(|| {
            anyhow::anyhow!(
                "{} cleanup not initialized - init() must run before start()",
                Self::MODULE_NAME
            )
        })?
        else {
            return Ok(());
        };

        let cleanup_cancel = cancel.child_token();
        let handle_cancel = cleanup_cancel.clone();
        let engine = Arc::clone(&cleanup.engine);
        let metrics = Arc::clone(&cleanup.metrics);
        let sweep_secs = cleanup.sweep_interval_secs;
        let orphan_grace_secs = cleanup.orphan_grace_secs;

        let handle = tokio::spawn(async move {
            let interval = tokio::time::Duration::from_secs(sweep_secs);
            loop {
                tokio::select! {
                    () = handle_cancel.cancelled() => {
                        tracing::info!("file-storage background cleanup sweep stopped");
                        break;
                    }
                    () = tokio::time::sleep(interval) => {
                        let result = engine.run_sweep().await;
                        // P2 1.8 remediation: export the same tallies as metrics
                        // counters at the point they are already logged.
                        metrics.record_sweep_result(
                            u64::try_from(result.abandoned_pending_deleted).unwrap_or(u64::MAX),
                            u64::try_from(result.abandoned_files_deleted).unwrap_or(u64::MAX),
                            u64::try_from(result.expired_multipart_aborted).unwrap_or(u64::MAX),
                            u64::try_from(result.retention_expired_deleted).unwrap_or(u64::MAX),
                            result.idempotency_keys_deleted,
                        );
                        tracing::info!(?result, "file-storage cleanup sweep completed");
                    }
                }
            }
        });

        let cancel_already_set = {
            let mut guard = self
                .cleanup_cancel
                .lock()
                .map_err(|e| anyhow::anyhow!("cleanup_cancel lock: {e}"))?;
            if guard.is_some() {
                true
            } else {
                *guard = Some(cleanup_cancel);
                false
            }
        };
        if cancel_already_set {
            handle.abort();
            anyhow::bail!("{} cleanup already started", Self::MODULE_NAME);
        }

        let mut handle = Some(handle);
        let handle_err = {
            match self.cleanup_handle.lock() {
                Ok(mut guard) => {
                    if guard.is_some() {
                        Some("cleanup_handle already set".to_owned())
                    } else {
                        *guard = handle.take();
                        None
                    }
                }
                Err(e) => Some(format!("cleanup_handle lock: {e}")),
            }
        };
        if let Some(msg) = handle_err {
            if let Ok(mut cancel_guard) = self.cleanup_cancel.lock()
                && let Some(cancel) = cancel_guard.take()
            {
                cancel.cancel();
            }
            if let Some(handle) = handle {
                handle.abort();
            }
            anyhow::bail!("{} {msg}", Self::MODULE_NAME);
        }

        info!(
            "file-storage background cleanup sweep enabled (interval={}s, grace={}s)",
            sweep_secs, orphan_grace_secs
        );
        Ok(())
    }

    async fn stop(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        if let Some(cleanup_cancel) = self
            .cleanup_cancel
            .lock()
            .map_err(|e| anyhow::anyhow!("cleanup_cancel lock: {e}"))?
            .take()
        {
            cleanup_cancel.cancel();
        }

        let handle = self
            .cleanup_handle
            .lock()
            .map_err(|e| anyhow::anyhow!("cleanup_handle lock: {e}"))?
            .take();
        if let Some(handle) = handle {
            tokio::select! {
                result = handle => {
                    if let Err(e) = result
                        && !e.is_cancelled()
                    {
                        tracing::warn!(error = ?e, "file-storage cleanup sweep task failed");
                    }
                }
                () = cancel.cancelled() => {
                    tracing::info!("file-storage cleanup sweep stop cancelled by framework deadline");
                }
            }
        }
        Ok(())
    }
}

/// Builds the backend registry from config: `local-fs` is always present and
/// is the default (unless overridden — see below); the non-durable `memory`
/// backend only joins when `cfg.enable_in_memory_backend` is set (dev/test
/// opt-in — see `FileStorageConfig::enable_in_memory_backend`); zero or more
/// `S3Backend`s join per `cfg.s3_backends` entry (P2 1.7.3 config wiring).
/// Extracted as a free function so it is unit-testable without a live
/// `GearCtx`.
///
/// `cfg.default_backend_id` (P2 1.7 Stage 6 e2e wiring), when set, overrides
/// the registry's default backend — e.g. so a deployment/test harness can
/// make a configured S3 backend the target of new `create`/
/// `initiate_multipart` calls instead of `local-fs`. An id naming no
/// configured backend fails fast via `BackendRegistry::new`'s own validation.
fn build_backend_registry(
    cfg: &FileStorageConfig,
) -> Result<BackendRegistry, crate::domain::error::DomainError> {
    let local: Arc<dyn StorageBackend> =
        Arc::new(LocalFsBackend::new(LOCAL_FS_ID, &cfg.storage_root));
    let mut backend_list: Vec<Arc<dyn StorageBackend>> = vec![local];
    if cfg.enable_in_memory_backend {
        backend_list.push(Arc::new(InMemoryBackend::new(MEMORY_ID)));
    }
    for s3_cfg in &cfg.s3_backends {
        // `S3Backend::from_config` performs no I/O — a bad endpoint URL or
        // missing credentials (with no env fallback) surfaces here as a
        // regular `Err`, failing gear init fast rather than panicking.
        let s3_backend = S3Backend::from_config(s3_cfg)?;
        backend_list.push(Arc::new(s3_backend));
    }
    let default_id = cfg.default_backend_id.as_deref().unwrap_or(LOCAL_FS_ID);
    BackendRegistry::new(backend_list, default_id)
}

impl DatabaseCapability for FileStorageGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing file-storage P1 database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for FileStorageGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("file-storage service not initialized"))?
            .clone();
        let multipart_service = self
            .multipart_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("file-storage multipart service not initialized"))?
            .clone();
        let policy_service = self
            .policy_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("file-storage policy service not initialized"))?
            .clone();
        let finalize_auth = self
            .finalize_auth
            .get()
            .ok_or_else(|| anyhow::anyhow!("file-storage finalize auth not initialized"))?
            .clone();
        info!("Registering file-storage control-plane REST routes");
        Ok(routes::register_routes(
            router,
            openapi,
            service,
            multipart_service,
            policy_service,
            finalize_auth,
        ))
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
