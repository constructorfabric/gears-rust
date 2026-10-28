//! Scaffolding shared by the integration tests: temp-file `SQLite`, a [`StackBuilder`] that wires a
//! `FileService`, `MultipartService`, `PolicyService`, `DataPlaneService` and `CleanupEngine`
//! over one store, and small request/part/claims/SQL helpers.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::response::Response;
use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer, actions};
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{
    CompletedMultipartUpload, MultipartCompleteOutcome, MultipartPlan,
};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, MultipartCompletionPart, StorageBackend,
};
use file_storage::infra::content::hash_mode::Manifest;
use file_storage::infra::external_clients::{QuotaClient, UsageReporter};
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{ByteRange, NewFile, OwnerKind};

pub const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file `SQLite` DB (a bare `sqlite::memory:` gives each pooled connection its own DB),
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

pub async fn build_db(tag: &str) -> Arc<DBProvider<DbError>> {
    build_db_with_dsn(tag).await.0
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

/// A `Uuid` column value as a `SQLite` BLOB literal (`Uuid` columns are 16-byte BLOBs).
pub fn uuid_blob(id: Uuid) -> String {
    format!("X'{}'", hex_encode(id.as_bytes()))
}

/// Number of rows in `files`, read through a second connection to `dsn`.
pub async fn count_files_rows(dsn: &str) -> i64 {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS c FROM files".to_owned(),
        ))
        .await
        .expect("count query")
        .expect("one row");
    row.try_get::<i64>("", "c").expect("i64 column c")
}

/// Grants every action inside the caller's tenant, except as configured:
/// - [`ScopedTestAuthorizer::admin_gated`]: `ADMIN_POLICY` is granted only after `set_admin(true)`;
/// - `deny_write_for_file` / `deny_write_for_subject`: `WRITE` is denied for that file / subject.
#[derive(Default)]
pub struct ScopedTestAuthorizer {
    admin_gated: bool,
    is_admin: AtomicBool,
    deny_write_for_file: Mutex<Option<Uuid>>,
    deny_write_for_subject: Mutex<Option<Uuid>>,
}

impl ScopedTestAuthorizer {
    /// Grants every action within the caller's tenant.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Like [`Self::new`], but `ADMIN_POLICY` needs `set_admin(true)`.
    #[must_use]
    pub fn admin_gated() -> Self {
        Self {
            admin_gated: true,
            ..Self::default()
        }
    }

    pub fn set_admin(&self, admin: bool) {
        self.is_admin.store(admin, Ordering::SeqCst);
    }

    /// Mark `file_id` as `WRITE`-denied.
    pub fn deny_write_for_file(&self, file_id: Uuid) {
        *self.deny_write_for_file.lock().expect("lock poisoned") = Some(file_id);
    }

    /// Mark `subject_id` as `WRITE`-denied.
    pub fn deny_write_for_subject(&self, subject_id: Uuid) {
        *self.deny_write_for_subject.lock().expect("lock poisoned") = Some(subject_id);
    }
}

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if self.admin_gated && action == actions::ADMIN_POLICY {
            return if self.is_admin.load(Ordering::SeqCst) {
                Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
            } else {
                Err(DomainError::Forbidden)
            };
        }

        if action == actions::WRITE {
            if let Some(denied) = *self.deny_write_for_file.lock().expect("lock poisoned")
                && Some(denied) == file_id
            {
                return Err(DomainError::Forbidden);
            }
            if let Some(denied) = *self.deny_write_for_subject.lock().expect("lock poisoned")
                && denied == ctx.subject_id()
            {
                return Err(DomainError::Forbidden);
            }
        }

        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

/// Backend decorator counting calls; every method delegates to `inner`.
/// - `reads`: whole-object reads (`get` / `get_stream`); a bounded `get_range` is not one;
/// - `complete_calls`: `complete_multipart` calls.
pub struct CountingBackend {
    inner: Arc<dyn StorageBackend>,
    pub reads: AtomicUsize,
    pub complete_calls: AtomicUsize,
}

impl CountingBackend {
    pub fn new(inner: Arc<dyn StorageBackend>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            reads: AtomicUsize::new(0),
            complete_calls: AtomicUsize::new(0),
        })
    }

    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    pub fn complete_calls(&self) -> usize {
        self.complete_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl StorageBackend for CountingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.inner.put(path, bytes).await
    }
    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(path).await
    }
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<futures::stream::BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get_stream(path).await
    }
    async fn get_range(&self, path: &str, range: ByteRange) -> Result<Bytes, DomainError> {
        self.inner.get_range(path, range).await
    }
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        self.inner.size(path).await
    }
    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }
    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }
    async fn initiate_multipart(&self, path: &str) -> Result<String, DomainError> {
        self.inner.initiate_multipart(path).await
    }
    async fn upload_part(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        self.inner
            .upload_part(path, upload_handle, part_number, part_offset, data)
            .await
    }
    async fn complete_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[MultipartCompletionPart],
    ) -> Result<(Manifest, [u8; 32]), DomainError> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .complete_multipart(path, upload_handle, parts)
            .await
    }
    async fn abort_multipart(&self, path: &str, upload_handle: &str) -> Result<(), DomainError> {
        self.inner.abort_multipart(path, upload_handle).await
    }
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        self.inner.list_paths().await
    }
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

impl Stack {
    /// A `MultipartService` over a caller-supplied store (e.g. a fault-injecting wrapper of
    /// `multipart_store`), sharing this stack's backends, authorizer and issuer.
    pub fn multipart_service_over(&self, store: Arc<dyn MultipartStore>) -> MultipartService {
        MultipartService::new(
            store,
            self.backends.clone(),
            Arc::clone(&self.authorizer),
            None,
            Arc::clone(&self.issuer),
            self.cfg.sidecar_base_url.clone(),
            self.cfg.default_url_ttl_secs,
        )
    }

    /// A `CleanupEngine` over a caller-supplied store, sharing this stack's backends.
    pub fn cleanup_engine_over(
        &self,
        store: Arc<dyn CleanupStore>,
        grace_secs: u64,
    ) -> CleanupEngine {
        CleanupEngine::new(
            store,
            self.backends.clone(),
            CleanupConfig {
                orphan_grace_secs: grace_secs,
            },
        )
    }
}

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

    /// Reuse an already migrated DB (and its DSN) instead of creating a fresh one.
    pub fn db(mut self, db: Arc<DBProvider<DbError>>, dsn: &str) -> Self {
        self.db = Some((db, dsn.to_owned()));
        self
    }

    /// A single backend, which is also the registry default.
    pub fn backend(self, backend: Arc<dyn StorageBackend>) -> Self {
        let id = backend.id().to_owned();
        self.backends(vec![backend], &id)
    }

    /// The backend set and the registry's default backend id; `Stack::backend` is the first one.
    pub fn backends(mut self, backends: Vec<Arc<dyn StorageBackend>>, default_id: &str) -> Self {
        self.backends = Some((backends, default_id.to_owned()));
        self
    }

    pub fn authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = Some(authorizer);
        self
    }

    /// Quota client for both services (single-part create and multipart plan checks).
    pub fn quota(mut self, quota: Arc<dyn QuotaClient>) -> Self {
        self.quota = Some(quota);
        self
    }

    /// Usage reporter shared by `FileService`, `MultipartService` and `CleanupEngine`.
    pub fn usage_reporter(mut self, reporter: Arc<dyn UsageReporter>) -> Self {
        self.usage_reporter = Some(reporter);
        self
    }

    /// Share one `Issuer` (so tests can mint tokens the services' verifier accepts).
    pub fn issuer(mut self, issuer: Arc<Issuer>) -> Self {
        self.issuer = Some(issuer);
        self
    }

    /// The whole `ServiceConfig`; `default_url_ttl_secs` / `sidecar_base_url` also drive the
    /// `MultipartService`, the page sizes also drive the `PolicyService`.
    pub fn config(mut self, cfg: ServiceConfig) -> Self {
        self.cfg = cfg;
        self
    }

    pub fn page_sizes(mut self, default_page_size: u64, max_page_size: u64) -> Self {
        self.cfg.default_page_size = default_page_size;
        self.cfg.max_page_size = max_page_size;
        self
    }

    pub fn idempotency_ttl_secs(mut self, secs: u64) -> Self {
        self.cfg.idempotency_ttl_secs = secs;
        self
    }

    pub fn url_ttl_secs(mut self, secs: i64) -> Self {
        self.cfg.default_url_ttl_secs = secs;
        self
    }

    /// `CleanupEngine` orphan grace window (default one day).
    pub fn grace_secs(mut self, secs: u64) -> Self {
        self.grace_secs = secs;
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

/// Two in-memory backends, `"mem"` (the registry default) and `"alt"`.
pub fn mem_and_alt_backends() -> (Arc<dyn StorageBackend>, Arc<dyn StorageBackend>) {
    (
        Arc::new(InMemoryBackend::new("mem")),
        Arc::new(InMemoryBackend::new("alt")),
    )
}

/// A multipart upload opened straight on a backend, for a version that has no row.
pub struct RawUpload {
    pub version_id: Uuid,
    pub backend_path: String,
    pub handle: String,
}

/// Opens a multipart upload for `file_id` on `backend` under a fresh version id.
pub async fn start_raw_upload(backend: &Arc<dyn StorageBackend>, file_id: Uuid) -> RawUpload {
    let version_id = Uuid::now_v7();
    let backend_path = backend_path(file_id, version_id);
    let handle = backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate multipart on the backend");
    RawUpload {
        version_id,
        backend_path,
        handle,
    }
}

/// Asserts the upload's handle was aborted: a live handle would still accept another part.
pub async fn assert_raw_upload_aborted(backend: &Arc<dyn StorageBackend>, upload: &RawUpload) {
    let after = backend
        .upload_part(
            &upload.backend_path,
            &upload.handle,
            1,
            0,
            Bytes::from_static(b"x"),
        )
        .await;
    assert!(
        after.is_err(),
        "the multipart handle on backend `{}` must have been aborted, but it is still live: \
         {after:?}",
        backend.id()
    );
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
