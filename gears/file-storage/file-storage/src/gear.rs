//! Gear entry point and capability wiring.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sea_orm_migration::MigrationTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::{DBProvider, DbError};
use tracing::{debug, info};

use crate::api::rest::routes;
use crate::config::FileStorageConfig;
use crate::domain::authz::Authorizer;
use crate::domain::local_client::FileStorageLocalClient;
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::ports::{FileStorageMetricsPort, MultipartStore, PolicyStore};
use crate::domain::service::{FileService, ServiceConfig};
use crate::infra::authz::PolicyEnforcerAuthorizer;
use crate::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, S3Backend, StorageBackend,
};
use crate::infra::metrics::FileStorageMetricsMeter;
use crate::infra::signed_url::Issuer;
use crate::infra::storage::Store;

/// Ids of the always-present `local-fs` backend and the optional `memory` backend.
const LOCAL_FS_ID: &str = "local-fs";
const MEMORY_ID: &str = "memory";

/// `FileStorage` control-plane gear.
///
/// Owns the metadata DB and the REST surface (`/api/file-storage/v1`). Content never
/// transits this gear (signed URLs against the sidecar), and it runs no background worker.
#[toolkit::gear(
    name = "file-storage",
    deps = [authz_resolver],
    capabilities = [db, rest]
)]
pub struct FileStorageGear {
    service: OnceLock<Arc<FileService>>,
    multipart_service: OnceLock<Arc<MultipartService>>,
    policy_service: OnceLock<Arc<PolicyService>>,
    /// Shared-secret credential for the s2s callbacks (`handlers::FinalizeAuth`).
    finalize_auth: OnceLock<Arc<crate::api::rest::handlers::FinalizeAuth>>,
}

impl Default for FileStorageGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            multipart_service: OnceLock::new(),
            policy_service: OnceLock::new(),
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

        // `cfg.validate()` already rejected an absent/empty secret.
        let secret = cfg
            .finalize_internal_secret
            .as_ref()
            .map(|s| s.expose().to_owned())
            .ok_or_else(|| anyhow::anyhow!("finalize_internal_secret is required"))?;
        let finalize_auth = Arc::new(crate::api::rest::handlers::FinalizeAuth::new(secret));
        self.finalize_auth
            .set(Arc::clone(&finalize_auth))
            .map_err(|_| {
                anyhow::anyhow!("{} finalize auth already initialized", Self::MODULE_NAME)
            })?;

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        let backends =
            build_backend_registry(&cfg).map_err(|e| anyhow::anyhow!("backend registry: {e}"))?;

        // A configured seed keeps the keypair stable across restarts; otherwise the key
        // is ephemeral (local dev).
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

        // Per-type access decisions via the platform Authorization Service. Tenant
        // isolation is independent of the PDP (point ops prefetch within the tenant;
        // listing applies the tenant scope).
        let authz = ctx
            .client_hub()
            .get::<dyn authz_resolver_sdk::AuthZResolverApi>()
            .map_err(|e| anyhow::anyhow!("failed to resolve AuthZ resolver: {e}"))?;
        let authorizer: Arc<dyn Authorizer> = Arc::new(PolicyEnforcerAuthorizer::new(authz));

        let svc_cfg = ServiceConfig {
            default_url_ttl_secs: i64::try_from(cfg.default_url_ttl_secs).unwrap_or(i64::MAX),
            sidecar_base_url: cfg.sidecar_base_url,
            default_page_size: cfg.default_page_size,
            max_page_size: cfg.max_page_size,
            idempotency_ttl_secs: cfg.idempotency_ttl_secs,
        };

        let metrics_scope =
            opentelemetry::InstrumentationScope::builder(Self::MODULE_NAME.to_owned()).build();
        let metrics: Arc<dyn FileStorageMetricsPort> = Arc::new(FileStorageMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(metrics_scope),
            "file_storage",
        ));

        let store = Store::new(Arc::clone(&db));

        let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
        let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());

        // Needed by both services before `svc_cfg` is moved.
        let sidecar_base_url = svc_cfg.sidecar_base_url.clone();
        let url_ttl_secs = svc_cfg.default_url_ttl_secs;

        // TODO: wire the quota-enforcement client once the Quota Enforcement gear
        // exposes an SDK crate; until then no quota checks are performed.
        //
        // TODO: wire the usage reporter (currently `None`). `UsageCollectorClientV1` is
        // reachable via `ctx.client_hub()`, but mapping `UsageDelta` to the collector
        // model needs a registered usage type, per-call idempotency keys, and
        // compensation records (`corrects_id`) for negative deltas, which this gear
        // does not track.
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
            .with_usage_reporter(None), // see TODO above
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

        ctx.client_hub()
            .register::<dyn file_storage_sdk::FileStorageClientV1>(Arc::new(
                FileStorageLocalClient::new(),
            ));

        info!("{} gear initialized", Self::MODULE_NAME);
        Ok(())
    }
}

/// Builds the backend registry from config: `local-fs` always, `memory` if enabled, plus
/// one `S3Backend` per `cfg.s3_backends` entry. `cfg.default_backend_id` overrides the
/// default (`local-fs`); an unknown id fails via `BackendRegistry::new`.
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
        // No I/O: a bad endpoint or missing credentials fail gear init here.
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
