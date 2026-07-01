//! Gear entry point and capability wiring.
//!
//! @cpt-cf-file-storage-component-http-gateway

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
use crate::domain::cleanup::{CleanupConfig, CleanupEngine};
use crate::domain::local_client::FileStorageLocalClient;
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::ports::{CleanupStore, MultipartStore, PolicyStore};
use crate::domain::service::{FileService, ServiceConfig};
use crate::infra::authz::PolicyEnforcerAuthorizer;
use crate::infra::backend::{BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend};
use crate::infra::signed_url::Issuer;
use crate::infra::storage::Store;

/// Default + in-memory backend ids configured in P1 (static).
const LOCAL_FS_ID: &str = "local-fs";
const MEMORY_ID: &str = "memory";

/// `FileStorage` control-plane gear.
///
/// `capabilities = [db, rest]`: owns the metadata DB (P1 migration) and the
/// control-plane REST surface (`/api/file-storage/v1`). Content never transits
/// this gear — it moves over signed URLs against the sidecar.
#[toolkit::gear(
    name = "file-storage",
    deps = ["authz-resolver"],
    capabilities = [db, rest]
)]
pub struct FileStorageGear {
    service: OnceLock<Arc<FileService>>,
    multipart_service: OnceLock<Arc<MultipartService>>,
    policy_service: OnceLock<Arc<PolicyService>>,
}

impl Default for FileStorageGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            multipart_service: OnceLock::new(),
            policy_service: OnceLock::new(),
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

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // P1 static backends: a local filesystem backend (default) plus an
        // in-memory backend, satisfying the "≥2 backend types" target.
        let local: Arc<dyn StorageBackend> =
            Arc::new(LocalFsBackend::new(LOCAL_FS_ID, &cfg.storage_root));
        let memory: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new(MEMORY_ID));
        let backends = BackendRegistry::new(vec![local, memory], LOCAL_FS_ID)
            .map_err(|e| anyhow::anyhow!("backend registry: {e}"))?;

        // URL-signing key. A configured seed yields a keypair that is stable
        // across restarts (so the sidecar's public key keeps verifying issued
        // URLs); without one we fall back to an ephemeral key for local dev.
        let max_ttl = i64::try_from(cfg.max_url_ttl_secs).unwrap_or(i64::MAX);
        let issuer = Arc::new(if let Some(seed_b64) = &cfg.signing_key_seed {
            let seed = URL_SAFE_NO_PAD
                .decode(seed_b64.trim())
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

        let store = Store::new(Arc::clone(&db));

        // Upcast to the narrow capability traits before distributing.
        // `Store` is Clone, so each consumer gets its own clone wrapped in Arc.
        let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
        let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
        let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
        let sweep_backends = backends.clone();

        // TODO(P2): wire the quota-enforcement client once the Quota Enforcement
        // gear exposes an SDK crate. For now, no quota checks are performed.
        // TODO(P2-M5): wire the usage reporter once a Usage Collector SDK is available.
        let service = Arc::new(FileService::new(
            store,
            backends.clone(),
            issuer,
            Arc::clone(&authorizer),
            svc_cfg,
            None, // quota_client
            None, // usage_reporter
        ));
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let multipart_svc = Arc::new(MultipartService::new(
            multipart_store,
            backends,
            Arc::clone(&authorizer),
            None, // quota_client
        ));
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

        // Optional background cleanup sweep (disabled by default so tests are
        // deterministic; enable via `enable_background_sweep = true` in config).
        if cfg.enable_background_sweep {
            let sweep_secs = cfg.sweep_interval_secs;
            let engine = Arc::new(CleanupEngine::new(
                sweep_store,
                sweep_backends,
                CleanupConfig {
                    orphan_grace_secs: cfg.orphan_grace_secs,
                },
            ));
            tokio::spawn(async move {
                let interval = tokio::time::Duration::from_secs(sweep_secs);
                loop {
                    tokio::time::sleep(interval).await;
                    let result = engine.run_sweep().await;
                    tracing::info!(?result, "file-storage cleanup sweep completed");
                }
            });
            info!(
                "file-storage background cleanup sweep enabled (interval={}s, grace={}s)",
                sweep_secs, cfg.orphan_grace_secs
            );
        }

        ctx.client_hub()
            .register::<dyn file_storage_sdk::FileStorageClientV1>(Arc::new(
                FileStorageLocalClient::new(),
            ));

        info!("{} gear initialized", Self::MODULE_NAME);
        Ok(())
    }
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
        info!("Registering file-storage control-plane REST routes");
        Ok(routes::register_routes(
            router,
            openapi,
            service,
            multipart_service,
            policy_service,
        ))
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
