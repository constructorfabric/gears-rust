//! Integration tests for the P2-M4 lifecycle & cleanup engine.
//!
//! Tests cover:
//! 1. Abandoned pending version sweep — a never-finalised pending version is
//!    deleted when its `created_at` is older than the grace cutoff.
//! 2. Expired multipart session sweep — a session past its `expires_at` is
//!    marked `aborted`.
//! 3. Retention-policy expiry sweep — a file with a tenant rule (max_age_days = 0)
//!    is deleted and a `retention_delete` audit row is written.
//! 4. Backend migration (`migrate_backend`) — happy path and rejection of
//!    versioned files.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::MultipartUploadSession;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{
    AgeRetention, RetentionRuleBody, RetentionScope, StoredRetentionRule,
};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::content::{hash, mime};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.cleanup_test.file.type.v1~");

/// Direct byte-path test double for what `DataPlaneService` used to provide
/// (removed: production never constructed it — the sidecar is the only real
/// byte path). Same steps `DataPlaneService::put_content` used to perform,
/// but through `put_stream` rather than the whole-object `put` that no
/// longer exists on `StorageBackend`.
struct TestDataPlane {
    svc: Arc<FileService>,
    store: Store,
    backends: BackendRegistry,
}

impl TestDataPlane {
    fn new(svc: Arc<FileService>, store: Store, backends: BackendRegistry) -> Self {
        Self {
            svc,
            store,
            backends,
        }
    }

    async fn put_content(
        &self,
        ctx: &SecurityContext,
        file_id: Uuid,
        version_id: Uuid,
        declared_mime: &str,
        bytes: Bytes,
    ) -> Result<(), DomainError> {
        mime::validate(declared_mime, &bytes)?;
        self.svc.authorize_write(ctx, file_id).await?;
        let version = self
            .store
            .get_version(file_id, version_id)
            .await?
            .ok_or_else(|| DomainError::version_not_found(file_id, version_id))?;
        let backend = self.backends.get(&version.backend_id)?;
        let len = bytes.len() as u64;
        let digest = hash::sha256(&bytes);
        let stream: futures::stream::BoxStream<'static, std::io::Result<Bytes>> =
            Box::pin(futures::stream::once(async move { Ok(bytes) }));
        backend
            .put_stream(&version.backend_path, stream, Some(len))
            .await?;
        self.svc
            .finalize_upload(
                ctx,
                file_id,
                version_id,
                i64::try_from(len).unwrap_or(i64::MAX),
                digest,
            )
            .await
    }
}

/// Write the whole of `bytes` to `path` via `put_stream` (a one-shot
/// stream) -- the test-only stand-in for the whole-object `put` the trait no
/// longer has.
async fn write_all(backend: &Arc<dyn StorageBackend>, path: &str, bytes: Bytes) {
    let len = bytes.len() as u64;
    let stream: futures::stream::BoxStream<'static, std::io::Result<Bytes>> =
        Box::pin(futures::stream::once(async move { Ok(bytes) }));
    backend
        .put_stream(path, stream, Some(len))
        .await
        .expect("put_stream");
}

/// Read the whole blob at `path` back via `get_stream` (collecting every
/// chunk) -- the test-only stand-in for the whole-object `get` the trait no
/// longer has.
async fn read_all(backend: &Arc<dyn StorageBackend>, path: &str, expected_len: u64) -> Bytes {
    use futures::StreamExt;

    let mut stream = backend
        .get_stream(path, expected_len)
        .await
        .expect("get_stream");
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.expect("chunk"));
    }
    Bytes::from(buf)
}

// ── test harness ──────────────────────────────────────────────────────────────

async fn build_db() -> Arc<DBProvider<DbError>> {
    build_db_with_dsn().await.0
}

/// Like [`build_db`], but also returns the sqlite DSN -- needed by tests that
/// open a second, independent raw `sea_orm` connection (via
/// `Database::connect(&dsn)`, the pattern `tests/multipart_test.rs`'s
/// `count_files_rows`/`tamper_request_hash` and
/// `tests/content_hash_modes_test.rs` already use) to assert on rows the
/// `Store`/`CleanupStore` API surface has no getter for, e.g. a raw
/// `idempotency_keys` row count.
async fn build_db_with_dsn() -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-cleanup-test-{}.db", Uuid::now_v7().simple()));
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

/// Build a service + cleanup engine sharing the same Store and BackendRegistry.
/// `grace_secs = 0` means every pending version is immediately eligible for sweep.
async fn build_all(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    Arc<MultipartService>,
    TestDataPlane,
    Store,
    CleanupEngine,
    Arc<dyn StorageBackend>,
) {
    let (svc, psvc, msvc, dp, store, engine, backend, _db) = build_all_full(grace_secs).await;
    (svc, psvc, msvc, dp, store, engine, backend)
}

/// Like [`build_all`], but also returns the raw `DBProvider` handle -- for
/// tests that need both `DataPlaneService` (to `put_content`) AND direct
/// entity-layer access (to backdate a `file_versions.created_at`/
/// `files.created_at` row past the sweep's grace cutoff; see
/// [`backdate_version_created_at`]).
async fn build_all_full(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    Arc<MultipartService>,
    TestDataPlane,
    Store,
    CleanupEngine,
    Arc<dyn StorageBackend>,
    Arc<DBProvider<DbError>>,
) {
    let db = build_db().await;

    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    // Upcast to narrow capability traits.
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        multipart_store,
        backends.clone(),
        Arc::clone(&authorizer),
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, psvc, msvc, dp, store, engine, backend, db)
}

/// Backdate a `file_versions.created_at` row directly through the entity
/// layer -- there is no public API to backdate an already-created version
/// row (same mechanism `sweep_deletes_versionless_file_past_grace` and
/// `sweep_aborts_expired_completing_session` already use for `files.created_at`
/// / `multipart_uploads`). Used to make a pending version unambiguously
/// older than the sweep's `grace_cutoff` by a wide, explicit margin, instead
/// of relying on `orphan_grace_secs = 0` and a freshly-inserted row racing an
/// exactly-equal-instant comparison against `now()` in the strict
/// `created_at < cutoff` sweep query -- that race is real: on a slow/loaded
/// CI runner (or a DB column that truncates timestamp precision), the two
/// `now()` calls a few instructions apart can land on the same stored
/// instant, and the row is then (correctly, per the strict `<`) not yet
/// eligible, making the sweep-deletes-it assertion flaky.
async fn backdate_version_created_at(
    db: &DBProvider<DbError>,
    version_id: Uuid,
    when: time::OffsetDateTime,
) {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as VersionColumn, Entity as VersionEntity,
    };

    let conn = db.conn().expect("conn");
    VersionEntity::update_many()
        .col_expr(VersionColumn::CreatedAt, Expr::value(when))
        .filter(VersionColumn::VersionId.eq(version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate file_versions.created_at");
}

/// Like [`build_all`], but also returns the sqlite DSN (see
/// [`build_db_with_dsn`]) -- used by the idempotency-keys cascade-delete
/// test below, which asserts on a raw row count `Store`/`CleanupStore` has no
/// getter for.
async fn build_all_with_dsn(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    Arc<MultipartService>,
    TestDataPlane,
    Store,
    CleanupEngine,
    Arc<dyn StorageBackend>,
    String,
) {
    let (db, dsn) = build_db_with_dsn().await;

    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        multipart_store,
        backends.clone(),
        Arc::clone(&authorizer),
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, psvc, msvc, dp, store, engine, backend, dsn)
}

/// Like [`build_all`], but also returns the raw `DBProvider` handle. Used by
/// the P2 2.8 live-multipart-session-guard tests below, which need to
/// backdate a `file_versions.created_at` / `multipart_uploads.expires_at`
/// value directly through the entity layer -- there is no public API to
/// backdate either column on an already-created row.
async fn build_all_with_db(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Store,
    CleanupEngine,
    Arc<DBProvider<DbError>>,
    Arc<dyn StorageBackend>,
) {
    let db = build_db().await;

    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, msvc, store, engine, db, backend)
}

/// Build a service + cleanup engine with TWO in-memory backends ("mem" and "alt").
async fn build_all_dual_backend(
    grace_secs: u64,
) -> (Arc<FileService>, TestDataPlane, Store, CleanupEngine) {
    let db = build_db().await;

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer = Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let sweep_backends = backends.clone();

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, dp, store, engine)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file() -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "test.txt".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "text/plain".to_owned(),
        custom_metadata: vec![],
    }
}

/// Box `data` into the one-shot `BoxStream` shape `upload_part_stream` now
/// expects, alongside its exact length. Every call site in this file already
/// builds its part bytes fully in memory (small fixed test payloads), so a
/// one-shot `stream::once` is enough.
fn one_shot_part_stream(
    data: Bytes,
) -> (
    futures::stream::BoxStream<'static, std::io::Result<Bytes>>,
    u64,
) {
    let len = data.len() as u64;
    (
        Box::pin(futures::stream::once(async move { Ok(data) })),
        len,
    )
}

/// [`new_file`] with the owner set explicitly, for tests whose authorizer
/// denies `ADMIN_POLICY`.
///
/// `new_file`'s `owner_id` is a fresh UUID that never equals the caller's
/// own `subject_id`, so every file it builds is owned by *another* subject.
/// `FileService::create_file` now requires `ADMIN_POLICY` for exactly that
/// (creating a file under a foreign `owner_id` picks that owner's effective
/// policy and debits that owner's quota), which an admin-denying authorizer
/// refuses -- unrelated to whatever such a test is actually asserting.
/// Passing the caller's own subject id keeps the creation self-owned, the
/// realistic shape for a non-admin caller.
fn new_file_owned_by(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_id,
        ..new_file()
    }
}

/// A [`CleanupStore`] wrapper that makes the version-collecting file delete
/// fail for one specific `file_id` while delegating every other method to a
/// real [`Store`]. `CleanupStore` is a narrow trait, so this is a small
/// hand-written newtype rather than a mocking-framework fake (same shape as
/// `enforce_test.rs`'s `ErroringQuota`/`CappedQuota`).
///
/// Used to prove (P2 remediation 0.6) that a transient version-listing
/// failure during the retention sweep aborts that file's expiry instead of
/// being swallowed as "zero versions" and deleting the file anyway. The
/// fault sits on `delete_file_with_event_collecting_versions` (not a
/// standalone `list_versions` call) because `expire_file` reads a file's
/// versions **inside** that method's own transaction now, immediately
/// before the delete -- see `Store::delete_file_collecting_versions`'s doc
/// comment.
struct FaultyListVersionsStore {
    inner: Store,
    fault_file_id: Uuid,
}

#[async_trait]
impl CleanupStore for FaultyListVersionsStore {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: time::OffsetDateTime,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        self.inner
            .list_abandoned_pending_versions(older_than, now, limit)
            .await
    }

    async fn list_versionless_orphan_files(
        &self,
        created_before: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner
            .list_versionless_orphan_files(created_before, limit)
            .await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.delete_version(file_id, version_id, audit).await
    }

    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_pending_version(file_id, version_id, audit)
            .await
    }

    async fn list_expired_multipart_uploads(
        &self,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        self.inner.list_expired_multipart_uploads(now, limit).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.abort_multipart_upload(upload_id, audit).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        self.inner.get_version(file_id, version_id).await
    }

    async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError> {
        self.inner.list_all_retention_rules().await
    }

    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner.list_all_files_for_sweep(after, limit).await
    }

    async fn list_metadata(&self, file_id: Uuid) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        self.inner.list_metadata(file_id).await
    }

    async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        self.inner.list_metadata_for_files(file_ids).await
    }

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        self.inner.list_versions(file_id).await
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        self.inner
            .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
            .await
    }

    async fn list_files_by_ids(&self, ids: &[Uuid]) -> Result<Vec<File>, DomainError> {
        self.inner
            .list_files_by_ids(&toolkit_security::AccessScope::allow_all(), ids)
            .await
    }

    async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_active_multipart_for_file(file_id).await
    }

    /// The one faulted method: errors for `fault_file_id`, delegates
    /// otherwise. This is where `expire_file` now reads a file's versions
    /// (inside the same transaction as its delete), so this is the method a
    /// transient version-listing failure surfaces through -- see this
    /// struct's own doc comment.
    async fn delete_file_with_event_collecting_versions(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<file_storage::domain::ports::DeletedFile, DomainError> {
        if file_id == self.fault_file_id {
            Err(DomainError::InternalError)
        } else {
            self.inner
                .delete_file_with_event_collecting_versions(scope, file_id, audit, event)
                .await
        }
    }

    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
    }

    async fn delete_expired_idempotency_keys(
        &self,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now, limit).await
    }
}

/// A [`CleanupStore`] wrapper that makes `list_files_by_ids` fail whenever
/// the requested batch includes one specific `file_id`, while delegating
/// every other method (including `get_file`, and `list_files_by_ids` for a
/// batch that does not mention `fault_file_id`) to a real [`Store`]. Same
/// shape as `FaultyListVersionsStore` above.
///
/// Used to prove that a transient batch-load failure during
/// `CleanupEngine::abort_expired_multipart_session`'s audit-tenant lookup
/// (`sweep_expired_multipart`'s `load_files_by_ids_for_audit` batch, run
/// before its loop) is logged (distinguishing it from a genuinely-absent
/// file) rather than silently folded into the same `Uuid::nil()` fallback
/// with no trace.
struct FaultyListFilesByIdsStore {
    inner: Store,
    fault_file_id: Uuid,
}

#[async_trait]
impl CleanupStore for FaultyListFilesByIdsStore {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: time::OffsetDateTime,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        self.inner
            .list_abandoned_pending_versions(older_than, now, limit)
            .await
    }

    async fn list_versionless_orphan_files(
        &self,
        created_before: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner
            .list_versionless_orphan_files(created_before, limit)
            .await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.delete_version(file_id, version_id, audit).await
    }

    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_pending_version(file_id, version_id, audit)
            .await
    }

    async fn list_expired_multipart_uploads(
        &self,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        self.inner.list_expired_multipart_uploads(now, limit).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.abort_multipart_upload(upload_id, audit).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        self.inner.get_version(file_id, version_id).await
    }

    async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError> {
        self.inner.list_all_retention_rules().await
    }

    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner.list_all_files_for_sweep(after, limit).await
    }

    async fn list_metadata(&self, file_id: Uuid) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        self.inner.list_metadata(file_id).await
    }

    async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        self.inner.list_metadata_for_files(file_ids).await
    }

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        self.inner.list_versions(file_id).await
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        self.inner
            .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
            .await
    }

    /// The one faulted method: errors whenever `ids` contains
    /// `fault_file_id`, delegates otherwise.
    async fn list_files_by_ids(&self, ids: &[Uuid]) -> Result<Vec<File>, DomainError> {
        if ids.contains(&self.fault_file_id) {
            Err(DomainError::InternalError)
        } else {
            self.inner
                .list_files_by_ids(&toolkit_security::AccessScope::allow_all(), ids)
                .await
        }
    }

    async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_active_multipart_for_file(file_id).await
    }

    async fn delete_file_with_event_collecting_versions(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<file_storage::domain::ports::DeletedFile, DomainError> {
        self.inner
            .delete_file_with_event_collecting_versions(scope, file_id, audit, event)
            .await
    }

    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
    }

    async fn delete_expired_idempotency_keys(
        &self,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now, limit).await
    }
}

/// A [`CleanupStore`] wrapper that counts calls to `get_file` and
/// `list_files_by_ids` (both delegated unchanged to a real [`Store`]) while
/// passing every other method straight through. Used to prove the N+1 fix:
/// a sweep batch of several candidates must resolve their audit-tenant files
/// via exactly one `list_files_by_ids` call, never a per-candidate `get_file`.
#[derive(Clone, Default)]
struct CountingCleanupStore {
    get_file_calls: Arc<std::sync::atomic::AtomicUsize>,
    list_files_by_ids_calls: Arc<std::sync::atomic::AtomicUsize>,
}

struct CountingCleanupStoreWrapper {
    inner: Store,
    counts: CountingCleanupStore,
}

#[async_trait]
impl CleanupStore for CountingCleanupStoreWrapper {
    async fn list_abandoned_pending_versions(
        &self,
        older_than: time::OffsetDateTime,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<FileVersion>, DomainError> {
        self.inner
            .list_abandoned_pending_versions(older_than, now, limit)
            .await
    }

    async fn list_versionless_orphan_files(
        &self,
        created_before: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner
            .list_versionless_orphan_files(created_before, limit)
            .await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.delete_version(file_id, version_id, audit).await
    }

    async fn delete_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_pending_version(file_id, version_id, audit)
            .await
    }

    async fn list_expired_multipart_uploads(
        &self,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        self.inner.list_expired_multipart_uploads(now, limit).await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.abort_multipart_upload(upload_id, audit).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        self.inner.get_version(file_id, version_id).await
    }

    async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError> {
        self.inner.list_all_retention_rules().await
    }

    async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        self.inner.list_all_files_for_sweep(after, limit).await
    }

    async fn list_metadata(&self, file_id: Uuid) -> Result<Vec<CustomMetadataEntry>, DomainError> {
        self.inner.list_metadata(file_id).await
    }

    async fn list_metadata_for_files(
        &self,
        file_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, Vec<CustomMetadataEntry>>, DomainError> {
        self.inner.list_metadata_for_files(file_ids).await
    }

    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        self.inner.list_versions(file_id).await
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        self.counts
            .get_file_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
            .await
    }

    async fn list_files_by_ids(&self, ids: &[Uuid]) -> Result<Vec<File>, DomainError> {
        self.counts
            .list_files_by_ids_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .list_files_by_ids(&toolkit_security::AccessScope::allow_all(), ids)
            .await
    }

    async fn has_active_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_active_multipart_for_file(file_id).await
    }

    async fn delete_file_with_event_collecting_versions(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<file_storage::domain::ports::DeletedFile, DomainError> {
        self.inner
            .delete_file_with_event_collecting_versions(scope, file_id, audit, event)
            .await
    }

    async fn delete_orphan_file_with_event(
        &self,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
    }

    async fn delete_expired_idempotency_keys(
        &self,
        now: time::OffsetDateTime,
        limit: u64,
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now, limit).await
    }
}

/// Minimal hand-rolled `tracing::Subscriber` that records every event's
/// formatted `message` field plus its level, so a test can assert a specific
/// log line was emitted without pulling in a log-capturing crate. Installed
/// per-test via `tracing::subscriber::set_default`, which is thread-local --
/// safe here because `#[tokio::test]` defaults to a current-thread runtime,
/// so the awaited call under test never hops to another OS thread.
#[derive(Clone, Default)]
struct RecordingSubscriber {
    messages: Arc<std::sync::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for RecordingSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct MessageVisitor(String);
        impl tracing::field::Visit for MessageVisitor {
            // The trait only ever hands back `&dyn Debug` (there is no
            // `Display`-based visitor method), so `{value:?}` here is the
            // API's own contract, not a debug-print left in by accident.
            #[allow(clippy::use_debug)]
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                // `write!` to a `String` cannot fail (`fmt::Write`'s only
                // error variant a `String` sink can never produce) -- `.ok()`
                // discards the always-`Ok` result explicitly rather than a
                // bare `let _ =` on a `#[must_use]` `Result`.
                write!(self.0, " {}={value:?}", field.name()).ok();
            }
        }
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.messages
            .lock()
            .expect("recording subscriber mutex")
            .push(format!("{}:{}", event.metadata().level(), visitor.0));
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

// ── test 1: abandoned pending version sweep ────────────────────────────────────

/// A pending version (never finalised) is deleted once it is older than the
/// grace cutoff.
///
/// Backdated 2h past a 1h grace window -- not `orphan_grace_secs = 0` against
/// a freshly-inserted row, which races the strict `created_at < cutoff`
/// sweep query against an equal-instant `now()` comparison (see
/// `backdate_version_created_at`'s doc comment). `run_sweep()` must delete
/// it and return `abandoned_pending_deleted = 1`.
#[tokio::test]
async fn abandoned_pending_version_is_deleted_by_sweep() {
    let (svc, _msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // create_file leaves exactly one pending version row.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    backdate_version_created_at(
        &db,
        ticket.version_id,
        time::OffsetDateTime::now_utc() - time::Duration::hours(2),
    )
    .await;

    // Verify the version exists before sweep.
    let before = store.list_versions(ticket.file_id).await.unwrap();
    assert_eq!(
        before.len(),
        1,
        "should have 1 pending version before sweep"
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "sweep should have deleted exactly 1 pending version"
    );

    // The version row should be gone.
    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        after.is_none(),
        "pending version row should be deleted after sweep"
    );

    // An orphan_reconcile audit row should have been written.
    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let reconcile_count = audit
        .iter()
        .filter(|r| r.operation == "orphan_reconcile")
        .count();
    assert!(
        reconcile_count >= 1,
        "expected at least 1 orphan_reconcile audit row"
    );
}

/// With `orphan_grace_secs = 86400` a newly-created pending version is NOT swept.
#[tokio::test]
async fn recent_pending_version_is_not_swept_within_grace_window() {
    // grace = 24 hours → a freshly created version must not be deleted.
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "recent pending version must not be swept"
    );

    // Version should still exist.
    let v = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        v.is_some(),
        "pending version must still exist after grace-protected sweep"
    );
}

/// P2 remediation 2.8: a file created by `POST /files` whose upload is
/// abandoned leaves a `files` row with no versions and `content_id IS NULL`.
/// Once the sweep reclaims that last (only) pending version, it must also
/// delete the now-permanently-orphaned parent `files` row -- otherwise it
/// lingers forever in `GET /files`, unable to ever serve content.
#[tokio::test]
async fn sweep_deletes_abandoned_zero_version_file() {
    // Backdated 2h past a 1h grace window -- not `orphan_grace_secs = 0`
    // against a freshly-inserted row, which races the strict
    // `created_at < cutoff` sweep query against an equal-instant `now()`
    // comparison (see `backdate_version_created_at`'s doc comment).
    let (svc, _msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // create_file leaves exactly one pending version and content_id = NULL.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    backdate_version_created_at(
        &db,
        ticket.version_id,
        time::OffsetDateTime::now_utc() - time::Duration::hours(2),
    )
    .await;

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "sweep should have deleted the abandoned pending version"
    );
    assert_eq!(
        result.abandoned_files_deleted, 1,
        "sweep should also have deleted the now-orphaned parent file row"
    );

    // The version row must be gone.
    let version_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_none(),
        "pending version row should be deleted after sweep"
    );

    // The parent `files` row must be gone too -- not lingering as a
    // permanent zero-version orphan.
    let file_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        file_after.is_none(),
        "orphaned zero-version file row must be deleted by the sweep"
    );

    // A `file.deleted` event must have been enqueued for downstream consumers.
    let events = store.list_file_events(ticket.file_id).await.unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "file.deleted"),
        "expected a file.deleted event for the orphan-reconciled file"
    );
}

/// Negative control for P2 2.8: a file with one abandoned pending version AND
/// one bound `Available` version must keep its parent `files` row -- the
/// sweep may only reclaim the abandoned version, never the file itself, once
/// real content still exists.
#[tokio::test]
async fn sweep_keeps_file_with_other_versions() {
    // Backdated 2h past a 1h grace window -- not `orphan_grace_secs = 0`
    // against a freshly-inserted row, which races the strict
    // `created_at < cutoff` sweep query against an equal-instant `now()`
    // comparison (see `backdate_version_created_at`'s doc comment).
    let (svc, _psvc, _msvc, dp, store, engine, _backend, db) = build_all_full(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // v1: created, then immediately abandoned (never uploaded/finalized).
    let v1 = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    backdate_version_created_at(
        &db,
        v1.version_id,
        time::OffsetDateTime::now_utc() - time::Duration::hours(2),
    )
    .await;

    // v2: a second version on the same file, uploaded and bound as current.
    let v2 = svc.presign_version(&ctx, v1.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        v1.file_id,
        v2.version_id,
        "text/plain",
        Bytes::from_static(b"real content"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, v1.file_id, v2.version_id, None)
        .await
        .unwrap();

    // Sanity: two versions exist before the sweep.
    let before = store.list_versions(v1.file_id).await.unwrap();
    assert_eq!(before.len(), 2, "file should have 2 versions before sweep");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "sweep should reclaim the one abandoned pending version (v1)"
    );
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "the file must NOT be deleted -- it still has a real, bound version"
    );

    // v1's pending version row is gone.
    let v1_after = store.get_version(v1.file_id, v1.version_id).await.unwrap();
    assert!(
        v1_after.is_none(),
        "the abandoned pending version must still be reclaimed"
    );

    // The file row and its bound version must survive untouched.
    let file_after = svc.get_file(&ctx, v1.file_id).await.unwrap();
    assert_eq!(file_after.content_id, Some(v2.version_id));
    let v2_after = store
        .get_version(v1.file_id, v2.version_id)
        .await
        .unwrap()
        .expect("bound version must survive the sweep");
    assert_eq!(v2_after.status, VersionStatus::Available);
}

// ── test 1b: versionless orphan files (never received any version) ───────────

/// Second phase of sweep step 1 (`CleanupEngine::sweep_versionless_files`):
/// a `files` row created via [`FileService::create_file_bare`] that never
/// got a version at all (simulating a crash between that commit and the
/// multipart plan's `insert_pending_version`, or a failed
/// `compensate_failed_multipart_initiate`) is reclaimed once its own
/// `created_at` ages past the grace cutoff -- even though it was never
/// picked up by `sweep_abandoned_pending`, which only ever sees a row that
/// once had a `file_versions` entry.
#[tokio::test]
async fn sweep_deletes_versionless_file_past_grace() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file::{Column as FileColumn, Entity as FileEntity};

    // grace = 1h so we can deterministically distinguish "old" (backdated
    // 2h) from "fresh" (just created) without racing the clock.
    let (svc, _msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // `create_file_bare` is the merged `POST /files` create+plan path's first
    // step: it commits a `files` row with NO version at all -- exactly the
    // shape this phase targets.
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    // Sanity: zero versions from the start.
    let versions = store.list_versions(file_id).await.unwrap();
    assert!(
        versions.is_empty(),
        "create_file_bare must leave no version"
    );

    // Backdate the file's own `created_at` past the grace cutoff -- there is
    // no public API to backdate it on an already-created row (mirrors the
    // sibling tests' direct-entity backdating of `file_versions.created_at`
    // / `multipart_uploads.expires_at`).
    let conn = db.conn().expect("conn");
    let backdated = time::OffsetDateTime::now_utc() - time::Duration::hours(2);
    FileEntity::update_many()
        .col_expr(FileColumn::CreatedAt, Expr::value(backdated))
        .filter(FileColumn::FileId.eq(file_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate file created_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "no pending version ever existed for this file"
    );
    assert_eq!(
        result.abandoned_files_deleted, 1,
        "sweep_versionless_files should have deleted the permanently versionless file"
    );

    let file_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap();
    assert!(
        file_after.is_none(),
        "the versionless orphan file row must be deleted by the sweep"
    );

    let audit = store.list_audit(file_id).await.unwrap();
    let reconcile_count = audit
        .iter()
        .filter(|r| r.operation == "orphan_reconcile")
        .count();
    assert!(
        reconcile_count >= 1,
        "expected at least 1 orphan_reconcile audit row"
    );

    let events = store.list_file_events(file_id).await.unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "file.deleted"),
        "expected a file.deleted event for the reclaimed versionless file"
    );
}

/// Negative control: a versionless file younger than the grace cutoff must
/// not be swept.
#[tokio::test]
async fn sweep_keeps_recent_versionless_file_within_grace() {
    let (svc, _msvc, store, engine, _db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "a recently-created versionless file must not be swept within the grace window"
    );

    let file_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap();
    assert!(
        file_after.is_some(),
        "the recent versionless file must survive the sweep"
    );
}

/// A versionless file past the grace cutoff, but with a live `in_progress`
/// multipart session still pointing at it, must NOT be deleted --
/// `maybe_delete_orphaned_file`'s `has_blocking_multipart_session` guard is
/// shared across every sweep phase, so this phase respects it exactly like
/// `sweep_abandoned_pending`'s own reclaim does. In production such a session
/// always implies a pending `file_versions` row exists (it is inserted
/// first, in `MultipartService::initiate_multipart_upload`), which would
/// already exclude the file from this phase's own listing query -- this test
/// exercises the guard directly (via a hand-inserted session row) to prove
/// the second phase never bypasses it, rather than relying on that
/// production invariant alone.
#[tokio::test]
async fn sweep_versionless_file_blocked_by_in_progress_multipart_session() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file::{Column as FileColumn, Entity as FileEntity};

    let (svc, _msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    let conn = db.conn().expect("conn");
    let backdated = time::OffsetDateTime::now_utc() - time::Duration::hours(2);
    FileEntity::update_many()
        .col_expr(FileColumn::CreatedAt, Expr::value(backdated))
        .filter(FileColumn::FileId.eq(file_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate file created_at");

    // Hand-insert an `in_progress` session for this file -- bypassing
    // `insert_pending_version` -- purely to exercise the shared blocking
    // guard in isolation (see the doc comment above for why this shape
    // cannot arise through the real `POST /files/{id}/multipart` path).
    let now = time::OffsetDateTime::now_utc();
    store
        .create_multipart_upload(
            Uuid::now_v7(),
            file_id,
            Uuid::now_v7(),
            "backend-handle",
            None,
            None,
            "text/plain",
            1024,
            1024,
            false,
            now + time::Duration::hours(1),
            now,
        )
        .await
        .expect("insert in_progress multipart session");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "a versionless file with a live in_progress multipart session must not be reclaimed"
    );

    let file_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap();
    assert!(
        file_after.is_some(),
        "the file must survive while its multipart session is still in_progress"
    );
}

/// A file that has a version (even an untouched `pending` one, itself still
/// fresh) must never be picked up by `sweep_versionless_files`, no matter how
/// old the *file* row itself is -- the phase's `NOT EXISTS (file_versions)`
/// predicate is what actually distinguishes it from
/// `sweep_abandoned_pending`'s version-age-keyed query, not `files.created_at`
/// alone.
#[tokio::test]
async fn sweep_versionless_files_skips_file_with_a_version() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file::{Column as FileColumn, Entity as FileEntity};

    // grace = 1h: the version created below stays fresh (not itself
    // eligible for `sweep_abandoned_pending`), while the file row is
    // deliberately backdated past the cutoff.
    let (svc, _msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Leaves one (fresh, still-pending) version -- unlike `create_file_bare`.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let conn = db.conn().expect("conn");
    let backdated = time::OffsetDateTime::now_utc() - time::Duration::hours(2);
    FileEntity::update_many()
        .col_expr(FileColumn::CreatedAt, Expr::value(backdated))
        .filter(FileColumn::FileId.eq(ticket.file_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate file created_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "the version itself is fresh, not past the grace cutoff"
    );
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "a file with an existing version row must never be treated as versionless, \
         regardless of the file row's own age"
    );

    let file_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(file_after.is_some(), "the file must survive the sweep");
    let version_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_some(),
        "the pending version must survive the sweep"
    );
}

// ── test 2: expired multipart session sweep ────────────────────────────────────

/// An in-progress multipart upload session whose `expires_at` is in the past is
/// aborted by the sweep.
///
/// We create a multipart session and then call `list_expired_multipart_uploads`
/// with a far-future `now` to confirm it returns the session (simulating passage
/// of time), then call the sweep directly with a past-pointing clock by inserting
/// a session with a manually-backdated `expires_at`.
#[tokio::test]
async fn expired_multipart_session_is_aborted_by_sweep() {
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create a file and initiate a multipart session.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    // Confirm the session is not yet expired from the sweep's perspective
    // (expires_at is 7 days in the future).
    let not_expired = store
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc(), 100)
        .await
        .unwrap();
    assert!(
        not_expired.is_empty(),
        "session with future expires_at must not appear in expired list"
    );

    // Directly insert a backdated multipart session to simulate expiry.
    let upload_id2 = Uuid::now_v7();
    let file_id2 = ticket.file_id;
    let version_id2 = Uuid::now_v7();
    let past_time = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let now_t = time::OffsetDateTime::now_utc();

    // Pre-register the pending version row for this fake session.
    store
        .insert_pending_version(
            file_id2,
            version_id2,
            "text/plain",
            "mem",
            &format!("/{file_id2}/{version_id2}"),
            now_t,
        )
        .await
        .unwrap();

    // Create the multipart session with expires_at already in the past.
    store
        .create_multipart_upload(
            upload_id2,
            file_id2,
            version_id2,
            "fake-backend-handle",
            Some("mem"),
            Some(&format!("/{file_id2}/{version_id2}")),
            "text/plain",
            0u64,      // declared_size (not relevant for sweep test)
            0u64,      // part_size (not relevant for sweep test)
            false,     // auto_bind (not relevant for sweep test)
            past_time, // expires in the past
            now_t,
        )
        .await
        .unwrap();

    // Confirm this session shows up as expired.
    let expired = store
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc(), 100)
        .await
        .unwrap();
    assert!(
        expired.iter().any(|s| s.upload_id == upload_id2),
        "backdated session must appear in expired list"
    );

    // Run the sweep.
    let result = engine.run_sweep().await;
    assert!(
        result.expired_multipart_aborted >= 1,
        "sweep must report at least 1 aborted multipart session"
    );

    // The original non-expired session should NOT be aborted.
    let original_session = store
        .get_multipart_upload(session.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original_session.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress,
        "non-expired session must still be in_progress"
    );

    // The backdated session should be aborted.
    let aborted_session = store
        .get_multipart_upload(upload_id2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        aborted_session.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "backdated session must be aborted after sweep"
    );
}

/// `m20260924_000001_upload_flow_redesign`'s `multipart_uploads_sweep_idx`
/// exists specifically to serve the OR's `completing AND lease_until < now` branch
/// (see that migration's own doc comment) -- everything above only exercises
/// the `in_progress` branch. A `completing` session is left behind when its
/// completer dies mid-assembly (after acquiring the completion lease, before
/// `finish_complete`); `abort_multipart_upload`'s `abort_expired_completing`
/// CAS (`state = 'completing' AND lease_until < now`) is what reclaims it.
///
/// There is no public API path that leaves a session `completing` with an
/// already-expired lease (acquiring the lease requires `expires_at > now`,
/// and normally the completer finishes well within its own lease), so this
/// backdates both columns directly through the entity layer -- same
/// mechanism as `sweep_deletes_versionless_file_past_grace`'s `created_at`
/// backdate above.
#[tokio::test]
async fn sweep_aborts_expired_completing_session() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::multipart_upload::{
        Column as UploadColumn, Entity as UploadEntity,
    };

    // grace = 1h is still needed here, but no longer for the reason this
    // comment used to give: `list_pending_older_than` now excludes a
    // `completing` session's own backing version unconditionally (regardless
    // of lease or `expires_at`), so step 1 can no longer race *that* version
    // out from under step 2 no matter the grace setting. What still needs a
    // non-zero grace is `create_file`'s OWN pending version (`ticket`,
    // separate from the multipart session's `plan.version_id`) -- it is not
    // tied to any multipart session, so nothing but its wall-clock age keeps
    // step 1 from reclaiming it at `grace_secs = 0`, which would delete it
    // and (once step 2 later reclaims the multipart version too) leave the
    // file with zero versions -- cascading away this very session row before
    // this test gets to assert on its `Aborted` state. A 1h grace keeps that
    // unrelated fresh version out of step 1's reach, isolating this test to
    // step 2's own behavior.
    let (svc, msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    // Move the session straight to an expired-completing state: `completing`
    // with a `lease_until` in the past, and `expires_at` in the past too --
    // `list_expired`'s predicate requires `expires_at < now` regardless of
    // which half of its `state` OR matched.
    let conn = db.conn().expect("conn");
    let past = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let rows_updated = UploadEntity::update_many()
        .col_expr(UploadColumn::State, Expr::value("completing"))
        .col_expr(UploadColumn::LeaseUntil, Expr::value(Some(past)))
        .col_expr(
            UploadColumn::LeaseOwner,
            Expr::value(Some("stale-completer".to_owned())),
        )
        .col_expr(UploadColumn::ExpiresAt, Expr::value(past))
        .filter(UploadColumn::UploadId.eq(session.upload_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate session into expired completing state");
    assert_eq!(
        rows_updated.rows_affected, 1,
        "the backdate must hit exactly the session row created above"
    );

    // Confirm it is actually picked up by the sweep's own listing query --
    // this is the index-hardening migration's `completing`-branch predicate.
    let expired = store
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc(), 100)
        .await
        .unwrap();
    assert!(
        expired.iter().any(|s| s.upload_id == session.upload_id),
        "an expired-lease completing session must appear in the sweep's expired list"
    );

    let result = engine.run_sweep().await;
    assert!(
        result.expired_multipart_aborted >= 1,
        "sweep must report the completing session as aborted"
    );

    let after = store
        .get_multipart_upload(session.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "an expired-lease completing session must be aborted by the sweep, not left dangling"
    );
    assert!(
        after.lease_until.is_none(),
        "abort_expired_completing clears the lease alongside the state transition"
    );
}

/// End-to-end regression for `m20260924_000001_upload_flow_redesign`'s
/// `idempotency_keys_file_idx`: `idempotency_keys.file_id` carries `ON DELETE
/// CASCADE` back to `files`, so deleting a file through the real service path
/// must leave no `idempotency_keys` row behind for it. Asserted via a raw
/// row count against a second, independent connection to the same sqlite
/// file (the `Store`/`CleanupStore` API surface has no getter for this
/// table) -- same technique as `tests/multipart_test.rs`'s
/// `count_files_rows`.
#[tokio::test]
async fn delete_file_cascades_idempotency_keys() {
    use sea_orm::{ConnectionTrait, Database, Statement};

    let (svc, _psvc, _msvc, _dp, _store, _engine, _backend, dsn) = build_all_with_dsn(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(
            &ctx,
            new_file(),
            Some("cascade-test-idem-key".to_owned()),
            false,
        )
        .await
        .unwrap();

    // `sea_orm`'s sqlite driver binds a `Uuid` column as a raw 16-byte BLOB
    // (not a hyphenated TEXT string) -- see `tests/multipart_test.rs`'s
    // `tamper_request_hash` for the same caveat -- so the raw query below
    // must match via an `X'...'` blob literal, not a quoted string.
    let file_hex = ticket
        .file_id
        .as_bytes()
        .iter()
        .fold(String::new(), |mut acc, b| {
            use std::fmt::Write;
            write!(acc, "{b:02x}").expect("writing to a String cannot fail");
            acc
        });
    let count_sql =
        format!("SELECT COUNT(*) AS c FROM idempotency_keys WHERE file_id = X'{file_hex}'");

    let conn = Database::connect(&dsn).await.expect("raw connect");
    let before = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            count_sql.clone(),
        ))
        .await
        .expect("count query")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c");
    assert_eq!(
        before, 1,
        "create_file with an idempotency_key must have inserted exactly one row"
    );

    svc.delete_file(&ctx, ticket.file_id, Some("*"))
        .await
        .expect("delete_file must succeed");

    let after = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            count_sql,
        ))
        .await
        .expect("count query")
        .expect("one row")
        .try_get::<i64>("", "c")
        .expect("i64 column c");
    assert_eq!(
        after, 0,
        "deleting the file must cascade-delete its idempotency_keys rows via \
         idempotency_keys_file_idx's FK ON DELETE CASCADE"
    );
}

/// Regression test: a transient batch-load failure while resolving expired
/// multipart sessions' audit-tenant `File`s (`sweep_expired_multipart`'s
/// `load_files_by_ids_for_audit` batch, run once before its loop) must be
/// **logged** (`warn!`), not silently folded into the same `Uuid::nil()`
/// fallback used for a genuinely-missing file, with no trace of why.
///
/// Wires the sweep to `FaultyListFilesByIdsStore`, which fails
/// `list_files_by_ids` only when the requested batch mentions the one file
/// this test cares about -- everything else (finding the expired session,
/// aborting it, reclaiming the pending version) goes through the real
/// `Store` unchanged. This proves two things at once: the sweep's own
/// behavior is unaffected by the lookup failure (the session is still
/// aborted, the version still reclaimed -- the fallback to a nil tenant is a
/// pre-existing, intentional best-effort choice this fix does not change),
/// while a warning naming the failing `file_id` is now emitted where none
/// was before.
#[tokio::test]
async fn abort_expired_session_logs_warning_on_transient_file_batch_load_error() {
    let (svc, msvc, store, _default_engine, _db, backend) = build_all_with_db(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let _live_session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    // Backdate a second multipart session so the sweep has something to
    // abort (same setup as `expired_multipart_session_is_aborted_by_sweep`).
    let upload_id2 = Uuid::now_v7();
    let file_id2 = ticket.file_id;
    let version_id2 = Uuid::now_v7();
    let past_time = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let now_t = time::OffsetDateTime::now_utc();

    store
        .insert_pending_version(
            file_id2,
            version_id2,
            "text/plain",
            "mem",
            &format!("/{file_id2}/{version_id2}"),
            now_t,
        )
        .await
        .unwrap();
    store
        .create_multipart_upload(
            upload_id2,
            file_id2,
            version_id2,
            "fake-backend-handle",
            Some("mem"),
            Some(&format!("/{file_id2}/{version_id2}")),
            "text/plain",
            0u64,
            0u64,
            false,
            past_time,
            now_t,
        )
        .await
        .unwrap();

    // An engine wired to the faulty store: `list_files_by_ids` returns
    // `Err(DomainError::InternalError)` for any batch mentioning `file_id2`
    // -- a real, possibly-transient DB error, not the file being genuinely
    // absent.
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyListFilesByIdsStore {
        inner: store.clone(),
        fault_file_id: file_id2,
    });
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 0,
        },
    );

    let subscriber = RecordingSubscriber::default();
    let messages = Arc::clone(&subscriber.messages);
    // Thread-local default -- see `RecordingSubscriber`'s doc comment for why
    // this is safe to hold across the `.await` below.
    let tracing_guard = tracing::subscriber::set_default(subscriber);
    let result = engine.run_sweep().await;
    drop(tracing_guard);

    assert!(
        result.expired_multipart_aborted >= 1,
        "sweep must still abort the backdated session despite the get_file failure"
    );
    let aborted_session = store
        .get_multipart_upload(upload_id2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        aborted_session.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "session must still be aborted even though the audit-tenant lookup failed"
    );

    let captured = messages.lock().expect("recording subscriber mutex");
    assert!(
        captured.iter().any(|m| m.contains("WARN")
            && m.to_lowercase().contains("failed to load file")
            && m.contains(&file_id2.to_string())),
        "a transient batch-load failure during the audit-tenant lookup must be logged as a \
         warning naming the file_id; captured events: {captured:?}"
    );
}

/// N+1 regression: sweeping several abandoned-pending-version candidates
/// across distinct files must resolve every candidate's audit-tenant `File`
/// via exactly ONE `list_files_by_ids` batch call, never a per-candidate
/// `get_file` round trip -- see `CleanupEngine::sweep_abandoned_pending`'s
/// batch-load-before-the-loop comment.
#[tokio::test]
async fn sweep_abandoned_pending_batch_loads_files_instead_of_per_candidate_get_file() {
    // Three distinct files, each left with exactly one abandoned pending
    // version (create_file's default create+plan path), under three
    // distinct tenants -- so a wrong (e.g. nil, or cross-wired) tenant_id in
    // any one candidate's audit row would be caught.
    const N: usize = 3;

    let (svc, _msvc, store, _default_engine, _db, backend) = build_all_with_db(0).await;
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let mut file_ids_and_tenants = Vec::with_capacity(N);
    for _ in 0..N {
        let tenant = Uuid::now_v7();
        let ctx = ctx(tenant);
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();
        file_ids_and_tenants.push((ticket.file_id, tenant));
    }

    let counts = CountingCleanupStore::default();
    let counting_store: Arc<dyn CleanupStore> = Arc::new(CountingCleanupStoreWrapper {
        inner: store.clone(),
        counts: counts.clone(),
    });
    let engine = CleanupEngine::new(
        counting_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 0,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, N,
        "all {N} candidates must be reclaimed"
    );

    assert_eq!(
        counts
            .list_files_by_ids_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "resolving {N} candidates' audit tenants must cost exactly ONE batch call"
    );
    assert_eq!(
        counts
            .get_file_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the batch prefetch must make the per-candidate get_file path entirely unused"
    );

    // Every candidate's own orphan_reconcile audit row must carry ITS OWN
    // file's real tenant_id, not a nil fallback or another candidate's.
    for (file_id, tenant) in file_ids_and_tenants {
        let audit = store.list_audit(file_id).await.unwrap();
        let reconcile = audit
            .iter()
            .find(|r| r.operation == "orphan_reconcile")
            .unwrap_or_else(|| panic!("expected an orphan_reconcile audit row for {file_id}"));
        assert_eq!(
            reconcile.tenant_id, tenant,
            "audit row for {file_id} must carry its own file's tenant_id, not nil or a \
             different candidate's"
        );
    }
}

/// Same N+1 regression as the sibling test above, for step 2
/// (`sweep_expired_multipart`/`abort_expired_multipart_session`): sweeping
/// several expired multipart sessions across distinct files must resolve
/// every candidate's audit-tenant `File` via exactly ONE `list_files_by_ids`
/// batch call, never a per-candidate `get_file` round trip.
#[tokio::test]
async fn sweep_expired_multipart_batch_loads_files_instead_of_per_session_get_file() {
    const N: usize = 3;

    // A large orphan_grace_secs keeps step 1 (sweep_abandoned_pending) from
    // ever touching these files' create-time pending versions (created
    // "now", nowhere near this grace window's cutoff) -- so every
    // list_files_by_ids/get_file call below is attributable to step 2 alone.
    let (svc, _msvc, store, _default_engine, _db, backend) = build_all_with_db(86400).await;
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");

    let past_time = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let now_t = time::OffsetDateTime::now_utc();
    let mut file_ids_and_tenants = Vec::with_capacity(N);
    for _ in 0..N {
        let tenant = Uuid::now_v7();
        let ctx = ctx(tenant);
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();

        // A second, already-expired multipart session on the same file --
        // mirrors `abort_expired_session_logs_warning_on_transient_file_batch_load_error`'s
        // setup. The file's own create-time pending version is left alone by
        // step 1 (see this test's large orphan_grace_secs above).
        let upload_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        store
            .insert_pending_version(
                ticket.file_id,
                version_id,
                "text/plain",
                "mem",
                &format!("/{}/{}", ticket.file_id, version_id),
                now_t,
            )
            .await
            .unwrap();
        store
            .create_multipart_upload(
                upload_id,
                ticket.file_id,
                version_id,
                "fake-backend-handle",
                Some("mem"),
                Some(&format!("/{}/{}", ticket.file_id, version_id)),
                "text/plain",
                0u64,
                0u64,
                false,
                past_time,
                now_t,
            )
            .await
            .unwrap();
        file_ids_and_tenants.push((ticket.file_id, tenant));
    }

    let counts = CountingCleanupStore::default();
    let counting_store: Arc<dyn CleanupStore> = Arc::new(CountingCleanupStoreWrapper {
        inner: store.clone(),
        counts: counts.clone(),
    });
    let engine = CleanupEngine::new(
        counting_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 86400,
        },
    );

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, N,
        "all {N} expired sessions must be aborted"
    );
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "step 1 must not have touched any of these files' pending versions \
         (large orphan_grace_secs)"
    );

    assert_eq!(
        counts
            .list_files_by_ids_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "resolving {N} sessions' audit tenants must cost exactly ONE batch call"
    );
    assert_eq!(
        counts
            .get_file_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the batch prefetch must make the per-session get_file path entirely unused"
    );

    for (file_id, tenant) in file_ids_and_tenants {
        let audit = store.list_audit(file_id).await.unwrap();
        let abort_audit = audit
            .iter()
            .find(|r| r.operation == "multipart_abort")
            .unwrap_or_else(|| panic!("expected a multipart_abort audit row for {file_id}"));
        assert_eq!(
            abort_audit.tenant_id, tenant,
            "audit row for {file_id} must carry its own file's tenant_id, not nil or a \
             different candidate's"
        );
    }
}

/// P2 remediation 2.8 (remaining): the abandoned-pending sweep must not
/// reclaim a pending version that still backs a **live** `in_progress`
/// multipart session, no matter how old that version is. Before this fix
/// `list_pending_older_than` keyed solely on `(status, created_at)`, so a
/// long-running upload (big file, generous URL TTL) that outlives
/// `orphan_grace_secs` would have its backing version deleted out from under
/// it -- and the eventual `complete_multipart_upload` would fail at
/// `finalize_version`, losing the whole upload's work.
#[tokio::test]
async fn sweep_skips_pending_version_of_active_multipart_session() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };

    // grace = 1 hour so the file's own creation-time pending version (which
    // stays fresh) is never itself a sweep candidate -- only the
    // deliberately backdated multipart-session version is.
    let (svc, msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    // Backdate the multipart session's backing version's `created_at` well
    // past the grace cutoff -- simulating a long-running upload -- while
    // leaving the session's `expires_at` untouched (still far in the
    // future; `default_url_ttl_secs = 3600` in `build_all_with_db`).
    let conn = db.conn().expect("conn");
    let backdated = time::OffsetDateTime::now_utc() - time::Duration::hours(2);
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(backdated))
        .filter(FileVersionColumn::VersionId.eq(plan.version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "a pending version backing a live multipart session must not be reclaimed"
    );

    let version_after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_some(),
        "the multipart session's backing version must survive the sweep"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert_eq!(
        session_after.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress,
        "the live session must survive the sweep untouched"
    );
}

/// A `completing` session under a live completion lease must block step 1
/// exactly like a live `in_progress` one, even once its backing pending
/// version is older than `orphan_grace_secs` -- a client's `complete` call
/// that takes longer than the grace window to assemble the object (a large
/// file) must not have its pending version (and, transitively, its parent
/// `files` row) deleted out from under the in-flight assembly.
///
/// Before this fix, `list_pending_older_than`'s exclusion subquery only
/// matched `state = 'in_progress' AND expires_at > now`; a `completing`
/// session (`has_active_for_file`/`has_active_multipart_for_file`, née
/// `has_in_progress_*`, had the same narrowing) fell through it entirely,
/// so step 1 would reclaim the version -- and the file, once it became a
/// zero-version orphan -- while the completer still held a live lease.
#[tokio::test]
async fn sweep_skips_pending_version_of_completing_session() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };

    // grace = 1 hour, same as the sibling `in_progress` test above -- only
    // the deliberately backdated multipart-session version should ever be a
    // sweep candidate.
    let (svc, msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    // Move the session to `completing` under a live lease -- the state a
    // real `complete_multipart_upload` call leaves it in while it assembles
    // the final object.
    let now = time::OffsetDateTime::now_utc();
    let acquired = store
        .acquire_multipart_complete_lease(
            plan.upload_id,
            "completer-a",
            now + time::Duration::minutes(5),
            now,
        )
        .await
        .unwrap();
    assert!(acquired, "setup: must acquire the lease before completing");

    // Backdate the backing pending version's `created_at` well past the
    // grace cutoff -- simulating an assembly that started long after the
    // upload session itself, or simply a slow completer -- while the lease
    // above stays live.
    let conn = db.conn().expect("conn");
    let backdated = now - time::Duration::hours(2);
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(backdated))
        .filter(FileVersionColumn::VersionId.eq(plan.version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 0,
        "a pending version backing a live completing session must not be reclaimed"
    );
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "the parent file must not be reclaimed while its only version is still \
         backing a live completing session"
    );

    let version_after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_some(),
        "the completing session's backing version must survive the sweep"
    );

    let file_after = svc.get_file(&ctx, ticket.file_id).await;
    assert!(
        file_after.is_ok(),
        "the parent file must survive the sweep untouched -- got: {file_after:?}"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert_eq!(
        session_after.state,
        file_storage::domain::multipart::MultipartUploadState::Completing,
        "the live completing session must survive the sweep untouched"
    );
}

/// Companion to [`sweep_skips_pending_version_of_active_multipart_session`]:
/// once the same session's `expires_at` has also passed, it is no longer
/// "live" from the sweep's perspective -- `sweep_expired_multipart` aborts
/// it, and its now-unprotected backing version becomes reclaimable.
#[tokio::test]
async fn sweep_reclaims_version_after_session_expires() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };
    use file_storage::infra::storage::entity::multipart_upload::{
        Column as MultipartUploadColumn, Entity as MultipartUploadEntity,
    };

    let (svc, msvc, store, engine, db, _backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    let conn = db.conn().expect("conn");
    let now = time::OffsetDateTime::now_utc();

    // Same backdated `created_at` as the sibling test above.
    let backdated_created = now - time::Duration::hours(2);
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(backdated_created))
        .filter(FileVersionColumn::VersionId.eq(plan.version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");

    // ...but this time the session's `expires_at` has also passed.
    let backdated_expiry = now - time::Duration::seconds(10);
    MultipartUploadEntity::update_many()
        .col_expr(
            MultipartUploadColumn::ExpiresAt,
            Expr::value(backdated_expiry),
        )
        .filter(MultipartUploadColumn::UploadId.eq(plan.upload_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate session expires_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 1,
        "the now-expired session must be aborted"
    );
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "the version must be reclaimed once its session is no longer live"
    );

    let version_after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version_after.is_none(),
        "the pending version must be gone once the multipart session is no longer live"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("the session row itself is aborted, not deleted");
    assert_eq!(
        session_after.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "the session must be aborted once its expiry has passed"
    );
}

/// P2 remediation: `sweep_reclaims_version_after_session_expires` already
/// proves the *version* is reclaimed and the session ends up `aborted` when
/// step 1 (`sweep_abandoned_pending`) races ahead of step 2
/// (`sweep_expired_multipart`) within the same `run_sweep()` call. This test
/// extends that exact ordering with the two follow-on gaps it left open:
/// before the fix, `cleanup_expired_session_version` early-returned as soon
/// as its own `get_version` lookup came back empty (because step 1 had
/// already deleted the row), which skipped BOTH the backend
/// `abort_multipart` call (leaking the backend-side multipart upload, e.g.
/// incomplete S3 MPU parts) AND the `multipart_upload_parts` row deletion
/// for this session (unbounded growth). Both must still happen in this
/// reordering.
#[tokio::test]
async fn sweep_reclaims_version_after_session_expires_still_aborts_backend_and_deletes_parts() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;

    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };
    use file_storage::infra::storage::entity::multipart_upload::{
        Column as MultipartUploadColumn, Entity as MultipartUploadEntity,
    };

    let (svc, msvc, store, engine, db, backend) = build_all_with_db(3600).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, false)
        .await
        .unwrap();

    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    // Simulate the sidecar having already uploaded (and reported) one part
    // before the session expires -- both the backend-side part state and the
    // `multipart_upload_parts` row it left behind are exactly what must be
    // reclaimed by the abort flow, even in this step1-before-step2 ordering.
    let part = plan.parts.first().expect("declared_size fits in one part");
    let part_bytes = Bytes::from_static(b"partial-part-data-before-expiry");
    let part_size = i64::try_from(part_bytes.len()).unwrap();
    let (stream, len) = one_shot_part_stream(part_bytes);
    let (etag, part_hash) = backend
        .upload_part_stream(
            &backend_path,
            &session.backend_upload_handle,
            part.part_number,
            part.offset,
            stream,
            len,
        )
        .await
        .expect("simulated sidecar part upload");
    store
        .upsert_multipart_part(
            plan.upload_id,
            i32::try_from(part.part_number).unwrap(),
            &etag,
            part_hash,
            part_size,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_multipart_parts(plan.upload_id)
            .await
            .unwrap()
            .len(),
        1,
        "sanity: the part row must exist before the sweep"
    );

    let conn = db.conn().expect("conn");
    let now = time::OffsetDateTime::now_utc();

    // Same setup as `sweep_reclaims_version_after_session_expires`: both the
    // version's `created_at` and the session's `expires_at` are backdated, so
    // step 1 reclaims the version in the SAME `run_sweep()` call, before step
    // 2 ever fetches this session.
    FileVersionEntity::update_many()
        .col_expr(
            FileVersionColumn::CreatedAt,
            Expr::value(now - time::Duration::hours(2)),
        )
        .filter(FileVersionColumn::VersionId.eq(plan.version_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");
    MultipartUploadEntity::update_many()
        .col_expr(
            MultipartUploadColumn::ExpiresAt,
            Expr::value(now - time::Duration::seconds(10)),
        )
        .filter(MultipartUploadColumn::UploadId.eq(plan.upload_id))
        .secure()
        .scope_with(&toolkit_security::AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate session expires_at");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "step 1 must reclaim the version before step 2 ever sees the session"
    );
    assert_eq!(
        result.expired_multipart_aborted, 1,
        "step 2 must still abort the session even though its version is already gone"
    );

    // The version row is gone (reclaimed by step 1, as in the sibling test).
    assert!(
        store
            .get_version(ticket.file_id, plan.version_id)
            .await
            .unwrap()
            .is_none(),
        "version must be gone"
    );

    // The part rows must be gone too -- before the fix,
    // `cleanup_expired_session_version` early-returned as soon as
    // `get_version` came back empty, so this delete was never reached.
    assert!(
        store
            .list_multipart_parts(plan.upload_id)
            .await
            .unwrap()
            .is_empty(),
        "multipart_upload_parts rows must be deleted even when the version was \
         already reclaimed by step 1"
    );

    // The backend multipart handle must have been aborted too -- before the
    // fix it leaked, because the same early return also skipped the backend
    // `abort_multipart` call. Prove it indirectly: a still-live (non-aborted)
    // handle would accept another `upload_part` call; an aborted one reports
    // "handle not found".
    let (stream, len) = one_shot_part_stream(Bytes::from_static(b"x"));
    let upload_after_abort = backend
        .upload_part_stream(
            &backend_path,
            &session.backend_upload_handle,
            2,
            0,
            stream,
            len,
        )
        .await;
    assert!(
        upload_after_abort.is_err(),
        "the backend multipart handle must have been aborted (a post-sweep upload_part \
         against it must fail), but it succeeded: {upload_after_abort:?}"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("the session row itself is aborted, not deleted");
    assert_eq!(
        session_after.state,
        file_storage::domain::multipart::MultipartUploadState::Aborted,
        "the session must be aborted"
    );
}

/// Regression (`m20260924_000001_upload_flow_redesign`'s `backend_id`/
/// `backend_path` columns): when the `file_versions` row backing an expired
/// session is already gone, cleanup must abort the backend upload on the
/// session's OWN backend/path -- read from the session row itself -- not
/// silently fall back to the *default* backend plus a recomputed
/// default-shaped path. Before those columns existed, a session whose
/// upload was never on the default backend would have its abort call
/// harmlessly no-op against the wrong (default) backend, leaking the real
/// handle on the backend the upload actually used.
#[tokio::test]
async fn cleanup_aborts_on_the_sessions_own_backend_when_version_is_already_gone() {
    let db = build_db().await;
    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        ServiceConfig {
            default_url_ttl_secs: 3600,
            sidecar_base_url: "http://sidecar.test".to_owned(),
            default_page_size: 50,
            max_page_size: 1000,
            idempotency_ttl_secs: 86400,
        },
        None,
        None,
    ));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let engine = CleanupEngine::new(
        sweep_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 3600,
        },
    );

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    // A real in-progress multipart upload against the NON-default "alt"
    // backend -- the actual backend-side handle this test proves gets
    // aborted (or leaked, on the old code).
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let backend_handle = alt_backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate on the alt backend");

    // No `file_versions` row is ever inserted for this `version_id` -- models
    // the version having already been reclaimed by a racing sweep step by the
    // time cleanup gets to this session (see
    // `cleanup_expired_session_version_with_file`'s doc comment). The session
    // itself carries its own `backend_id`/`backend_path`, exactly as
    // `initiate_multipart_upload` persists them.
    let now = time::OffsetDateTime::now_utc();
    let session = MultipartUploadSession {
        upload_id: Uuid::now_v7(),
        file_id,
        version_id,
        backend_upload_handle: backend_handle.clone(),
        state: file_storage::domain::multipart::MultipartUploadState::InProgress,
        declared_mime: "application/octet-stream".to_owned(),
        mime_validated: false,
        declared_size: 0,
        part_size: 0,
        auto_bind: false,
        lease_until: None,
        complete_result: None,
        backend_id: Some("alt".to_owned()),
        backend_path: Some(backend_path.clone()),
        created_at: now,
        expires_at: now - time::Duration::hours(1),
    };

    engine.cleanup_expired_session_version(&session).await;

    // Prove the abort landed on "alt", not "mem": a still-live handle would
    // accept another `upload_part` call; an aborted one reports "handle not
    // found". On the old (buggy) fallback, this call would have SUCCEEDED --
    // the abort would have silently no-op'd against "mem" instead.
    let (stream, len) = one_shot_part_stream(Bytes::from_static(b"x"));
    let after_abort = alt_backend
        .upload_part_stream(&backend_path, &backend_handle, 1, 0, stream, len)
        .await;
    assert!(
        after_abort.is_err(),
        "the multipart handle on the session's OWN backend (\"alt\") must have been \
         aborted, but it is still live: {after_abort:?}"
    );
}

// ── test 3: retention-policy expiry sweep ─────────────────────────────────────

/// A file that matches a tenant-level age retention rule (max_age_days = 0)
/// is deleted by the sweep and a `retention_delete` audit row is written.
///
/// P2 remediation 0.11 makes `PolicyService::create_retention_rule` reject
/// `max_age_days = 0` at write time (see `sweep_does_not_run_zero_age_rule`
/// below), so this test exercises the sweep *matcher* mechanics in isolation
/// by inserting the rule directly through the store — bypassing the service's
/// validation guard, the same way `expired_multipart_session_is_aborted_by_sweep`
/// bypasses normal session creation to simulate a backdated row.
#[tokio::test]
async fn retention_expired_file_is_deleted_by_sweep() {
    let (svc, _psvc, _msvc, dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload + bind a file.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"retention test"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Directly insert a tenant retention rule: max_age_days = 0 (expires
    // immediately) — bypasses `PolicyService::create_retention_rule`'s
    // validation guard on purpose, to test sweep mechanics against a
    // (hypothetical, pre-existing, or migrated) zero-age row.
    store
        .insert_retention_rule(
            &toolkit_security::AccessScope::allow_all(),
            tenant,
            &RetentionScope::Tenant,
            None,
            &RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    // Verify the file exists before sweep.
    let before = store.list_all_files_for_sweep(None, 1000).await.unwrap();
    assert!(
        before.iter().any(|f| f.file_id == ticket.file_id),
        "file must be present before sweep"
    );

    let result = engine.run_sweep().await;
    assert!(
        result.retention_expired_deleted >= 1,
        "sweep must delete at least 1 retention-expired file"
    );

    // The file should be gone from the DB.
    let after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        after.is_none(),
        "file must be deleted after retention sweep"
    );

    // A retention_delete audit row must exist.
    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let ret_del: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "retention_delete")
        .collect();
    assert!(
        !ret_del.is_empty(),
        "expected at least 1 retention_delete audit row"
    );
    assert_eq!(ret_del[0].outcome, "success");
}

/// Companion to `retention_expired_file_is_deleted_by_sweep`: proves that,
/// through the normal service API, a `max_age_days = 0` rule can never reach
/// the sweep in the first place — `PolicyService::create_retention_rule`
/// rejects it at write time (P2 remediation 0.11), so zero rows are ever
/// written, and a file that would otherwise match survives the sweep.
#[tokio::test]
async fn sweep_does_not_run_zero_age_rule() {
    let (svc, psvc, _msvc, dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Attempt to create the dangerous rule via the service — must be
    // rejected before any row is written.
    let result = psvc
        .create_retention_rule(
            &ctx,
            RetentionScope::Tenant,
            None,
            RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
        )
        .await;
    assert!(
        matches!(
            result,
            Err(file_storage::domain::error::DomainError::Validation { .. })
        ),
        "expected Validation, got {result:?}"
    );

    let rules = store
        .list_retention_rules(&toolkit_security::AccessScope::allow_all(), tenant)
        .await
        .unwrap();
    assert!(
        rules.is_empty(),
        "no retention rule row should exist after a rejected create"
    );

    // Create + upload + bind a file that WOULD have matched a zero-age rule.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"must survive"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.retention_expired_deleted, 0,
        "no rule exists, so nothing should be retention-deleted"
    );

    let after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        after.is_some(),
        "file must survive the sweep since the dangerous rule was never created"
    );
}

/// A transient `list_versions` failure for one file during the retention
/// sweep must abort that file's expiry (no delete) instead of being
/// swallowed as "zero versions" and deleting it anyway -- which would
/// silently orphan the file's real, un-enumerated version blobs. A second,
/// unrelated matching file (real `list_versions`) must still be deleted in
/// the same sweep, proving one file's fault does not abort the whole sweep.
#[tokio::test]
async fn expire_file_list_versions_error_does_not_delete_file() {
    let (svc, _psvc, _msvc, dp, store, _engine, backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // The file whose `list_versions` call will be made to fail.
    let faulted = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        faulted.file_id,
        faulted.version_id,
        "text/plain",
        Bytes::from_static(b"faulted"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, faulted.file_id, faulted.version_id, None)
        .await
        .unwrap();

    // A second, unrelated file with a real (non-faulted) `list_versions`.
    let healthy = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        healthy.file_id,
        healthy.version_id,
        "text/plain",
        Bytes::from_static(b"healthy"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, healthy.file_id, healthy.version_id, None)
        .await
        .unwrap();

    // Directly insert a tenant retention rule: max_age_days = 0 (expires
    // immediately) -- bypasses `PolicyService::create_retention_rule`'s
    // validation guard (P2 remediation 0.11) on purpose, same pattern as
    // `retention_expired_file_is_deleted_by_sweep`. Matches both files.
    store
        .insert_retention_rule(
            &toolkit_security::AccessScope::allow_all(),
            tenant,
            &RetentionScope::Tenant,
            None,
            &RetentionRuleBody {
                age: Some(AgeRetention { max_age_days: 0 }),
                inactivity: None,
                metadata: None,
            },
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    // Run the sweep against a fault-injecting store wrapper so only
    // `faulted.file_id`'s `list_versions` call errors; everything else
    // (including `healthy`'s) goes through the real `Store`.
    let faulty_store: Arc<dyn CleanupStore> = Arc::new(FaultyListVersionsStore {
        inner: store.clone(),
        fault_file_id: faulted.file_id,
    });
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let engine = CleanupEngine::new(
        faulty_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 86400,
        },
    );

    let result = engine.run_sweep().await;

    // (b) only the healthy file counts as retention-expired-deleted -- the
    // faulted file contributes 0 to the tally.
    assert_eq!(
        result.retention_expired_deleted, 1,
        "only the unrelated healthy file should count as retention-expired-deleted"
    );

    // (a) the faulted file's row must still exist.
    let faulted_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), faulted.file_id)
        .await
        .unwrap();
    assert!(
        faulted_after.is_some(),
        "file with a faulted list_versions call must survive the sweep"
    );

    // (c) the unrelated, healthy file must still be deleted.
    let healthy_after = store
        .get_file(&toolkit_security::AccessScope::allow_all(), healthy.file_id)
        .await
        .unwrap();
    assert!(
        healthy_after.is_none(),
        "unrelated matching file must still be deleted by the same sweep"
    );
}

/// A file that does NOT match any retention rule is NOT deleted.
#[tokio::test]
async fn file_without_matching_retention_rule_is_not_deleted() {
    let (svc, _psvc, _msvc, dp, _store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"should not be deleted"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // No retention rules configured.
    let result = engine.run_sweep().await;
    assert_eq!(
        result.retention_expired_deleted, 0,
        "file without a matching rule must not be deleted"
    );

    // Confirm the file still exists.
    let file = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(file.file_id, ticket.file_id);
}

// ── test 4: backend migration ─────────────────────────────────────────────────

/// Migrate a non-versioned file from "mem" to "alt" backend.
///
/// After migration:
/// - The file is readable via the service (content unchanged).
/// - The version row points to the "alt" backend.
/// - A `backend_migrate` audit row is written.
#[tokio::test]
async fn migrate_backend_moves_content_and_updates_version_row() {
    let (svc, dp, store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload + bind a file on the default "mem" backend.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"migrate me"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Confirm the version is on "mem".
    let v_before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(v_before.backend_id, "mem");

    // Migrate to "alt".
    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap();

    // Version row should now point to "alt".
    let v_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v_after.backend_id, "alt",
        "version must now point to alt backend"
    );

    // A backend_migrate audit row must exist.
    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let migrate_rows: Vec<_> = audit
        .iter()
        .filter(|r| r.operation == "backend_migrate")
        .collect();
    assert!(
        !migrate_rows.is_empty(),
        "expected at least 1 backend_migrate audit row"
    );
    assert_eq!(migrate_rows[0].outcome, "success");
}

/// Migrating to the same backend is a no-op.
#[tokio::test]
async fn migrate_backend_to_same_backend_is_noop() {
    let (svc, dp, store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"same backend"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Migrate to the same "mem" backend (no-op).
    svc.migrate_backend(&ctx, ticket.file_id, "mem")
        .await
        .unwrap();

    // No backend_migrate audit row should be written (was a no-op).
    let audit = store.list_audit(ticket.file_id).await.unwrap();
    let migrate_count = audit
        .iter()
        .filter(|r| r.operation == "backend_migrate")
        .count();
    assert_eq!(
        migrate_count, 0,
        "no-op migration must not write an audit row"
    );
}

/// Versioned files (more than 1 version) cannot be migrated — the service
/// returns `VersionedFileMigrationNotSupported`.
#[tokio::test]
async fn migrate_backend_rejects_versioned_file() {
    use file_storage::domain::error::DomainError;

    let (svc, dp, _store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload v1, bind it.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Presign + upload v2.
    let t2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        t2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .unwrap();

    // Now the file has 2 versions — migration must be rejected.
    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::VersionedFileMigrationNotSupported { .. }),
        "expected VersionedFileMigrationNotSupported, got {err:?}"
    );
}

// ── P2 remediation 0.5: non-durable migration target requires admin scope ──────
//
// `TenantOnlyAuthorizer` (used by `build_all_dual_backend` above) grants every
// action unconditionally, so it can't distinguish an ordinary WRITE-authorized
// caller from an admin-scoped one. These tests need that distinction, so they
// use a minimal local copy of the `ScopedTestAuthorizer` test double
// introduced in `tests/policy_authz_test.rs` (P2 remediation 0.7) — that file
// documents it as intentionally self-contained and reusable verbatim by later
// steps.

/// Grants `READ`/`WRITE`/`DELETE` unconditionally, but only grants
/// `ADMIN_POLICY` while `set_admin(true)` has been called. See
/// `tests/policy_authz_test.rs` for the canonical copy and rationale.
#[derive(Default)]
struct ScopedTestAuthorizer {
    is_admin: std::sync::atomic::AtomicBool,
}

impl ScopedTestAuthorizer {
    fn set_admin(&self, admin: bool) {
        self.is_admin
            .store(admin, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl file_storage::domain::authz::Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<toolkit_security::AccessScope, DomainError> {
        if action == file_storage::domain::authz::actions::ADMIN_POLICY
            && !self.is_admin.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(DomainError::Forbidden);
        }
        Ok(toolkit_security::AccessScope::for_tenant(
            ctx.subject_tenant_id(),
        ))
    }
}

/// Build a service with TWO in-memory backends ("mem" default, "alt" — both
/// non-durable) behind a [`ScopedTestAuthorizer`], so tests can toggle the
/// admin scope needed to migrate content onto a non-durable target.
async fn build_all_dual_backend_scoped(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    TestDataPlane,
    Store,
    Arc<ScopedTestAuthorizer>,
) {
    let db = build_db().await;

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(vec![mem_backend, alt_backend], "mem").expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer = Arc::new(ScopedTestAuthorizer::default());
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));

    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        Arc::clone(&authorizer) as Arc<dyn file_storage::domain::authz::Authorizer>,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);
    // `grace_secs` is unused by these tests but kept for signature symmetry
    // with the other `build_all*` helpers.
    let _ = grace_secs;
    (svc, dp, store, authorizer)
}

/// A non-admin caller may not migrate content onto a non-durable ("alt",
/// `InMemoryBackend`) target: `migrate_backend` must reject with `Forbidden`
/// and the version row must stay unchanged.
#[tokio::test]
async fn migrate_backend_rejects_non_durable_target_for_non_admin() {
    let (svc, dp, store, authorizer) = build_all_dual_backend_scoped(86400).await;
    authorizer.set_admin(false);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file_owned_by(ctx.subject_id()), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"non-admin migrate attempt"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got {err:?}"
    );

    let v_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v_after.backend_id, "mem",
        "version must stay on the source backend after a rejected migration"
    );
}

/// An admin-scoped caller may migrate content onto a non-durable ("alt")
/// target; the version row is updated as usual.
#[tokio::test]
async fn migrate_backend_allows_non_durable_target_for_admin_scope() {
    let (svc, dp, store, authorizer) = build_all_dual_backend_scoped(86400).await;
    authorizer.set_admin(true);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"admin migrate attempt"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap();

    let v_after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        v_after.backend_id, "alt",
        "admin-scoped caller must be able to migrate onto a non-durable target"
    );
}

// ── P2 0.3: sweep-vs-complete race tests ────────────────────────────────────────
//
// These races are tested as deterministic call-orderings per the unit-testing
// doctrine -- never via `sleep` or real concurrency. Each test either drives
// the two competing operations (session-CAS-via-sweep vs.
// `complete_multipart_upload`) fully to completion in a fixed order, or calls
// a sweep-internal helper directly to pin down the exact narrow window under
// test.

/// Drive a single-part multipart upload to a bound, `Available` version
/// through the real `msvc` + `store` + `backend` path (mirrors
/// `simulate_sidecar_put_part` + the happy-path sequence in
/// `multipart_test.rs`: initiate -> native `upload_part` ->
/// `upsert_multipart_part` -> `complete_multipart_upload` -> `bind`).
///
/// Returns `(upload_id, version_id)`.
async fn complete_one_part_multipart_upload(
    msvc: &Arc<MultipartService>,
    svc: &FileService,
    store: &Store,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: &'static [u8],
) -> (Uuid, Uuid) {
    let declared_size = data.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    let part = plan.parts.first().expect("single-part plan");

    let (stream, len) = one_shot_part_stream(Bytes::from_static(data));
    let (backend_etag, part_hash) = backend
        .upload_part_stream(
            &backend_path,
            &session.backend_upload_handle,
            part.part_number,
            part.offset,
            stream,
            len,
        )
        .await
        .expect("backend upload_part_stream");
    store
        .upsert_multipart_part(
            plan.upload_id,
            i32::try_from(part.part_number).unwrap(),
            &backend_etag,
            part_hash,
            i64::try_from(part.size).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    let _completed = msvc
        .complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    svc.bind(ctx, file_id, plan.version_id, None).await.unwrap();

    (plan.upload_id, plan.version_id)
}

/// A concurrent `complete_multipart_upload` that wins *before* the sweep gets
/// to the same session must leave the now-bound, `Available` version
/// completely untouched -- even once the sweep later observes a backdated
/// `expires_at` on that (already-`completed`) session.
///
/// The session is completed first, *then* backdated (not built with a past
/// `expires_at` from the start): the P2 0.3 step-3 defense-in-depth check in
/// `complete_multipart_upload` would otherwise reject a still-`in_progress`
/// expired session outright, which would defeat the point of this test (it
/// must exercise the sweep's session CAS losing against an
/// already-`completed` row, not `complete` being rejected up front).
#[tokio::test]
async fn sweep_after_complete_wins_does_not_delete_bound_version() {
    let (svc, _psvc, msvc, _dp, store, engine, backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let (upload_id, version_id) = complete_one_part_multipart_upload(
        &msvc,
        &svc,
        &store,
        &backend,
        &ctx,
        ticket.file_id,
        b"Hello, World!",
    )
    .await;

    // Sanity: complete + bind already happened.
    let before = store
        .get_version(ticket.file_id, version_id)
        .await
        .unwrap()
        .expect("version must exist after complete");
    assert_eq!(before.status, VersionStatus::Available);
    let file_before = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(file_before.content_id, Some(version_id));

    // Backdate the now-`completed` session's expires_at into the past,
    // simulating the sweep tick finally catching up *after* complete already
    // won the session CAS.
    store
        .set_multipart_expires_at_for_test(
            upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 0,
        "the sweep's session CAS must lose against the already-`completed` row"
    );

    // The version row must be untouched.
    let after = store
        .get_version(ticket.file_id, version_id)
        .await
        .unwrap()
        .expect("bound version must not be deleted by the sweep");
    assert_eq!(after.status, VersionStatus::Available);

    // `files.content_id` must be unchanged.
    let file_after = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(file_after.content_id, Some(version_id));
}

/// The reverse ordering: the sweep wins the session CAS *before* any
/// `complete_multipart_upload` call for the same session. Both pending
/// versions on this file (`create_file`'s own initial one, and the
/// standalone multipart session's) are reclaimed by the same `grace = 0`
/// sweep pass, so the file ends up a genuine zero-version, `NULL`-content_id
/// orphan -- and, since FS-05/F10's fix, is now correctly reclaimed too
/// (within this same sweep pass, once step 2 aborts the session that had
/// been blocking step 1's own orphan-file check). A subsequent `complete`
/// attempt therefore hits `FileNotFound` (the file itself is gone), not
/// `MultipartUploadNotInProgress`.
#[tokio::test]
async fn sweep_before_complete_wins_cleans_up_expired_session() {
    let (svc, msvc, store, engine, db, _backend) = build_all_with_db(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    // Backdate `create_file`'s own pending version explicitly, rather than
    // relying on `orphan_grace_secs = 0` against a freshly-inserted row,
    // which races the strict `created_at < cutoff` sweep query against an
    // equal-instant `now()` comparison (see `backdate_version_created_at`'s
    // doc comment). The multipart session's own version below is reclaimed
    // unconditionally by step 2's `cleanup_expired_session_version` once the
    // session is aborted, so it needs no backdating.
    backdate_version_created_at(
        &db,
        ticket.version_id,
        time::OffsetDateTime::now_utc() - time::Duration::hours(2),
    )
    .await;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();

    // Backdate the still-in_progress session's expires_at into the past
    // *before* any complete attempt -- the sweep must win this race.
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let result = engine.run_sweep().await;
    assert_eq!(
        result.expired_multipart_aborted, 1,
        "sweep must win the session CAS and abort the expired session"
    );
    assert_eq!(
        result.abandoned_files_deleted, 1,
        "FS-05/F10 fix: the now-zero-version parent file must also be reclaimed in this same \
         sweep pass, via step 2's own orphan-file check (cleanup_expired_session_version)"
    );

    // The pending version row must be gone.
    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version.is_none(),
        "pending version must be deleted once the sweep wins the session CAS"
    );

    // FS-05/F10 fix: the file itself must be gone too -- both of its
    // versions (create_file's own initial one, and this multipart session's)
    // were reclaimed by the same grace=0 sweep pass, leaving zero versions
    // and a NULL content_id, and step 2's own orphan check (unblocked now
    // that the session is aborted) correctly reclaims it.
    let file_after = svc.get_file(&ctx, ticket.file_id).await;
    assert!(
        matches!(
            file_after,
            Err(file_storage::domain::error::DomainError::FileNotFound { .. })
        ),
        "FS-05/F10 fix: expected the now-orphaned parent file to be reclaimed by the same sweep \
         pass, got: {file_after:?}"
    );

    // A subsequent complete attempt for the same upload_id must be rejected
    // -- FileNotFound now (the file itself is gone), not
    // MultipartUploadNotInProgress (which would require the file to still
    // exist for require_file to get past before reaching the session check).
    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            file_storage::domain::error::DomainError::FileNotFound { .. }
        ),
        "expected FileNotFound after the sweep reclaimed both the session and its now-orphaned \
         parent file, got {err:?}"
    );
}

/// Step 3's defense-in-depth, exercised independent of the sweep: a session
/// whose `expires_at` is already in the past but whose state is still
/// `in_progress` (no sweep tick has run at all) must be rejected by
/// `complete_multipart_upload` itself.
#[tokio::test]
async fn complete_after_session_expired_is_rejected() {
    let (svc, _psvc, msvc, _dp, store, _engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();

    // Backdate expires_at without running the sweep at all -- the session
    // row is still `in_progress` in the DB.
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            file_storage::domain::error::DomainError::MultipartUploadNotInProgress { .. }
        ),
        "expected MultipartUploadNotInProgress for an expired-but-still-in_progress \
         session, got {err:?}"
    );
}

/// Step 5's hardening, exercised in isolation from the session-CAS timing:
/// simulate the exact narrow mid-flight window where `complete_multipart_upload`
/// has already called `finalize_version` (pending -> available) but has not
/// yet reached its own session-completion CAS, so the session row is still
/// `in_progress` in the DB. Call the sweep's internal version-cleanup helper
/// directly (as `abort_expired_multipart_session` does immediately after
/// winning its own session CAS) and confirm the now-`Available` version is
/// left untouched -- the status-guarded delete must match zero rows.
#[tokio::test]
async fn sweep_mid_flight_after_finalize_but_before_session_cas_does_not_delete_available_version()
{
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .unwrap();
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    assert_eq!(
        session.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress,
        "session must still be in_progress at the moment cleanup is invoked"
    );

    // Simulate the mid-flight window: `finalize_version` has already flipped
    // the version pending -> available, but `complete_multipart_upload`
    // hasn't reached its own session CAS yet.
    let finalize_audit = file_storage::domain::audit::AuditEntry {
        tenant_id: Uuid::nil(),
        actor_kind: "system".to_owned(),
        actor_id: Uuid::nil(),
        file_id: Some(ticket.file_id),
        operation: file_storage::domain::audit::AuditOperation::FinalizeVersion,
        outcome: file_storage::domain::audit::AuditOutcome::Success,
        detail: serde_json::json!({ "test": "mid-flight simulation" }),
        occurred_at: time::OffsetDateTime::now_utc(),
    };
    let finalized = store
        .finalize_version(
            ticket.file_id,
            plan.version_id,
            5,
            vec![0u8; 32],
            file_storage::infra::content::hash_mode::HashMode::WholeSha256,
            None,
            None,
            None,
            finalize_audit,
            None,
        )
        .await
        .unwrap()
        .updated;
    assert!(
        finalized,
        "finalize_version must flip the pending version to available"
    );

    // Invoke the sweep's version-cleanup helper directly -- as
    // `abort_expired_multipart_session` would immediately after winning its
    // own session CAS (`Ok(true)`).
    engine.cleanup_expired_session_version(&session).await;

    // The version must be untouched: the status-guarded delete matched zero
    // rows because the row is no longer `pending`.
    let after = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must not be deleted by the mid-flight cleanup");
    assert_eq!(after.status, VersionStatus::Available);
}

/// Step 1's sibling to
/// [`sweep_mid_flight_after_finalize_but_before_session_cas_does_not_delete_available_version`]:
/// before the fix, `sweep_abandoned_pending` deleted its candidates with the
/// unconditional `delete_version` instead of the status-guarded
/// `delete_pending_version` step 2 already uses. A version finalized by a
/// racing `finalize_upload` between `list_abandoned_pending_versions`
/// returning the row and step 1's per-row delete running would therefore be
/// deleted anyway -- destroying a just-finalized version row and its now-real
/// backend blob.
///
/// Exercised directly against the now-`pub`
/// `CleanupEngine::delete_abandoned_pending_version` (called by
/// `sweep_abandoned_pending` per-row with the exact snapshot values
/// `list_abandoned_pending_versions` returned) rather than via a real
/// concurrent task, for the same determinism reason the step-2 sibling test
/// above calls `cleanup_expired_session_version` directly.
#[tokio::test]
async fn sweep_step1_does_not_delete_version_finalized_between_list_and_delete() {
    let (svc, _psvc, _msvc, _dp, store, engine, backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    // Snapshot exactly what `list_abandoned_pending_versions` would have
    // returned for this row (still `pending` at this point).
    let candidate = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("pending version must exist");
    assert_eq!(candidate.status, VersionStatus::Pending);

    // Put real content at the version's backend path so a wrongful blob
    // delete would be observable.
    {
        let content = Bytes::from_static(b"finalized content");
        let len = content.len() as u64;
        let stream: futures::stream::BoxStream<'static, std::io::Result<Bytes>> =
            Box::pin(futures::stream::once(async move { Ok(content) }));
        backend
            .put_stream(&candidate.backend_path, stream, Some(len))
            .await
            .unwrap();
    }

    // Simulate the race: a client's `finalize_upload` wins between the list
    // query and step 1's per-row delete, flipping the version
    // `pending -> available`.
    let finalize_audit = AuditEntry {
        tenant_id: tenant,
        actor_kind: "system".to_owned(),
        actor_id: Uuid::nil(),
        file_id: Some(ticket.file_id),
        operation: AuditOperation::FinalizeVersion,
        outcome: file_storage::domain::audit::AuditOutcome::Success,
        detail: serde_json::json!({ "test": "step1 mid-flight simulation" }),
        occurred_at: time::OffsetDateTime::now_utc(),
    };
    let finalized = store
        .finalize_version(
            ticket.file_id,
            ticket.version_id,
            17,
            vec![0u8; 32],
            file_storage::infra::content::hash_mode::HashMode::WholeSha256,
            None,
            None,
            None,
            finalize_audit,
            None,
        )
        .await
        .unwrap()
        .updated;
    assert!(finalized, "finalize_version must flip pending -> available");

    // Invoke step 1's per-row delete directly with the stale (pre-finalize)
    // snapshot -- exactly what `sweep_abandoned_pending` would do had this
    // race landed inside a real `run_sweep()` call.
    let (pending_deleted, files_deleted) = engine
        .delete_abandoned_pending_version(
            ticket.file_id,
            ticket.version_id,
            candidate.size,
            &candidate.backend_id,
            &candidate.backend_path,
            None,
        )
        .await;
    assert_eq!(
        (pending_deleted, files_deleted),
        (0, 0),
        "the status-guarded delete must match zero rows once the version is no \
         longer pending"
    );

    // The version row must be untouched.
    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must not be deleted by the mid-flight race");
    assert_eq!(after.status, VersionStatus::Available);

    // The backend blob must survive too -- proving `best_effort_delete` was
    // never reached (it lives inside the `Ok(true)` branch of the guarded
    // delete, which this race never takes).
    let blob = backend.stat(&candidate.backend_path).await;
    assert!(
        matches!(blob, Ok(Some(_))),
        "the just-finalized backend blob must not be deleted, got {blob:?}"
    );
}

// ── test: idempotency-key GC / outbox lock-in (P2 remediation 1.9) ─────────────

/// `run_sweep()` deletes `idempotency_keys` rows whose `expires_at` is at or
/// before `now` and leaves live rows completely untouched.
///
/// Builds its own `Store`/`CleanupEngine` (rather than `build_all`) so the
/// test can reach the raw `DBProvider` connection and seed rows directly via
/// `IdempotencyRepo::insert`, then assert the post-sweep state via a direct
/// `idempotency_key::Entity::find()` -- mirroring the pattern already used by
/// `multipart_test.rs` for asserting DB state independent of the store's own
/// read methods.
#[tokio::test]
async fn run_sweep_deletes_expired_idempotency_rows() {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::entity::idempotency_key;
    use file_storage::infra::storage::repo::IdempotencyRepo;
    use file_storage::infra::storage::store::IdempotencyInsert;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let store = Store::new(Arc::clone(&db));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    // `orphan_grace_secs: 86400` (not `0`) -- these files exist purely to
    // satisfy `idempotency_keys.file_id`'s FK, never bind real content, and
    // are created moments before the sweep runs. A `0` grace window would
    // make step 1 (P2 2.8) treat them as immediately-abandoned zero-version
    // orphans and delete them, cascading away the very `idempotency_keys`
    // rows this test seeds (`ON DELETE CASCADE`) before step 4 even runs --
    // unrelated to what this test actually exercises.
    let engine = CleanupEngine::new(
        sweep_store,
        backends.clone(),
        CleanupConfig {
            orphan_grace_secs: 86400,
        },
    );

    // `idempotency_keys.file_id` carries a `REFERENCES files (file_id)`
    // foreign key, so the seeded rows must point at real file rows rather
    // than arbitrary UUIDs.
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        ServiceConfig {
            default_url_ttl_secs: 3600,
            sidecar_base_url: "http://sidecar.test".to_owned(),
            default_page_size: 50,
            max_page_size: 1000,
            idempotency_ttl_secs: 86400,
        },
        None,
        None,
    );
    let tenant_id = Uuid::now_v7();
    let ctx = ctx(tenant_id);
    let expired_ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let live_ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let conn = db.conn().expect("conn");
    let repo = IdempotencyRepo::new();
    let subject_id = Uuid::now_v7();
    let now = time::OffsetDateTime::now_utc();

    // Expired row: `expires_at` is in the past, so the sweep must delete it.
    repo.insert(
        &conn,
        &IdempotencyInsert {
            tenant_id,
            owner_kind: "user".to_owned(),
            owner_id: Uuid::now_v7(),
            key: "expired-key".to_owned(),
            subject_id,
            response_status: 201,
            response_body: "{}".to_owned(),
            response_etag: "etag-expired".to_owned(),
            request_hash: b"expired-hash".to_vec(),
            expires_at: now - time::Duration::hours(1),
        },
        expired_ticket.file_id,
        now - time::Duration::hours(2),
    )
    .await
    .expect("insert expired row");

    // Live row: `expires_at` is in the future, so the sweep must leave it
    // (and every one of its fields) untouched.
    let live_owner_id = Uuid::now_v7();
    let live_file_id = live_ticket.file_id;
    repo.insert(
        &conn,
        &IdempotencyInsert {
            tenant_id,
            owner_kind: "user".to_owned(),
            owner_id: live_owner_id,
            key: "live-key".to_owned(),
            subject_id,
            response_status: 201,
            response_body: "{\"ok\":true}".to_owned(),
            response_etag: "etag-live".to_owned(),
            request_hash: b"live-hash".to_vec(),
            expires_at: now + time::Duration::days(1),
        },
        live_file_id,
        now,
    )
    .await
    .expect("insert live row");

    let result = engine.run_sweep().await;
    assert_eq!(
        result.idempotency_keys_deleted, 1,
        "sweep should have deleted exactly the one expired idempotency row"
    );

    let rows = idempotency_key::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query idempotency_keys directly");
    assert_eq!(rows.len(), 1, "only the live row should remain");
    let remaining = &rows[0];
    assert_eq!(remaining.idempotency_key, "live-key");
    assert_eq!(remaining.owner_id, live_owner_id);
    assert_eq!(remaining.file_id, live_file_id);
    assert_eq!(remaining.subject_id, subject_id);
    assert_eq!(remaining.response_status, 201);
    assert_eq!(remaining.response_body, "{\"ok\":true}");
    assert_eq!(remaining.response_etag, "etag-live");
}

/// Defense-in-depth lock-in (P2 remediation 1.9): `run_sweep()` must NOT touch
/// `audit_outbox`/`events_outbox` rows regardless of age, because `published_at`
/// stays `NULL` until the Tier 4 `EventBroker` relay exists -- a row-age-based
/// purge would silently drop events that were never delivered. This test seeds
/// an ancient, unpublished row in each outbox table directly (there is no
/// public API to backdate `occurred_at`) and confirms both survive a sweep.
#[tokio::test]
async fn run_sweep_does_not_touch_unpublished_outbox_rows() {
    use sea_orm::{EntityTrait, Set};
    use toolkit_db::secure::{SecureEntityExt, secure_insert};
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::entity::{audit_outbox, events_outbox};

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let store = Store::new(Arc::clone(&db));
    let sweep_store: Arc<dyn CleanupStore> = Arc::new(store.clone());
    let engine = CleanupEngine::new(
        sweep_store,
        backends,
        CleanupConfig {
            orphan_grace_secs: 0,
        },
    );

    let conn = db.conn().expect("conn");
    // Deliberately ancient -- decades old -- so any plausible age-based purge
    // threshold would have caught it.
    let ancient = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    let tenant_id = Uuid::now_v7();
    let file_id = Uuid::now_v7();

    let audit_event_id = Uuid::now_v7();
    let audit_am = audit_outbox::ActiveModel {
        event_id: Set(audit_event_id),
        tenant_id: Set(tenant_id),
        actor_kind: Set("system".to_owned()),
        actor_id: Set(Uuid::nil()),
        file_id: Set(Some(file_id)),
        operation: Set("orphan_reconcile".to_owned()),
        outcome: Set("success".to_owned()),
        detail: Set(serde_json::json!({ "seed": "ancient" })),
        occurred_at: Set(ancient),
        published_at: Set(None),
    };
    secure_insert::<audit_outbox::Entity>(audit_am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert ancient audit_outbox row");

    let event_event_id = Uuid::now_v7();
    let events_am = events_outbox::ActiveModel {
        event_id: Set(event_event_id),
        tenant_id: Set(tenant_id),
        owner_id: Set(Uuid::now_v7()),
        file_id: Set(file_id),
        event_type: Set("file.deleted".to_owned()),
        payload: Set(serde_json::json!({ "seed": "ancient" })),
        occurred_at: Set(ancient),
        published_at: Set(None),
    };
    secure_insert::<events_outbox::Entity>(events_am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert ancient events_outbox row");

    engine.run_sweep().await;

    let audit_rows = audit_outbox::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query audit_outbox directly");
    assert!(
        audit_rows.iter().any(|r| r.event_id == audit_event_id),
        "unpublished audit_outbox row must survive the sweep regardless of age"
    );

    let event_rows = events_outbox::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query events_outbox directly");
    assert!(
        event_rows.iter().any(|r| r.event_id == event_event_id),
        "unpublished events_outbox row must survive the sweep regardless of age"
    );
}

// ── P2 remediation 2.3: migrate_backend CAS on backend pointer ─────────────────
//
// `VersionRepo::rebind_backend`'s `UPDATE` used to be keyed only on
// `(file_id, version_id)`, with no predicate on the version's *current*
// `backend_id`/`backend_path`. Two concurrent `migrate_backend` calls that
// both read the same starting pointer would therefore both report success,
// and whichever committed last would silently win with no way for the loser
// to detect it. The CAS predicate added here
// (`backend_id = expected AND backend_path = expected`) makes the loser's
// `UPDATE` affect zero rows, so `migrate_backend` can detect and correctly
// react to the race -- see the three-way branch below.
//

/// Two racers that both captured the SAME pre-migration `(backend_id,
/// backend_path)` call `VersionRepo::rebind_backend` directly with that
/// identical CAS predicate (simulating two `migrate_backend` calls that both
/// read the same starting state before either commits): the first call must
/// win (`rows_affected == 1`) and the second must lose (`rows_affected ==
/// 0`) because the row no longer matches the predicate once the first call
/// has committed. The version row must end up reflecting only the first
/// call's target.
///
/// Fails against the pre-fix code (no `backend_id`/`backend_path` predicate
/// on the CAS): both calls would report `rows_affected == 1` there.
#[tokio::test]
async fn concurrent_migrate_backend_second_racer_is_rejected() {
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::repo::VersionRepo;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = FileService::new(store.clone(), backends, issuer, authorizer, cfg, None, None);

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("pending version row must exist");
    assert_eq!(before.backend_id, "mem");

    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let repo = VersionRepo::new();

    // Both racers read the SAME pre-migration state before either commits.
    let expected_backend_id = before.backend_id.clone();
    let expected_backend_path = before.backend_path.clone();

    let first = repo
        .rebind_backend(
            &conn,
            &scope,
            ticket.file_id,
            ticket.version_id,
            &expected_backend_id,
            &expected_backend_path,
            "alt",
            "/alt/racer-a",
        )
        .await
        .expect("first racer's CAS call");
    assert!(first, "first racer's CAS must win");

    let second = repo
        .rebind_backend(
            &conn,
            &scope,
            ticket.file_id,
            ticket.version_id,
            &expected_backend_id,
            &expected_backend_path,
            "other",
            "/other/racer-b",
        )
        .await
        .expect("second racer's CAS call");
    assert!(
        !second,
        "second racer's CAS must lose: the pointer already changed"
    );

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(
        after.backend_id, "alt",
        "version must reflect only the FIRST racer's target"
    );
    assert_eq!(after.backend_path, "/alt/racer-a");
}

/// `(file_id, version_id, expected_backend_id, expected_backend_path)`,
/// populated once the file/version under test exist and read by an injected
/// racer hook once `migrate_backend`'s own `dest.put()` fires (see
/// `RacingBackend` below).
type RaceIds = Arc<std::sync::Mutex<Option<(Uuid, Uuid, String, String)>>>;

/// A `StorageBackend` wrapper whose `publish_exclusive` runs a
/// caller-supplied `FnOnce` hook exactly once -- immediately before
/// delegating to the real backend -- then never fires again. Used to model
/// a second `migrate_backend` racer committing its own CAS write in the
/// narrow real-world window between this call's destination write and its
/// own CAS attempt, deterministically and in-process: the "other racer" runs
/// synchronously as a side effect of this call's own backend write, with no
/// `sleep`/real concurrency involved.
///
/// The hook fires on `publish_exclusive`, not `put_stream`: `migrate_backend`
/// writes its destination object via `publish_exclusive` specifically (see
/// its own doc comment), never `put_stream` -- hooking the wrong method would
/// silently never fire on the real write path.
struct RacingBackend {
    inner: Arc<dyn StorageBackend>,
    #[allow(clippy::type_complexity)]
    on_publish: std::sync::Mutex<
        Option<Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send>>,
    >,
}

#[async_trait]
impl StorageBackend for RacingBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }

    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        let hook = self.on_publish.lock().expect("on_publish mutex").take();
        if let Some(hook) = hook {
            hook().await;
        }
        self.inner.publish_exclusive(path, stream, max_size).await
    }

    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_stream(path, expected_len).await
    }

    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }

    async fn get_range_stream(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_range_stream(path, range, expected_len).await
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

    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }
}

/// Regression for the P2 2.3 loser-cleanup path: a concurrent migration to a
/// **different** target commits while this call is mid-flight. This call's
/// own CAS must then lose and, because the winner's target differs from
/// ours, our own destination write is safe to clean up -- it is never the
/// live pointer.
///
/// Modeled deterministically: a `RacingBackend` wraps the monitored call's
/// destination ("alt1") and, on its own `put()` (i.e. exactly in the window
/// between writing the destination blob and attempting the CAS), commits a
/// second migration to a DIFFERENT target ("alt2") using the version's
/// ORIGINAL pre-migration pointer as the CAS predicate -- precisely what a
/// genuine concurrent racer that read the same starting state would do.
#[tokio::test]
async fn migrate_backend_loser_target_blob_cleaned_up() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt1_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt1"));
    let alt2_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt2"));

    let tenant = Uuid::now_v7();
    let content = Bytes::from_static(b"loser cleanup content");

    // Populated once the file/version exist, read by the injected racer hook
    // when `migrate_backend`'s own `dest.put()` fires.
    let ids_cell: RaceIds = Arc::new(std::sync::Mutex::new(None));

    let hook_ids_cell = Arc::clone(&ids_cell);
    let hook_store = store.clone();
    let hook_alt2 = Arc::clone(&alt2_backend);
    let hook_bytes = content.clone();
    let hook: Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send> =
        Box::new(move || {
            Box::pin(async move {
                let (file_id, version_id, orig_backend_id, orig_backend_path) = hook_ids_cell
                    .lock()
                    .expect("ids_cell mutex")
                    .clone()
                    .expect("ids must be set before migrate_backend runs");
                let dest_path = format!("/{file_id}/{version_id}");
                write_all(&hook_alt2, &dest_path, hook_bytes.clone()).await;
                let audit = AuditEntry::success(
                    tenant,
                    "system",
                    Uuid::nil(),
                    Some(file_id),
                    AuditOperation::BackendMigrate,
                    serde_json::json!({ "racer": "concurrent-winner-different-target" }),
                );
                let won = hook_store
                    .rebind_version_backend(
                        file_id,
                        version_id,
                        &orig_backend_id,
                        &orig_backend_path,
                        "alt2",
                        &dest_path,
                        audit,
                    )
                    .await
                    .expect("racer's CAS call");
                assert!(
                    won,
                    "the injected racer's CAS must win: nothing else has touched the row yet"
                );
            })
        });

    let racing_alt1: Arc<dyn StorageBackend> = Arc::new(RacingBackend {
        inner: alt1_inner.clone(),
        on_publish: std::sync::Mutex::new(Some(hook)),
    });

    let backends = BackendRegistry::new(
        vec![
            Arc::clone(&mem_backend),
            Arc::clone(&racing_alt1),
            Arc::clone(&alt2_backend),
        ],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);

    let ctx = ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must exist before migration");
    assert_eq!(before.backend_id, "mem");
    *ids_cell.lock().expect("ids_cell mutex") = Some((
        ticket.file_id,
        ticket.version_id,
        before.backend_id.clone(),
        before.backend_path.clone(),
    ));

    let expected_dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt1")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict from the losing CAS, got {err:?}"
    );

    // The loser's own destination write must be cleaned up -- it is not the
    // live pointer.
    assert!(
        !alt1_inner.exists(&expected_dest_path).await.unwrap(),
        "loser's target blob must be cleaned up after the CAS loses"
    );

    // The winner's commit (to "alt2") must be untouched and must be the live
    // pointer.
    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(after.backend_id, "alt2");
    assert_eq!(after.backend_path, expected_dest_path);
    let winner_bytes = read_all(&alt2_backend, &expected_dest_path, content.len() as u64).await;
    assert_eq!(winner_bytes, content, "winner's blob must be untouched");
}

/// Regression for the P2 2.3 data-loss trap: a concurrent migration to the
/// **SAME** target commits while this call is mid-flight. Because
/// `Self::backend_path` is deterministic (`/{file_id}/{version_id}`), both
/// racers write to the identical path on the identical backend. This call's
/// own CAS must then lose, but -- critically -- it must recognize that the
/// live pointer now equals its OWN destination and must return `Ok(())` as a
/// no-op WITHOUT deleting the destination blob, since that blob is the
/// winner's live content. A naive "always clean up my own destination on CAS
/// failure" fix would destroy it here.
///
/// Modeled deterministically the same way as
/// `migrate_backend_loser_target_blob_cleaned_up`, but the injected racer
/// commits to the SAME target ("alt1") as the monitored call.
#[tokio::test]
async fn migrate_backend_same_target_race_preserves_winner_blob() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt1_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt1"));

    let tenant = Uuid::now_v7();
    let content = Bytes::from_static(b"same target race content");

    let ids_cell: RaceIds = Arc::new(std::sync::Mutex::new(None));

    let hook_ids_cell = Arc::clone(&ids_cell);
    let hook_store = store.clone();
    let hook_alt1 = Arc::clone(&alt1_inner);
    let hook_bytes = content.clone();
    let hook: Box<dyn FnOnce() -> futures::future::BoxFuture<'static, ()> + Send> =
        Box::new(move || {
            Box::pin(async move {
                let (file_id, version_id, orig_backend_id, orig_backend_path) = hook_ids_cell
                    .lock()
                    .expect("ids_cell mutex")
                    .clone()
                    .expect("ids must be set before migrate_backend runs");
                let dest_path = format!("/{file_id}/{version_id}");
                // The winner commits its own blob to the SAME path/backend
                // the monitored call is about to write to.
                write_all(&hook_alt1, &dest_path, hook_bytes.clone()).await;
                let audit = AuditEntry::success(
                    tenant,
                    "system",
                    Uuid::nil(),
                    Some(file_id),
                    AuditOperation::BackendMigrate,
                    serde_json::json!({ "racer": "concurrent-winner-same-target" }),
                );
                let won = hook_store
                    .rebind_version_backend(
                        file_id,
                        version_id,
                        &orig_backend_id,
                        &orig_backend_path,
                        "alt1",
                        &dest_path,
                        audit,
                    )
                    .await
                    .expect("racer's CAS call");
                assert!(
                    won,
                    "the injected racer's CAS must win: nothing else has touched the row yet"
                );
            })
        });

    let racing_alt1: Arc<dyn StorageBackend> = Arc::new(RacingBackend {
        inner: alt1_inner.clone(),
        on_publish: std::sync::Mutex::new(Some(hook)),
    });

    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&racing_alt1)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);

    let ctx = ctx(tenant);
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must exist before migration");
    assert_eq!(before.backend_id, "mem");
    *ids_cell.lock().expect("ids_cell mutex") = Some((
        ticket.file_id,
        ticket.version_id,
        before.backend_id.clone(),
        before.backend_path.clone(),
    ));

    let expected_dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);

    // The CAS loses, but the same-target race must be treated as a
    // successful no-op, not an error.
    svc.migrate_backend(&ctx, ticket.file_id, "alt1")
        .await
        .expect("same-target race must be a no-op, not an error");

    // The version row must reflect the winner's commit (which happens to be
    // the same target this call also wrote to).
    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(after.backend_id, "alt1");
    assert_eq!(after.backend_path, expected_dest_path);

    // Critically: the destination blob must still exist and hold the
    // winner's content -- a naive unconditional cleanup would have deleted
    // it here, destroying the winner's live data.
    assert!(
        alt1_inner.exists(&expected_dest_path).await.unwrap(),
        "winner's destination blob must NOT be deleted by the loser's cleanup"
    );
    let stored_bytes = read_all(&alt1_inner, &expected_dest_path, content.len() as u64).await;
    assert_eq!(
        stored_bytes, content,
        "surviving blob must match the winner's bytes"
    );
}

// ── P2 remediation: a pre-existing destination object is re-verified, never
// trusted on the strength of `created: false` alone ────────────────────────
//
// `publish_exclusive`'s `created: false` only means *some* earlier attempt
// already wrote to the deterministic destination path -- never that its
// bytes were ever hash-checked. An interrupted earlier attempt (crashed or
// cancelled after its own `publish_exclusive` call returned but before it
// read its own verification slot and cleaned up on mismatch) can leave
// unverified, possibly corrupt bytes sitting there with no live database
// pointer. These tests seed exactly that: bytes already present at the
// canonical destination path *before* `migrate_backend` ever runs, with no
// racer involved at all (unlike `migrate_backend_loser_target_blob_cleaned_up`
// / `migrate_backend_same_target_race_preserves_winner_blob` above, which
// model a genuine in-flight concurrent racer).

/// (a) Corrupted bytes already sit at the destination's canonical path
/// (same declared length, different content) before `migrate_backend` runs
/// at all. The call must read them back, notice the mismatch, refuse to
/// migrate, and delete the garbage -- even though this call did not create
/// it.
#[tokio::test]
async fn migrate_backend_rejects_corrupted_preexisting_destination_object() {
    let db = build_db().await;
    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let content = Bytes::from_static(b"the real, correct content on the source backend");

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Pre-seed "alt" at the canonical destination path with the SAME length
    // but DIFFERENT bytes -- standing in for an earlier, interrupted
    // migration attempt that wrote but never got to verify/clean up.
    let dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);
    let mut corrupted_bytes = content.to_vec();
    for b in &mut corrupted_bytes {
        *b ^= 0xFF;
    }
    let corrupted = Bytes::from(corrupted_bytes);
    write_all(&alt_backend, &dest_path, corrupted).await;

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "expected HashMismatch from the pre-existing destination object's read-back \
         verification, got {err:?}"
    );

    // The version must stay on the source backend -- migration never
    // committed.
    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after.backend_id, "mem",
        "version must stay on the source backend when the pre-existing \
         destination object fails re-verification"
    );

    // The garbage object must be deleted -- even though this call did not
    // create it, it never proved itself to be anyone's verified content.
    assert!(
        !alt_backend.exists(&dest_path).await.unwrap(),
        "corrupted pre-existing destination object must be cleaned up"
    );
}

/// (b) The CORRECT bytes already sit at the destination's canonical path
/// before `migrate_backend` runs (e.g. an earlier attempt that fully
/// completed its own write and would have verified fine, but the CAS/audit
/// step never ran for some unrelated reason). The read-back verification
/// must pass and the migration must succeed exactly as if this call had
/// written the bytes itself.
#[tokio::test]
async fn migrate_backend_accepts_correct_preexisting_destination_object() {
    let db = build_db().await;
    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let content = Bytes::from_static(b"identical content on both backends");

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Pre-seed "alt" at the canonical destination path with the CORRECT
    // bytes -- this call must not need to write anything there itself.
    let dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);
    write_all(&alt_backend, &dest_path, content.clone()).await;

    svc.migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .expect("a correct pre-existing destination object must be accepted");

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(after.backend_id, "alt", "version must now point to alt");
    assert_eq!(after.backend_path, dest_path);

    // The (already-correct) destination object must be untouched.
    let stored = read_all(&alt_backend, &dest_path, content.len() as u64).await;
    assert_eq!(
        stored, content,
        "pre-existing correct destination object must be left as-is"
    );

    // A `backend_migrate` audit row must still be written -- this is a real
    // committed migration, not a same-backend no-op.
    let audit = store.list_audit(ticket.file_id).await.unwrap();
    assert!(
        audit.iter().any(|r| r.operation == "backend_migrate"),
        "expected a backend_migrate audit row"
    );
}

/// (c) Same as (a), but for a `multipart-composite-sha256` version: the
/// pre-existing destination object's bytes are corrupted relative to the
/// stored `version_hash_manifest`. The read-back verification must run the
/// same composite-mode split-rehash-rebuild-compare algorithm the source
/// stream uses, not a whole-object check, and must still reject and clean up
/// on mismatch.
#[tokio::test]
async fn migrate_backend_rejects_corrupted_preexisting_destination_object_composite() {
    use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;

    let db = build_db().await;
    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_backend)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        multipart_store,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // `create_file_bare` creates the file with NO initial version at all
    // (unlike `create_file`, whose returned ticket already carries one) --
    // `migrate_backend` requires exactly 1 version, so the multipart upload
    // below must be the file's only one.
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    // Force a 2-part plan (part 1 = DEFAULT_MIN_PART_SIZE, part 2 = the
    // 100-byte remainder) so the resulting version is
    // `multipart-composite-sha256`, not the single-part fast path's
    // `whole-sha256`.
    let part1 = Bytes::from(vec![
        0xABu8;
        usize::try_from(DEFAULT_MIN_PART_SIZE)
            .expect("fits in usize")
    ]);
    let part2 = Bytes::from(vec![0xCDu8; 100]);
    let declared_size = part1.len() as u64 + part2.len() as u64;

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            Some(DEFAULT_MIN_PART_SIZE),
            false,
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 2, "plan must have exactly 2 parts");

    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", file_id, plan.version_id);

    for (part, data) in plan.parts.iter().zip([part1.clone(), part2.clone()]) {
        assert_eq!(
            data.len() as u64,
            part.size,
            "part {} size",
            part.part_number
        );
        let (stream, len) = one_shot_part_stream(data);
        let (backend_etag, part_hash) = mem_backend
            .upload_part_stream(
                &backend_path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                stream,
                len,
            )
            .await
            .expect("backend upload_part_stream");
        store
            .upsert_multipart_part(
                plan.upload_id,
                i32::try_from(part.part_number).unwrap(),
                &backend_etag,
                part_hash,
                i64::try_from(part.size).unwrap(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .unwrap();
    }

    let _completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    svc.bind(&ctx, file_id, plan.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version must exist after complete+bind");
    assert_eq!(before.hash_mode, "multipart-composite-sha256");
    assert_eq!(before.backend_id, "mem");

    // Pre-seed "alt" at the canonical destination path with the correct
    // TOTAL length but corrupted bytes in the second part's span.
    let dest_path = format!("/{}/{}", file_id, plan.version_id);
    let mut corrupted = Vec::with_capacity(usize::try_from(declared_size).expect("fits in usize"));
    corrupted.extend_from_slice(&part1);
    corrupted.extend_from_slice(&vec![0xEFu8; part2.len()]);
    write_all(&alt_backend, &dest_path, Bytes::from(corrupted)).await;

    let err = svc.migrate_backend(&ctx, file_id, "alt").await.unwrap_err();
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "expected HashMismatch for the corrupted composite destination object, got {err:?}"
    );

    let after = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after.backend_id, "mem",
        "composite version must stay on the source backend on read-back mismatch"
    );
    assert!(
        !alt_backend.exists(&dest_path).await.unwrap(),
        "corrupted pre-existing composite destination object must be cleaned up"
    );
}

// ── MAJOR remediation: a pre-existing destination object's re-verification
// distinguishes a *confirmed* mismatch (delete) from a check that could not
// be *completed at all* (read failure, broken stream) -- the latter must
// NOT delete anything and must surface a retryable error instead; and a
// pre-CAS `stat` re-check catches an object that vanished entirely between
// this call's own verification and its CAS ─────────────────────────────────
//
// Regression for the MAJOR review finding: two concurrent migrations of the
// same file to the same destination, where the losing racer's read-back
// re-verification of the winner's already-correct object hits a *transient*
// backend error (not a real content mismatch), used to be treated exactly
// like a confirmed mismatch -- deleting the winner's live, correctly-written
// object out from under it, after which the winner's own CAS would still
// succeed (it only compares DB columns), leaving the version pointing at
// nothing. These tests seed a pre-existing, CORRECT destination object (as
// if written by a delayed concurrent migration attempt) and either inject a
// transient read failure into its re-verification, or delete the object
// entirely in the window between write and CAS.

/// A `StorageBackend` wrapper whose `get_stream` yields only the first
/// `abort_after` bytes of the real object, then a fault of a caller-chosen
/// `kind` -- modeling a read failure while re-verifying a pre-existing
/// destination object, transient (a dropped connection, a backend hiccup) or
/// permanent depending on `kind`. Fetches the real object once via
/// `inner.get_stream` (test-harness-only; the objects under test here are
/// small) and rebuilds a stream from it with the fault applied. Everything
/// else passes straight through to `inner`.
struct DestReadFaultBackend {
    inner: Arc<dyn StorageBackend>,
    abort_after: usize,
    kind: std::io::ErrorKind,
}

#[async_trait]
impl StorageBackend for DestReadFaultBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        use futures::StreamExt;

        let mut real = self.inner.get_stream(path, expected_len).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = real.next().await {
            bytes.extend_from_slice(
                &chunk.map_err(|e| DomainError::backend(self.id(), e.to_string()))?,
            );
        }
        let take = self.abort_after.min(bytes.len());
        let chunks: Vec<std::io::Result<Bytes>> = vec![
            Ok(Bytes::copy_from_slice(&bytes[..take])),
            Err(std::io::Error::new(
                self.kind,
                "simulated destination read failure (test)",
            )),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_range_stream(path, range, expected_len).await
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
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }
}

/// (a)+(b) A pre-existing, CORRECT destination object's re-verification hits
/// a transient (`TimedOut`) mid-read failure (not a real mismatch): the
/// migration must fail with a retryable `BackendUnavailable` (not
/// `HashMismatch`, and not the permanent `Backend` a non-transient cause
/// would produce), and -- critically -- must NOT delete the object, since it
/// may be another migration's valid, already-verified content. A subsequent
/// retry, once the read succeeds, must complete normally using that same,
/// untouched object.
#[tokio::test]
async fn migrate_backend_dest_reverify_read_error_is_retryable_and_preserves_object() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);

    // A plain registry, used both for initial setup and for the later,
    // fault-free retry.
    let plain_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_inner)],
        "mem",
    )
    .expect("registry");
    let plain_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let plain_svc = Arc::new(FileService::new(
        store.clone(),
        plain_backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        plain_cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&plain_svc), store.clone(), plain_backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let content =
        Bytes::from_static(b"content already correctly written by a delayed concurrent migration");

    let ticket = plain_svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    plain_svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Pre-seed "alt" at the canonical destination path with the CORRECT
    // bytes -- standing in for a delayed writer (A in the review scenario)
    // that already wrote and would have verified fine.
    let dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);
    write_all(&alt_inner, &dest_path, content.clone()).await;

    // A second registry/service over the SAME db, substituting a
    // read-fault-injecting wrapper for "alt" -- models this call (B in the
    // review scenario) hitting a transient error while re-verifying A's
    // object.
    let faulty_alt: Arc<dyn StorageBackend> = Arc::new(DestReadFaultBackend {
        inner: Arc::clone(&alt_inner),
        abort_after: 10,
        kind: std::io::ErrorKind::TimedOut,
    });
    let faulty_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&faulty_alt)],
        "mem",
    )
    .expect("registry");
    let faulty_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let faulty_svc = Arc::new(FileService::new(
        store.clone(),
        faulty_backends,
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        faulty_cfg,
        None,
        None,
    ));

    let err = faulty_svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::BackendUnavailable { .. }),
        "a transient (TimedOut) read failure while re-verifying a pre-existing \
         destination object must surface as a retryable BackendUnavailable, got {err:?}"
    );

    // The object must survive: a transient read failure is not proof of
    // corruption.
    assert!(
        alt_inner.exists(&dest_path).await.unwrap(),
        "a pre-existing destination object must NOT be deleted when its \
         re-verification merely fails to complete"
    );
    let preserved = read_all(&alt_inner, &dest_path, content.len() as u64).await;
    assert_eq!(
        preserved, content,
        "the preserved object's bytes must be untouched"
    );

    let after_failure = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after_failure.backend_id, "mem",
        "version must stay on the source backend after a retryable failure"
    );

    // Retrying (without the fault) must now succeed, reusing the same,
    // still-intact object.
    plain_svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .expect("retrying once the read succeeds must complete the migration");

    let after_retry = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after_retry.backend_id, "alt",
        "retry must switch the version to alt"
    );
    assert_eq!(after_retry.backend_path, dest_path);
}

/// Same scenario as the transient case above, but with a permanent
/// (`InvalidData`) mid-read fault instead of a transient one: the migration
/// must still fail with the non-retryable `Backend` class (not
/// `BackendUnavailable`), and the pre-existing object must still be
/// preserved untouched -- an *unconfirmed* check never deletes, regardless
/// of whether its underlying cause was transient or permanent.
#[tokio::test]
async fn migrate_backend_dest_reverify_permanent_read_error_is_backend_and_preserves_object() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);

    let plain_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_inner)],
        "mem",
    )
    .expect("registry");
    let plain_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let plain_svc = Arc::new(FileService::new(
        store.clone(),
        plain_backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        plain_cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&plain_svc), store.clone(), plain_backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let content = Bytes::from_static(
        b"content already correctly written by a delayed concurrent migration (permanent fault case)",
    );

    let ticket = plain_svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    plain_svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);
    write_all(&alt_inner, &dest_path, content.clone()).await;

    let faulty_alt: Arc<dyn StorageBackend> = Arc::new(DestReadFaultBackend {
        inner: Arc::clone(&alt_inner),
        abort_after: 10,
        kind: std::io::ErrorKind::InvalidData,
    });
    let faulty_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&faulty_alt)],
        "mem",
    )
    .expect("registry");
    let faulty_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let faulty_svc = Arc::new(FileService::new(
        store.clone(),
        faulty_backends,
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        faulty_cfg,
        None,
        None,
    ));

    let err = faulty_svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Backend { .. }),
        "a permanent (InvalidData) read failure while re-verifying a pre-existing \
         destination object must surface as a non-retryable Backend error, got {err:?}"
    );

    assert!(
        alt_inner.exists(&dest_path).await.unwrap(),
        "a pre-existing destination object must NOT be deleted when its \
         re-verification merely fails to complete, even for a permanent cause"
    );
    let preserved = read_all(&alt_inner, &dest_path, content.len() as u64).await;
    assert_eq!(
        preserved, content,
        "the preserved object's bytes must be untouched"
    );

    let after_failure = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after_failure.backend_id, "mem",
        "version must stay on the source backend after a non-retryable failure"
    );
}

/// A `StorageBackend` wrapper whose `get_stream` fails to even open --
/// modeling a transient backend fault (a dropped connection, a timeout)
/// discovered before a single byte of the read-back is seen, as opposed to
/// [`DestReadFaultBackend`]'s mid-read failure. Unlike a mid-read failure
/// (whose chunk error type is a plain `std::io::Error`, always folded into
/// `DomainError::Backend`), a failure to open the stream is already a
/// `DomainError` returned by the backend itself, so
/// `verify_existing_dest_object` propagates it completely unchanged -- this
/// backend returns `BackendUnavailable` to prove that class survives the
/// wrapping instead of being collapsed into `Backend`.
struct DestOpenFaultBackend {
    inner: Arc<dyn StorageBackend>,
}

#[async_trait]
impl StorageBackend for DestOpenFaultBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    async fn get_stream(
        &self,
        _path: &str,
        _expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        Err(DomainError::backend_unavailable(
            self.id(),
            "simulated transient connect failure (test fault)",
        ))
    }
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_range_stream(path, range, expected_len).await
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
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }
}

/// A pre-existing destination object whose re-verification fails to even
/// open the read-back stream with a `BackendUnavailable` must surface that
/// exact error, unchanged -- not the `Backend` a mid-read failure would
/// produce -- since `verify_existing_dest_object` propagates a `get_stream`
/// failure directly rather than re-wrapping it. The object must survive and
/// a fault-free retry must still complete the migration.
#[tokio::test]
async fn migrate_backend_dest_reverify_open_error_preserves_backend_unavailable_class() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);

    let plain_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_inner)],
        "mem",
    )
    .expect("registry");
    let plain_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let plain_svc = Arc::new(FileService::new(
        store.clone(),
        plain_backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        plain_cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&plain_svc), store.clone(), plain_backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let content = Bytes::from_static(b"content already correctly written, but unreachable");

    let ticket = plain_svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    plain_svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);
    write_all(&alt_inner, &dest_path, content.clone()).await;

    let faulty_alt: Arc<dyn StorageBackend> = Arc::new(DestOpenFaultBackend {
        inner: Arc::clone(&alt_inner),
    });
    let faulty_backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&faulty_alt)],
        "mem",
    )
    .expect("registry");
    let faulty_cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let faulty_svc = Arc::new(FileService::new(
        store.clone(),
        faulty_backends,
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        faulty_cfg,
        None,
        None,
    ));

    let err = faulty_svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::BackendUnavailable { .. }),
        "a get_stream failure that is already BackendUnavailable must propagate \
         unchanged, not collapse into Backend, got {err:?}"
    );

    assert!(
        alt_inner.exists(&dest_path).await.unwrap(),
        "a pre-existing destination object must NOT be deleted when its \
         re-verification merely fails to open"
    );

    plain_svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .expect("retrying once the open failure clears must complete the migration");

    let after_retry = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after_retry.backend_id, "alt",
        "retry must switch the version to alt"
    );
    assert_eq!(after_retry.backend_path, dest_path);
}

/// A `StorageBackend` wrapper whose `publish_exclusive` deletes the object it
/// just wrote (straight from the underlying backend) immediately after a
/// successful write, before returning control to the caller -- modeling an
/// external actor (a distinct process, an operator, direct backend surgery)
/// removing the object in the narrow window between this call's own
/// write+verification and its CAS. This is exactly the window the pre-CAS
/// `stat` re-check exists to catch.
struct DeleteAfterPublishBackend {
    inner: Arc<dyn StorageBackend>,
}

#[async_trait]
impl StorageBackend for DeleteAfterPublishBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: futures::stream::BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<file_storage::infra::backend::PublishOutcome, DomainError> {
        let outcome = self.inner.publish_exclusive(path, stream, max_size).await?;
        if outcome.created {
            self.inner
                .delete(path)
                .await
                .expect("test setup: delete-after-publish must succeed");
        }
        Ok(outcome)
    }
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_stream(path, expected_len).await
    }
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
        expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_range_stream(path, range, expected_len).await
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
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }
}

/// (d) The destination object is removed by something outside
/// `migrate_backend`'s own coordination (modeled here via
/// `DeleteAfterPublishBackend`) in the narrow window between this call's own
/// successful write+verification and its CAS. The pre-CAS `stat` re-check
/// must catch this: the CAS must never fire, the migration must fail with a
/// retryable backend error, and the version must stay on the source backend
/// untouched.
#[tokio::test]
async fn migrate_backend_dest_object_vanishes_before_cas_fails_without_committing() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));

    let mem_backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let alt_inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("alt"));
    let alt_vanishing: Arc<dyn StorageBackend> = Arc::new(DeleteAfterPublishBackend {
        inner: Arc::clone(&alt_inner),
    });

    let backends = BackendRegistry::new(
        vec![Arc::clone(&mem_backend), Arc::clone(&alt_vanishing)],
        "mem",
    )
    .expect("registry");

    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = TestDataPlane::new(Arc::clone(&svc), store.clone(), backends);

    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let content = Bytes::from_static(b"vanishing destination object content");

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        content.clone(),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let before = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must exist before migration");

    let dest_path = format!("/{}/{}", ticket.file_id, ticket.version_id);

    let err = svc
        .migrate_backend(&ctx, ticket.file_id, "alt")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::BackendUnavailable { .. }),
        "a destination object vanishing before the CAS is a concurrent-change \
         race, not a permanent fault -- it must surface as a retryable \
         BackendUnavailable (503), got {err:?}"
    );

    assert!(
        !alt_inner.exists(&dest_path).await.unwrap(),
        "test setup sanity check: the object must indeed be gone"
    );

    let after = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version must still exist");
    assert_eq!(
        after.backend_id, before.backend_id,
        "version must stay on the source backend when the destination \
         object vanishes before the CAS"
    );
    assert_eq!(after.backend_path, before.backend_path);
}
