//! ModKit module shell for FileStorage.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::{Module, ModuleCtx};
use modkit_db::DBProvider;
use modkit_db::DbError;
use std::collections::{HashMap, VecDeque};
use tokio::sync::Mutex;
use tracing::{info, warn};

use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use file_storage_sdk::{BackendId, FileStorageClient};

use crate::api::rest::routes;
use crate::config::{BackendKindCfg, FileStorageConfig, validate_config};
use crate::domain::local_client::LocalClient;
use crate::domain::service::{OrphanQueue, Service};
use crate::errors::InitError;
use crate::infra::backends::registry::BackendRegistry;
use crate::infra::backends::s3::{S3Backend, S3BackendConfig};
use crate::infra::backends::smoke;
use crate::infra::backends::r#trait::{
    BackendDescriptor, SharedBackend, StorageBackend, derive_s3_key,
};
use crate::infra::storage::sea_orm_repo::SeaOrmFilesRepository;

/// Concrete service type with the SeaORM repo.
type ConcreteService = Service<SeaOrmFilesRepository>;

#[modkit::module(
    name = "file-storage",
    deps = ["authz-resolver"],
    capabilities = [rest, db]
)]
pub struct FileStorageModule {
    service: OnceLock<Arc<ConcreteService>>,
}

impl Default for FileStorageModule {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl modkit::contracts::DatabaseCapability for FileStorageModule {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing file-storage database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Module for FileStorageModule {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: FileStorageConfig = ctx.config_or_default()?;
        validate_config(&cfg).map_err(InitError::InvalidConfig)?;

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        let registry = build_registry(&cfg).map_err(InitError::BackendRoster)?;
        let backend_pairs: Vec<(BackendId, &SharedBackend)> = registry.iter().collect();
        smoke::run_smoke_tests(&backend_pairs).await?;
        let registry = Arc::new(registry);

        let repo: Arc<SeaOrmFilesRepository> = Arc::new(SeaOrmFilesRepository::new());

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let orphan_queue: OrphanQueue = Arc::new(Mutex::new(VecDeque::new()));

        let cfg_arc = Arc::new(cfg);
        let service = Arc::new(Service::new(
            db,
            repo,
            policy_enforcer,
            cfg_arc.clone(),
            registry.clone(),
            orphan_queue.clone(),
        ));

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let local_client: Arc<dyn FileStorageClient> = Arc::new(LocalClient::new(service.clone()));
        ctx.client_hub().register(local_client);

        let registry_clone = registry.clone();
        let orphan_clone = orphan_queue.clone();
        let cancel = ctx.cancellation_token().clone();
        tokio::spawn(async move {
            run_orphan_worker(registry_clone, orphan_clone, cancel).await;
        });

        Ok(())
    }
}

#[async_trait]
impl modkit::contracts::RestApiCapability for FileStorageModule {
    fn register_rest(
        &self,
        _ctx: &ModuleCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        info!("FileStorage module: register_rest called");
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();
        let router = routes::register_routes(router, openapi, service);
        Ok(router)
    }
}

fn build_registry(cfg: &FileStorageConfig) -> Result<BackendRegistry, String> {
    let mut backends: HashMap<BackendId, SharedBackend> = HashMap::new();
    for entry in &cfg.backends {
        let BackendKindCfg::S3Compatible = entry.kind;

        let mut capabilities = vec![file_storage_sdk::BackendCapability::PresignedUrls];
        if entry.public_read_urls {
            capabilities.push(file_storage_sdk::BackendCapability::PublicReadUrls);
        }
        if entry.presigned_conditional_put {
            capabilities.push(file_storage_sdk::BackendCapability::PresignedConditionalPut);
        }
        let sdk = file_storage_sdk::Backend {
            id: entry.id,
            kind: file_storage_sdk::BackendKind::S3Compatible,
            default_public: entry.default_public,
            default_private: entry.default_private,
            transport: file_storage_sdk::BackendTransport::Redirect,
            capabilities,
            max_file_size_bytes: entry.max_file_size_bytes,
        };
        let descriptor = BackendDescriptor {
            sdk,
            max_signed_url_ttl_seconds_value: entry.max_signed_url_ttl_seconds,
            tenant_access: entry.tenant_access.clone(),
        };

        let backend: SharedBackend = Arc::new(S3Backend::new(S3BackendConfig {
            descriptor,
            endpoint: entry.endpoint.clone(),
            region: entry.region.clone(),
            bucket: entry.bucket.clone(),
            access_key: entry.access_key.clone(),
            secret_key: entry.secret_key.clone(),
            public_read_urls: entry.public_read_urls,
        }));
        if backends.insert(entry.id, backend).is_some() {
            return Err(format!("duplicate backend id {:?}", entry.id));
        }
    }
    Ok(BackendRegistry::new(backends))
}

async fn run_orphan_worker(
    registry: Arc<BackendRegistry>,
    queue: OrphanQueue,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("orphan-delete worker shutting down");
                return;
            }
            _ = tick.tick() => {}
        }

        let now = time::OffsetDateTime::now_utc();
        let mut due_entries = Vec::new();
        {
            let mut q = queue.lock().await;
            let mut keep: VecDeque<crate::domain::service::OrphanEntry> = VecDeque::new();
            while let Some(e) = q.pop_front() {
                if e.eligible_at <= now {
                    due_entries.push(e);
                } else {
                    keep.push_back(e);
                }
            }
            *q = keep;
        }

        for entry in due_entries {
            let backend: Option<SharedBackend> = registry
                .iter()
                .find(|(id, _)| *id == entry.backend_id)
                .map(|(_, b)| b.clone());
            if let Some(b) = backend {
                let key = derive_s3_key(entry.file_id);
                if let Err(e) = b.delete_object(&key).await {
                    warn!(
                        backend = %entry.backend_id,
                        key = %key,
                        error = %e,
                        "orphan-delete failed; re-queueing with backoff"
                    );
                    let mut q = queue.lock().await;
                    q.push_back(crate::domain::service::OrphanEntry {
                        backend_id: entry.backend_id,
                        file_id: entry.file_id,
                        eligible_at: now + time::Duration::seconds(300),
                    });
                }
            } else {
                warn!(
                    backend = %entry.backend_id,
                    "orphan-delete target backend not in registry; dropping"
                );
            }
        }
    }
}

#[allow(dead_code)]
fn _force_storage_backend_dyn(b: &dyn StorageBackend) -> file_storage_sdk::BackendId {
    b.descriptor().id()
}
