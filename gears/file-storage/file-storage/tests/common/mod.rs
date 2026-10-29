//! Scaffolding shared by the integration tests: temp-file `SQLite`, a [`StackBuilder`] that wires a
//! `FileService`, `MultipartService`, `PolicyService`, `DataPlaneService` and `CleanupEngine`
//! over one store, and small request/part/claims/SQL helpers.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::routing::post;
use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer, actions};
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{
    CompletedMultipartUpload, DEFAULT_MIN_PART_SIZE, MultipartCompleteOutcome, MultipartPlan,
};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig, UploadTicket};
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, MultipartCompletionPart, StorageBackend,
};
use file_storage::infra::content::hash_mode::{HashMode, Manifest};
use file_storage::infra::external_clients::{QuotaClient, UsageDelta, UsageReporter};
use file_storage::infra::signed_url::{
    Claims, Issuer, MultipartClaims, Op, UploadConstraints, Verifier,
};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{ByteRange, File, FileVersion, NewFile, OwnerKind, VersionStatus};

/// Declared size that plans to exactly 3 parts at the default min part size (two full parts and
/// a 3-byte remainder).
pub const MULTI_PART_DECLARED_SIZE: u64 = 2 * DEFAULT_MIN_PART_SIZE + 3;

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

fn base_config() -> ServiceConfig {
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
fn hex_encode(bytes: &[u8]) -> String {
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

/// Runs one raw statement against a second connection to `dsn`; returns the rows affected.
pub async fn raw_exec(dsn: &str, sql: &str) -> u64 {
    let conn = Database::connect(dsn).await.expect("raw connect");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"))
    .rows_affected()
}

/// [`raw_exec`] that asserts the statement hit exactly the one row the test prepared: a silent
/// no-op tamper would make the rest of the test meaningless.
pub async fn exec_expect_one_row(dsn: &str, label: &str, sql: &str) {
    assert_eq!(
        raw_exec(dsn, sql).await,
        1,
        "tamper `{label}` must hit exactly the one row the test prepared"
    );
}

/// Number of rows in `table`, read through a second connection to `dsn`.
pub async fn raw_count(dsn: &str, table: &str) -> i64 {
    let conn = Database::connect(dsn).await.expect("raw connect");
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        format!("SELECT COUNT(*) AS c FROM {table}"),
    ))
    .await
    .expect("count query")
    .expect("one row")
    .try_get::<i64>("", "c")
    .expect("i64 column c")
}

/// The `fs-token` query value of a signed upload URL.
pub fn token_of(url: &str) -> &str {
    &url[url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len()..]
}

/// A semantically valid retention-rule body, for tests focused on something else.
pub fn valid_rule_body() -> RetentionRuleBody {
    RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 30 }),
        inactivity: None,
        metadata: None,
    }
}

/// A `files` row for `(file_id, tenant_id)`: random user owner, unbound, created now. Override
/// fields with struct-update syntax.
pub fn file_row(file_id: Uuid, tenant_id: Uuid) -> File {
    let now = time::OffsetDateTime::now_utc();
    File {
        file_id,
        tenant_id,
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        content_id: None,
        meta_version: 0,
        created_at: now,
        last_modified_at: now,
    }
}

/// An `Available`, non-current whole-sha256 `file_versions` row on the `mem` backend. Override
/// fields with struct-update syntax.
pub fn version_row(file_id: Uuid, version_id: Uuid) -> FileVersion {
    FileVersion {
        file_id,
        version_id,
        mime_type: "text/plain".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: HashMode::WholeSha256.as_str().to_owned(),
        part_count: None,
        status: VersionStatus::Available,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(file_id, version_id),
        created_at: time::OffsetDateTime::now_utc(),
        bound_on_finalize: false,
    }
}

/// Seeds one file for `owner_id` per entry of `instants`, each with that `created_at` (ties
/// included); returns the file ids in the order given.
pub async fn seed_files(
    db: &Arc<DBProvider<DbError>>,
    tenant: Uuid,
    owner_id: Uuid,
    instants: &[time::OffsetDateTime],
) -> Vec<Uuid> {
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let mut ids = Vec::with_capacity(instants.len());
    for &created_at in instants {
        let file = File {
            owner_id,
            created_at,
            last_modified_at: created_at,
            ..file_row(Uuid::now_v7(), tenant)
        };
        FileRepo::new()
            .create(&conn, &scope, &file)
            .await
            .expect("create file");
        ids.push(file.file_id);
    }
    ids
}

/// Seeds one file and one non-current whole-sha256 version per entry of `instants` (each with that
/// `created_at`, ties included); returns the file id and the version ids in the order given.
pub async fn seed_file_with_versions(
    db: &Arc<DBProvider<DbError>>,
    tenant: Uuid,
    owner_id: Uuid,
    instants: &[time::OffsetDateTime],
) -> (Uuid, Vec<Uuid>) {
    let file_id = seed_files(db, tenant, owner_id, &[time::OffsetDateTime::now_utc()]).await[0];
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let mut ids = Vec::with_capacity(instants.len());
    for &created_at in instants {
        let version = FileVersion {
            created_at,
            ..version_row(file_id, Uuid::now_v7())
        };
        VersionRepo::new()
            .insert(&conn, &scope, &version)
            .await
            .expect("insert version");
        ids.push(version.version_id);
    }
    (file_id, ids)
}

/// Inserts a two-part `multipart-composite-sha256` version of `file_id` with `manifest_text` as its
/// manifest row; returns its id.
pub async fn insert_composite_version(
    db: &Arc<DBProvider<DbError>>,
    file_id: Uuid,
    created_at: time::OffsetDateTime,
    manifest_text: &str,
) -> Uuid {
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let version = FileVersion {
        created_at,
        hash_mode: HashMode::MultipartCompositeSha256.as_str().to_owned(),
        part_count: Some(2),
        ..version_row(file_id, Uuid::now_v7())
    };
    let versions = VersionRepo::new();
    versions
        .insert(&conn, &scope, &version)
        .await
        .expect("insert composite version");
    versions
        .insert_manifest(&conn, &scope, version.version_id, manifest_text, created_at)
        .await
        .expect("insert manifest");
    version.version_id
}

/// A body-less `GET` request.
pub fn get_req(uri: impl AsRef<str>) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri.as_ref())
        .body(Body::empty())
        .expect("build request")
}

/// Asserts `resp` is an RFC 9457 problem document with HTTP `status` (media type
/// `application/problem+json`, `status`/`type`/`title` fields) and returns its body.
pub async fn problem_body(resp: Response, status: u16) -> Value {
    assert_eq!(resp.status().as_u16(), status);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("application/problem+json"),
        "content-type was `{content_type}`"
    );
    let body = body_json(resp).await;
    assert_eq!(body["status"], status, "{body}");
    for field in ["type", "title"] {
        assert!(
            body[field].as_str().is_some_and(|s| !s.is_empty()),
            "problem `{field}` must be a non-empty string: {body}"
        );
    }
    body
}

/// `(field, reason)` of every violation of a canonical problem body, in order.
pub fn violations(body: &Value) -> Vec<(String, String)> {
    body["context"]["field_violations"]
        .as_array()
        .unwrap_or_else(|| panic!("no field_violations in {body}"))
        .iter()
        .map(|v| {
            (
                v["field"].as_str().expect("field").to_owned(),
                v["reason"].as_str().expect("reason").to_owned(),
            )
        })
        .collect()
}

/// Creates a file, uploads `data` through the data plane and binds it.
pub async fn bound_file(
    svc: &FileService,
    dp: &DataPlaneService,
    ctx: &SecurityContext,
    data: &'static [u8],
) -> UploadTicket {
    let ticket = svc
        .create_file(ctx, new_file_of("test.txt", "text/plain"), None, false)
        .await
        .unwrap();
    dp.put_content(
        ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(data),
    )
    .await
    .unwrap();
    svc.bind(ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();
    ticket
}

/// The file's current `content_id` (the bound version), read straight from the store.
pub async fn content_id_of(store: &Store, file_id: Uuid) -> Option<Uuid> {
    store
        .get_file(&AccessScope::allow_all(), file_id)
        .await
        .expect("get_file")
        .expect("file")
        .content_id
}

/// Plants a completion lease held by `owner` that expires `secs_from_now` seconds from now
/// (negative = already expired: a completer that died mid-assembly).
pub async fn plant_lease(
    multipart_store: &Arc<dyn MultipartStore>,
    upload_id: Uuid,
    owner: &str,
    secs_from_now: i64,
) {
    let now = time::OffsetDateTime::now_utc();
    let acquired = multipart_store
        .acquire_multipart_complete_lease(
            upload_id,
            owner,
            now + time::Duration::seconds(secs_from_now),
            now,
        )
        .await
        .expect("acquire lease");
    assert!(acquired, "test setup: the lease must attach to the session");
}

/// The `report_multipart_part` callback behind the internal secret [`REPORT_SECRET`].
pub fn report_part_router(verifier: Arc<Verifier>, msvc: Arc<MultipartService>) -> Router {
    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(
        REPORT_SECRET.to_owned(),
        time::Duration::ZERO,
    ));
    Router::new()
        .route(
            "/api/file-storage/v1/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report",
            post(handlers::report_multipart_part),
        )
        .layer(axum::Extension(verifier))
        .layer(axum::Extension(finalize_auth))
        .layer(axum::Extension(msvc))
}

const REPORT_SECRET: &str = "test-internal-secret";

/// A `report_multipart_part` request for `part_number` of `plan`, authenticated with `token`.
pub fn report_part_request(
    file_id: Uuid,
    plan: &MultipartPlan,
    part_number: u32,
    token: &str,
    body: &Value,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!(
            "/api/file-storage/v1/files/{file_id}/versions/{}/multipart/{}/parts/{part_number}/report",
            plan.version_id, plan.upload_id
        ))
        .header("content-type", "application/json")
        .header("x-fs-token", token)
        .header("x-fs-internal-token", REPORT_SECRET)
        .body(Body::from(serde_json::to_vec(body).expect("serialize body")))
        .expect("build request")
}

/// The authz-focused stack: [`ScopedTestAuthorizer`] (optionally `ADMIN_POLICY`-gated) over one
/// fresh store.
pub struct AuthzHarness {
    pub file_svc: Arc<FileService>,
    pub policy_svc: Arc<PolicyService>,
    pub policy_store: Arc<dyn PolicyStore>,
    pub authz: Arc<ScopedTestAuthorizer>,
    /// For direct row edits that simulate stored state drifting.
    pub db: Arc<DBProvider<DbError>>,
}

pub async fn authz_harness(tag: &str, admin_gated: bool) -> AuthzHarness {
    let authz = Arc::new(if admin_gated {
        ScopedTestAuthorizer::admin_gated()
    } else {
        ScopedTestAuthorizer::new()
    });
    let s = StackBuilder::new(tag)
        .authorizer(Arc::clone(&authz) as Arc<dyn Authorizer>)
        .build()
        .await;
    AuthzHarness {
        file_svc: s.svc,
        policy_svc: s.psvc,
        policy_store: Arc::new(s.store),
        authz,
        db: s.db,
    }
}

/// A `NewFile` owned by `owner_id`, for the authz tests.
pub fn owned_file(owner_id: Uuid) -> NewFile {
    new_file_for(owner_id, "victim.bin", "application/octet-stream")
}

/// Capturing `UsageReporter`. Production reports from a spawned task, so a test awaits the
/// expected number of reports on a channel instead of sleeping.
pub struct FakeUsageReporter {
    tx: tokio::sync::mpsc::UnboundedSender<UsageDelta>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<UsageDelta>>,
    seen: tokio::sync::Mutex<Vec<UsageDelta>>,
}

impl FakeUsageReporter {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            tx,
            rx: tokio::sync::Mutex::new(rx),
            seen: tokio::sync::Mutex::default(),
        })
    }

    /// Waits until at least `at_least` deltas have been reported in total, then yields once so
    /// the other already spawned report tasks land too, and returns every delta in arrival order.
    /// `reports(0)` is the "nothing (more) was reported" check.
    pub async fn reports(&self, at_least: usize) -> Vec<UsageDelta> {
        let mut rx = self.rx.lock().await;
        let mut seen = self.seen.lock().await;
        while seen.len() < at_least {
            seen.push(rx.recv().await.expect("reporter channel closed"));
        }
        tokio::task::yield_now().await;
        while let Ok(delta) = rx.try_recv() {
            seen.push(delta);
        }
        seen.clone()
    }
}

#[async_trait]
impl UsageReporter for FakeUsageReporter {
    async fn report(&self, delta: UsageDelta) {
        self.tx.send(delta).expect("reporter channel open");
    }
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

    /// Like [`Self::new`], over two in-memory backends: `"mem"` (the default) and `"alt"`.
    pub fn dual_backend(tag: &str) -> Self {
        let (mem, alt) = mem_and_alt_backends();
        Self::new(tag).backends(vec![mem, alt], "mem")
    }

    /// Reuse an already migrated DB (and its DSN) instead of creating a fresh one.
    pub fn db(mut self, db: Arc<DBProvider<DbError>>, dsn: &str) -> Self {
        self.db = Some((db, dsn.to_owned()));
        self
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

    pub fn page_sizes(mut self, default_page_size: u64, max_page_size: u64) -> Self {
        self.cfg.default_page_size = default_page_size;
        self.cfg.max_page_size = max_page_size;
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
    let backend_path = backend_path(file_id, plan.version_id);
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

/// A new file with a two-part session (a 5 MiB part and a 4 KiB tail) whose parts are uploaded but
/// not completed. Returns `(file_id, plan, full content)`.
pub async fn composite_session(s: &Stack, ctx: &SecurityContext) -> (Uuid, MultipartPlan, Vec<u8>) {
    let file_id = s
        .svc
        .create_file(ctx, new_file("upload.bin"), None, false)
        .await
        .expect("create file")
        .file_id;
    let parts = [
        Bytes::from(vec![b'a'; 5 * 1024 * 1024]),
        Bytes::from(vec![b'c'; 4096]),
    ];
    let full: Vec<u8> = parts.iter().flat_map(|p| p.iter().copied()).collect();
    let plan = s
        .msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            full.len() as u64,
            None,
            false,
        )
        .await
        .expect("initiate");
    assert_eq!(plan.parts.len(), 2, "test setup: a two-part plan");
    for (part, data) in plan.parts.iter().zip(parts) {
        put_part(
            &s.multipart_store,
            &s.backend,
            &plan,
            file_id,
            part.part_number,
            data,
        )
        .await;
    }
    (file_id, plan, full)
}

/// A file for `ctx` with no content yet (one pending version); returns its id.
pub async fn bare_file(svc: &FileService, ctx: &SecurityContext) -> Uuid {
    svc.create_file(ctx, new_file("upload.bin"), None, false)
        .await
        .expect("create file")
        .file_id
}

/// A manual-bind session of `size` bytes on `file_id` with the default part size.
pub async fn initiate(
    msvc: &MultipartService,
    ctx: &SecurityContext,
    file_id: Uuid,
    size: u64,
) -> MultipartPlan {
    msvc.initiate_multipart_upload(ctx, file_id, "application/octet-stream", size, None, false)
        .await
        .expect("initiate")
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
