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
//!
//! @cpt-cf-file-storage-fr-orphan-reconciliation
//! @cpt-cf-file-storage-fr-retention-policies
//! @cpt-cf-file-storage-fr-backend-migration

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
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::MultipartUploadSession;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{
    AgeRetention, RetentionRuleBody, RetentionScope, StoredRetentionRule,
};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.cleanup_test.file.type.v1~");

// ── test harness ──────────────────────────────────────────────────────────────

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-cleanup-test-{}.db", Uuid::now_v7().simple()));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    Arc::new(DBProvider::new(db))
}

/// Build a service + cleanup engine sharing the same Store and BackendRegistry.
/// `grace_secs = 0` means every pending version is immediately eligible for sweep.
async fn build_all(
    grace_secs: u64,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    Arc<MultipartService>,
    DataPlaneService,
    Store,
    CleanupEngine,
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
        backends,
        Arc::clone(&authorizer),
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let engine = CleanupEngine::new(
        sweep_store,
        sweep_backends,
        CleanupConfig {
            orphan_grace_secs: grace_secs,
        },
    );
    (svc, psvc, msvc, dp, store, engine, backend)
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
) -> (Arc<FileService>, DataPlaneService, Store, CleanupEngine) {
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
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
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

/// A [`CleanupStore`] wrapper that makes `list_versions` fail for one
/// specific `file_id` while delegating every other method to a real
/// [`Store`]. `CleanupStore` is a narrow trait, so this is a small
/// hand-written newtype rather than a mocking-framework fake (same shape as
/// `enforce_test.rs`'s `ErroringQuota`/`CappedQuota`).
///
/// Used to prove (P2 remediation 0.6) that a transient `list_versions`
/// failure during the retention sweep aborts that file's expiry instead of
/// being swallowed as "zero versions" and deleting the file anyway.
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
    ) -> Result<Vec<FileVersion>, DomainError> {
        self.inner
            .list_abandoned_pending_versions(older_than, now)
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
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        self.inner.list_expired_multipart_uploads(now).await
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

    /// The one faulted method: errors for `fault_file_id`, delegates otherwise.
    async fn list_versions(&self, file_id: Uuid) -> Result<Vec<FileVersion>, DomainError> {
        if file_id == self.fault_file_id {
            Err(DomainError::InternalError)
        } else {
            self.inner.list_versions(file_id).await
        }
    }

    async fn get_file(&self, file_id: Uuid) -> Result<Option<File>, DomainError> {
        self.inner
            .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
            .await
    }

    async fn has_in_progress_multipart_for_file(&self, file_id: Uuid) -> Result<bool, DomainError> {
        self.inner.has_in_progress_multipart_for_file(file_id).await
    }

    async fn delete_file_with_event(
        &self,
        scope: &toolkit_security::AccessScope,
        file_id: Uuid,
        audit: AuditEntry,
        event: Option<FileEvent>,
    ) -> Result<bool, DomainError> {
        self.inner
            .delete_file_with_event(scope, file_id, audit, event)
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
    ) -> Result<u64, DomainError> {
        self.inner.delete_expired_idempotency_keys(now).await
    }
}

// ── test 1: abandoned pending version sweep ────────────────────────────────────

/// A pending version (never finalised) is deleted when the grace period is 0.
///
/// With `orphan_grace_secs = 0` every pending version created before `now()` is
/// immediately eligible; `run_sweep()` must delete it and return
/// `abandoned_pending_deleted = 1`.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn abandoned_pending_version_is_deleted_by_sweep() {
    // grace = 0 → any pre-existing pending version is eligible immediately.
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // create_file leaves exactly one pending version row.
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn recent_pending_version_is_not_swept_within_grace_window() {
    // grace = 24 hours → a freshly created version must not be deleted.
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn sweep_deletes_abandoned_zero_version_file() {
    // grace = 0 → the file's only pending version is immediately eligible.
    let (svc, _psvc, _msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // create_file leaves exactly one pending version and content_id = NULL.
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn sweep_keeps_file_with_other_versions() {
    let (svc, _psvc, _msvc, dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // v1: created, then immediately abandoned (never uploaded/finalized).
    let v1 = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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

// ── test 2: expired multipart session sweep ────────────────────────────────────

/// An in-progress multipart upload session whose `expires_at` is in the past is
/// aborted by the sweep.
///
/// We create a multipart session and then call `list_expired_multipart_uploads`
/// with a far-future `now` to confirm it returns the session (simulating passage
/// of time), then call the sweep directly with a past-pointing clock by inserting
/// a session with a manually-backdated `expires_at`.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn expired_multipart_session_is_aborted_by_sweep() {
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create a file and initiate a multipart session.
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None, false)
        .await
        .unwrap();

    // Confirm the session is not yet expired from the sweep's perspective
    // (expires_at is 7 days in the future).
    let not_expired = store
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc())
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
        .list_expired_multipart_uploads(time::OffsetDateTime::now_utc())
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

/// P2 remediation 2.8 (remaining): the abandoned-pending sweep must not
/// reclaim a pending version that still backs a **live** `in_progress`
/// multipart session, no matter how old that version is. Before this fix
/// `list_pending_older_than` keyed solely on `(status, created_at)`, so a
/// long-running upload (big file, generous URL TTL) that outlives
/// `orphan_grace_secs` would have its backing version deleted out from under
/// it -- and the eventual `complete_multipart_upload` would fail at
/// `finalize_version`, losing the whole upload's work.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
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

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None, false)
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

/// Companion to [`sweep_skips_pending_version_of_active_multipart_session`]:
/// once the same session's `expires_at` has also passed, it is no longer
/// "live" from the sweep's perspective -- `sweep_expired_multipart` aborts
/// it, and its now-unprotected backing version becomes reclaimable.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
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

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None, false)
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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
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

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None, false)
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
    let (etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            part.part_number,
            part.offset,
            part_bytes,
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
    let upload_after_abort = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            2,
            0,
            Bytes::from_static(b"x"),
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
///
/// @cpt-cf-file-storage-fr-retention-policies
#[tokio::test]
async fn retention_expired_file_is_deleted_by_sweep() {
    let (svc, _psvc, _msvc, dp, store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload + bind a file.
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-retention-policies
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
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-retention-policies
#[tokio::test]
async fn expire_file_list_versions_error_does_not_delete_file() {
    let (svc, _psvc, _msvc, dp, store, _engine, backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // The file whose `list_versions` call will be made to fail.
    let faulted = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    let healthy = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-retention-policies
#[tokio::test]
async fn file_without_matching_retention_rule_is_not_deleted() {
    let (svc, _psvc, _msvc, dp, _store, engine, _backend) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-backend-migration
#[tokio::test]
async fn migrate_backend_moves_content_and_updates_version_row() {
    let (svc, dp, store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload + bind a file on the default "mem" backend.
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-backend-migration
#[tokio::test]
async fn migrate_backend_to_same_backend_is_noop() {
    let (svc, dp, store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-backend-migration
#[tokio::test]
async fn migrate_backend_rejects_versioned_file() {
    use file_storage::domain::error::DomainError;

    let (svc, dp, _store, _engine) = build_all_dual_backend(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload v1, bind it.
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    DataPlaneService,
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
        backends,
        issuer,
        Arc::clone(&authorizer) as Arc<dyn file_storage::domain::authz::Authorizer>,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    // `grace_secs` is unused by these tests but kept for signature symmetry
    // with the other `build_all*` helpers.
    let _ = grace_secs;
    (svc, dp, store, authorizer)
}

/// A non-admin caller may not migrate content onto a non-durable ("alt",
/// `InMemoryBackend`) target: `migrate_backend` must reject with `Forbidden`
/// and the version row must stay unchanged.
///
/// @cpt-cf-file-storage-fr-backend-migration
#[tokio::test]
async fn migrate_backend_rejects_non_durable_target_for_non_admin() {
    let (svc, dp, store, authorizer) = build_all_dual_backend_scoped(86400).await;
    authorizer.set_admin(false);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
///
/// @cpt-cf-file-storage-fr-backend-migration
#[tokio::test]
async fn migrate_backend_allows_non_durable_target_for_admin_scope() {
    let (svc, dp, store, authorizer) = build_all_dual_backend_scoped(86400).await;
    authorizer.set_admin(true);
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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

    let (backend_etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            part.part_number,
            part.offset,
            Bytes::from_static(data),
        )
        .await
        .expect("backend upload_part");
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

    msvc.complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap().unwrap_completed();
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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn sweep_after_complete_wins_does_not_delete_bound_version() {
    let (svc, _psvc, msvc, _dp, store, engine, backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
/// `complete_multipart_upload` call for the same session. The version is
/// deleted, the session is `aborted`, and a subsequent `complete` attempt is
/// rejected.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn sweep_before_complete_wins_cleans_up_expired_session() {
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
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

    // The pending version row must be gone.
    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap();
    assert!(
        version.is_none(),
        "pending version must be deleted once the sweep wins the session CAS"
    );

    // A subsequent complete attempt for the same upload_id must be rejected.
    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            file_storage::domain::error::DomainError::MultipartUploadNotInProgress { .. }
        ),
        "expected MultipartUploadNotInProgress after the sweep aborted the session, got {err:?}"
    );
}

/// Step 3's defense-in-depth, exercised independent of the sweep: a session
/// whose `expires_at` is already in the past but whose state is still
/// `in_progress` (no sweep tick has run at all) must be rejected by
/// `complete_multipart_upload` itself.
///
/// @cpt-cf-file-storage-fr-multipart-upload
#[tokio::test]
async fn complete_after_session_expired_is_rejected() {
    let (svc, _psvc, msvc, _dp, store, _engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn sweep_mid_flight_after_finalize_but_before_session_cas_does_not_delete_available_version()
{
    let (svc, _psvc, msvc, _dp, store, engine, _backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
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
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
#[tokio::test]
async fn sweep_step1_does_not_delete_version_finalized_between_list_and_delete() {
    let (svc, _psvc, _msvc, _dp, store, engine, backend) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
    backend
        .put(
            &candidate.backend_path,
            Bytes::from_static(b"finalized content"),
        )
        .await
        .unwrap();

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
    let blob = backend.get(&candidate.backend_path).await;
    assert!(
        blob.is_ok(),
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
///
/// @cpt-cf-file-storage-fr-upload-idempotency
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
        backends,
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
    let expired_ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let live_ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
///
/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-fr-file-events
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
// @cpt-cf-file-storage-fr-backend-migration

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
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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

/// A `StorageBackend` wrapper whose `put` runs a caller-supplied `FnOnce`
/// hook exactly once -- immediately before delegating to the real backend --
/// then never fires again. Used to model a second `migrate_backend` racer
/// committing its own CAS write in the narrow real-world window between this
/// call's destination `put()` and its own CAS attempt, deterministically and
/// in-process: the "other racer" runs synchronously as a side effect of this
/// call's own backend write, with no `sleep`/real concurrency involved.
struct RacingBackend {
    inner: Arc<dyn StorageBackend>,
    #[allow(clippy::type_complexity)]
    on_put: std::sync::Mutex<
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

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        let hook = self.on_put.lock().expect("on_put mutex").take();
        if let Some(hook) = hook {
            hook().await;
        }
        self.inner.put(path, bytes).await
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.inner.get(path).await
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
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
                hook_alt2
                    .put(&dest_path, hook_bytes.clone())
                    .await
                    .expect("racer's own blob write");
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
        on_put: std::sync::Mutex::new(Some(hook)),
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
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let ctx = ctx(tenant);
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    let winner_bytes = alt2_backend.get(&expected_dest_path).await.unwrap();
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
                hook_alt1
                    .put(&dest_path, hook_bytes.clone())
                    .await
                    .expect("racer's own blob write");
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
        on_put: std::sync::Mutex::new(Some(hook)),
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
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let ctx = ctx(tenant);
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    let stored_bytes = alt1_inner.get(&expected_dest_path).await.unwrap();
    assert_eq!(
        stored_bytes, content,
        "surviving blob must match the winner's bytes"
    );
}
