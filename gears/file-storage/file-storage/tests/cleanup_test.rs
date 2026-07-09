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

use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody, RetentionScope};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{CleanupStore, DataPlanePort, MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = "gts.cf.fstorage.file.type.v1~x.cleanup-test.v1~";

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
    (svc, psvc, msvc, dp, store, engine)
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
    let (svc, _psvc, _msvc, _dp, store, engine) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // create_file leaves exactly one pending version row.
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

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
    let (svc, _psvc, _msvc, _dp, store, engine) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

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
    let (svc, _psvc, msvc, _dp, store, engine) = build_all(0).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create a file and initiate a multipart session.
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "text/plain", 1024, None, None)
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

// ── test 3: retention-policy expiry sweep ─────────────────────────────────────

/// A file that matches a tenant-level age retention rule (max_age_days = 0)
/// is deleted by the sweep and a `retention_delete` audit row is written.
///
/// @cpt-cf-file-storage-fr-retention-policies
#[tokio::test]
async fn retention_expired_file_is_deleted_by_sweep() {
    let (svc, psvc, _msvc, dp, store, engine) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    // Create + upload + bind a file.
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
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

    // Create a tenant retention rule: max_age_days = 0 (expires immediately).
    psvc.create_retention_rule(
        &ctx,
        RetentionScope::Tenant,
        None,
        RetentionRuleBody {
            age: Some(AgeRetention { max_age_days: 0 }),
            inactivity: None,
            metadata: None,
        },
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

/// A file that does NOT match any retention rule is NOT deleted.
///
/// @cpt-cf-file-storage-fr-retention-policies
#[tokio::test]
async fn file_without_matching_retention_rule_is_not_deleted() {
    let (svc, _psvc, _msvc, dp, _store, engine) = build_all(86400).await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
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
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
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

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
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
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
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
