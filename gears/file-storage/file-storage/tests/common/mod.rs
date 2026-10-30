//! Scaffolding shared by the integration tests: temp-file SQLite, a [`StackBuilder`] that wires a
//! `FileService`, `MultipartService`, `PolicyService`, `DataPlaneService` and `CleanupEngine`
//! over one store, and small request/part/claims/SQL helpers.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::sync::Arc;

use axum::response::Response;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer};
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::multipart::{
    CompletedMultipartUpload, MultipartCompleteOutcome, MultipartPlan,
};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::external_clients::{QuotaClient, UsageReporter};
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

pub const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB (a bare `sqlite::memory:` gives each pooled connection its own DB),
/// fully migrated. Returns the DSN too, for tests that tamper through a second connection.
pub async fn build_db_with_dsn(tag: &str) -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-{tag}-{}.db", Uuid::now_v7().simple()));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    (Arc::new(DBProvider::new(db)), dsn)
}

/// A context for `tenant` with a fresh random subject.
pub fn ctx(tenant: Uuid) -> SecurityContext {
    ctx_for(tenant, Uuid::now_v7())
}

/// A context for a specific `(tenant, subject)` pair.
pub fn ctx_for(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

/// A `NewFile` named `name` (`application/octet-stream`, random user owner).
pub fn new_file(name: &str) -> NewFile {
    new_file_of(name, "application/octet-stream")
}

/// A `NewFile` named `name` with `mime_type` and a random user owner.
pub fn new_file_of(name: &str, mime_type: &str) -> NewFile {
    new_file_for(Uuid::now_v7(), name, mime_type)
}

/// A `NewFile` named `name` with `mime_type`, owned by user `owner_id`.
pub fn new_file_for(owner_id: Uuid, name: &str, mime_type: &str) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: name.to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: mime_type.to_owned(),
        custom_metadata: vec![],
    }
}

pub fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    }
}

pub async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid JSON body")
}

/// Lower-case hex of `bytes`.
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut acc, b| {
        write!(acc, "{b:02x}").expect("writing to a String cannot fail");
        acc
    })
}

/// The backend object path of a version, as the service lays it out.
pub fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

/// Valid `Put` claims for `(file_id, version_id)` on the in-memory backend, expiring in an hour.
pub fn put_claims(
    file_id: Uuid,
    version_id: Uuid,
    backend_path: String,
    bind_on_finalize: bool,
) -> Claims {
    Claims {
        op: Op::Put,
        file_id,
        version_id,
        backend_id: "mem".to_owned(),
        backend_path,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize,
    }
}

/// A `Uuid` column value as a SQLite BLOB literal (`Uuid` columns are 16-byte BLOBs).
pub fn uuid_blob(id: Uuid) -> String {
    format!("X'{}'", hex_encode(id.as_bytes()))
}

/// Every service over one fresh store: `FileService`, `MultipartService`, `PolicyService`,
/// `DataPlaneService` and `CleanupEngine`, all sharing the backends, issuer and authorizer.
pub struct Stack {
    pub svc: Arc<FileService>,
    pub msvc: Arc<MultipartService>,
    pub psvc: Arc<PolicyService>,
    pub dp: DataPlaneService,
    pub engine: CleanupEngine,
    pub multipart_store: Arc<dyn MultipartStore>,
    /// The first (default, unless overridden) backend.
    pub backend: Arc<dyn StorageBackend>,
    pub backends: BackendRegistry,
    pub store: Store,
    pub db: Arc<DBProvider<DbError>>,
    pub dsn: String,
    pub issuer: Arc<Issuer>,
    pub authorizer: Arc<dyn Authorizer>,
    pub cfg: ServiceConfig,
}

impl Stack {}

type BackendSet = (Vec<Arc<dyn StorageBackend>>, String);
type MsvcTweak = Box<dyn FnOnce(MultipartService) -> MultipartService>;

/// Builder for a [`Stack`]; every hook defaults to what the plain in-memory tests use
/// (`InMemoryBackend` "mem", `TenantOnlyAuthorizer`, [`base_config`], no quota/usage reporter).
pub struct StackBuilder {
    tag: String,
    db: Option<(Arc<DBProvider<DbError>>, String)>,
    backends: Option<BackendSet>,
    authorizer: Option<Arc<dyn Authorizer>>,
    quota: Option<Arc<dyn QuotaClient>>,
    usage_reporter: Option<Arc<dyn UsageReporter>>,
    issuer: Option<Arc<Issuer>>,
    cfg: ServiceConfig,
    grace_secs: u64,
    msvc_tweak: Option<MsvcTweak>,
}

impl StackBuilder {
    pub fn new(tag: &str) -> Self {
        Self {
            tag: tag.to_owned(),
            db: None,
            backends: None,
            authorizer: None,
            quota: None,
            usage_reporter: None,
            issuer: None,
            cfg: base_config(),
            grace_secs: 86400,
            msvc_tweak: None,
        }
    }

    /// Share one `Issuer` (so tests can mint tokens the services' verifier accepts).
    pub fn issuer(mut self, issuer: Arc<Issuer>) -> Self {
        self.issuer = Some(issuer);
        self
    }

    /// Quota client for both services (single-part create and multipart plan checks).
    pub fn quota(mut self, quota: Arc<dyn QuotaClient>) -> Self {
        self.quota = Some(quota);
        self
    }

    /// Usage reporter for both services.
    pub fn usage_reporter(mut self, reporter: Arc<dyn UsageReporter>) -> Self {
        self.usage_reporter = Some(reporter);
        self
    }

    pub fn url_ttl_secs(mut self, secs: i64) -> Self {
        self.cfg.default_url_ttl_secs = secs;
        self
    }

    /// Tweak the `MultipartService` (session TTL, lease, ...) before it is shared.
    pub fn tweak_msvc(
        mut self,
        tweak: impl FnOnce(MultipartService) -> MultipartService + 'static,
    ) -> Self {
        self.msvc_tweak = Some(Box::new(tweak));
        self
    }

    pub async fn build(self) -> Stack {
        let (db, dsn) = match self.db {
            Some(pair) => pair,
            None => build_db_with_dsn(&self.tag).await,
        };
        let (backend_list, default_id) = self.backends.unwrap_or_else(|| {
            (
                vec![Arc::new(InMemoryBackend::new("mem"))],
                "mem".to_owned(),
            )
        });
        let backend = Arc::clone(&backend_list[0]);
        let backends = BackendRegistry::new(backend_list, &default_id).expect("registry");
        let issuer = self
            .issuer
            .unwrap_or_else(|| Arc::new(Issuer::generate(3600).expect("issuer")));
        let authorizer: Arc<dyn Authorizer> = self
            .authorizer
            .unwrap_or_else(|| Arc::new(TenantOnlyAuthorizer));
        let cfg = self.cfg;

        let store = Store::new(Arc::clone(&db));
        let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
        let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
        let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());

        let svc = Arc::new(FileService::new(
            store.clone(),
            backends.clone(),
            Arc::clone(&issuer),
            Arc::clone(&authorizer),
            cfg.clone(),
            self.quota.clone(),
            self.usage_reporter.clone(),
        ));
        let mut msvc = MultipartService::new(
            Arc::clone(&multipart_store),
            backends.clone(),
            Arc::clone(&authorizer),
            self.quota,
            Arc::clone(&issuer),
            cfg.sidecar_base_url.clone(),
            cfg.default_url_ttl_secs,
        );
        if let Some(reporter) = self.usage_reporter.clone() {
            msvc = msvc.with_usage_reporter(Some(reporter));
        }
        if let Some(tweak) = self.msvc_tweak {
            msvc = tweak(msvc);
        }
        let psvc = Arc::new(PolicyService::new(
            policy_store,
            Arc::clone(&authorizer),
            cfg.default_page_size,
            cfg.max_page_size,
        ));
        let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
        let mut engine = CleanupEngine::new(
            sweep_store,
            backends.clone(),
            CleanupConfig {
                orphan_grace_secs: self.grace_secs,
            },
        );
        if let Some(reporter) = self.usage_reporter {
            engine = engine.with_usage_reporter(Some(reporter));
        }

        Stack {
            svc,
            msvc: Arc::new(msvc),
            psvc,
            dp,
            engine,
            multipart_store,
            backend,
            backends,
            store,
            db,
            dsn,
            issuer,
            authorizer,
            cfg,
        }
    }
}

/// Build a [`Stack`] on the caller's `issuer` (so tests can mint tokens `svc.verifier()` accepts).
/// `complete_lease_secs` overrides the multipart complete lease when set.
pub async fn build_stack(
    tag: &str,
    issuer: Arc<Issuer>,
    complete_lease_secs: Option<i64>,
) -> Stack {
    build_stack_with(tag, issuer, 3600, move |msvc| match complete_lease_secs {
        Some(secs) => msvc.with_complete_lease_secs(secs),
        None => msvc,
    })
    .await
}

/// [`build_stack`] with a custom per-part URL TTL (for both services) and a hook to tweak the
/// `MultipartService` (session TTL, lease, ...) before it is shared.
pub async fn build_stack_with(
    tag: &str,
    issuer: Arc<Issuer>,
    url_ttl_secs: i64,
    tweak: impl FnOnce(MultipartService) -> MultipartService + 'static,
) -> Stack {
    StackBuilder::new(tag)
        .issuer(issuer)
        .url_ttl_secs(url_ttl_secs)
        .tweak_msvc(tweak)
        .build()
        .await
}

/// Upload one part of `plan` the way the sidecar would: bytes to the backend, then the part row.
pub async fn put_part(
    store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &MultipartPlan,
    file_id: Uuid,
    part_number: u32,
    data: Bytes,
) {
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .expect("query session")
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    let part = plan
        .parts
        .iter()
        .find(|p| p.part_number == part_number)
        .unwrap_or_else(|| panic!("part {part_number} not in plan"));
    assert_eq!(
        data.len() as u64,
        part.size,
        "part {part_number}: test data must match the plan"
    );

    let (backend_etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            part_number,
            part.offset,
            data,
        )
        .await
        .expect("backend upload_part");

    store
        .upsert_multipart_part(
            plan.upload_id,
            i32::try_from(part_number).unwrap(),
            &backend_etag,
            part_hash,
            i64::try_from(part.size).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .expect("upsert part");
}

/// A one-part session of exactly `data` on `file_id` (the part is uploaded, nothing completed).
pub async fn session_with_part(
    msvc: &MultipartService,
    store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: &'static [u8],
    auto_bind: bool,
) -> MultipartPlan {
    let plan = msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            data.len() as u64,
            None,
            auto_bind,
        )
        .await
        .expect("initiate");
    put_part(store, backend, &plan, file_id, 1, Bytes::from_static(data)).await;
    plan
}

/// Test-only unwrap of a complete outcome, for single-completer tests.
pub trait CompleteOutcomeExt {
    /// Unwrap `Completed`.
    ///
    /// # Panics
    /// Panics when the outcome is `Completing`.
    fn unwrap_completed(self) -> CompletedMultipartUpload;
}

impl CompleteOutcomeExt for MultipartCompleteOutcome {
    fn unwrap_completed(self) -> CompletedMultipartUpload {
        match self {
            Self::Completed(c) => c,
            Self::Completing { .. } => {
                panic!("complete is still in progress (completion lease held elsewhere)")
            }
        }
    }
}
