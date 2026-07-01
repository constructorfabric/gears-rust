//! Audit-trail integration tests.
//!
//! Verifies:
//! 1. Each write operation (create, finalize, bind, update_metadata, delete_file,
//!    delete_version, multipart complete) leaves **exactly one** audit row for its
//!    primary operation.
//! 2. A rolled-back mutation (failed metadata CAS) leaves **zero** audit rows —
//!    proving that the audit row and the mutation share a single transaction.
//!
//! @cpt-cf-file-storage-fr-audit-trail
//! @cpt-cf-file-storage-nfr-audit-completeness

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::{DataPlanePort, MultipartStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataPatch, NewFile, OwnerKind};

const GTS: &str = "gts.cf.fstorage.file.type.v1~x.test.v1~";

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-audit-test-{}.db", Uuid::now_v7().simple()));
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

async fn build_service() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    DataPlaneService,
    Store,
) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
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
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    (svc, msvc, dp, store)
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
        name: "audit.txt".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "text/plain".to_owned(),
        custom_metadata: vec![],
    }
}

// ── 1. create_file leaves exactly one "create" audit row ───────────────────────

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn create_file_leaves_one_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let rows = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(rows.len(), 1, "expected exactly 1 audit row after create");
    assert_eq!(rows[0].operation, "create");
    assert_eq!(rows[0].outcome, "success");
    assert_eq!(rows[0].file_id, Some(ticket.file_id));
}

// ── 2. finalize_upload leaves a "finalize_version" audit row ──────────────────

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn finalize_upload_leaves_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    // put_content calls finalize_upload internally.
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"hello"),
    )
    .await
    .unwrap();

    let rows = store.list_audit(ticket.file_id).await.unwrap();
    let finalize_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "finalize_version")
        .collect();
    assert_eq!(
        finalize_rows.len(),
        1,
        "expected exactly 1 finalize_version audit row"
    );
    assert_eq!(finalize_rows[0].outcome, "success");
}

// ── 3. bind leaves a "patch_content" audit row ────────────────────────────────

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn bind_leaves_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

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

    let rows = store.list_audit(ticket.file_id).await.unwrap();
    let bind_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "patch_content")
        .collect();
    assert_eq!(
        bind_rows.len(),
        1,
        "expected exactly 1 patch_content audit row"
    );
    assert_eq!(bind_rows[0].outcome, "success");
}

// ── 4. update_metadata leaves a "patch_metadata" audit row ────────────────────

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn update_metadata_leaves_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let patch = CustomMetadataPatch {
        entries: vec![("k".to_owned(), Some("v".to_owned()))],
    };
    svc.update_metadata(&ctx, ticket.file_id, patch, None)
        .await
        .unwrap();

    let rows = store.list_audit(ticket.file_id).await.unwrap();
    let meta_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "patch_metadata")
        .collect();
    assert_eq!(
        meta_rows.len(),
        1,
        "expected exactly 1 patch_metadata audit row"
    );
    assert_eq!(meta_rows[0].outcome, "success");
}

// ── 5. delete_file leaves a "delete_file" audit row ──────────────────────────

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn delete_file_leaves_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let file_id = ticket.file_id;

    // Use wildcard If-Match (file has no bound content yet).
    svc.delete_file(&ctx, file_id, Some("*")).await.unwrap();

    // File is gone but audit rows must survive (outbox, not FK-cascaded).
    let rows = store.list_audit(file_id).await.unwrap();
    // There is a "create" row and a "delete_file" row.
    let delete_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "delete_file")
        .collect();
    assert_eq!(
        delete_rows.len(),
        1,
        "expected exactly 1 delete_file audit row"
    );
    assert_eq!(delete_rows[0].outcome, "success");
}

// ── 6. delete_version leaves a "delete_version" audit row ────────────────────

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn delete_version_leaves_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    // Create + upload v1 and bind it.
    let t1 = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        t1.file_id,
        t1.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, t1.file_id, t1.version_id, None)
        .await
        .unwrap();

    // Presign v2 + upload, bind v2 so v1 is no longer current.
    let t2 = svc.presign_version(&ctx, t1.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        t1.file_id,
        t2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .unwrap();
    let cur = svc.get_file(&ctx, t1.file_id).await.unwrap();
    svc.bind(
        &ctx,
        t1.file_id,
        t2.version_id,
        file_storage::domain::etag::etag_for(&cur).as_deref(),
    )
    .await
    .unwrap();

    // Now delete v1 (non-current).
    svc.delete_version(&ctx, t1.file_id, t1.version_id)
        .await
        .unwrap();

    let rows = store.list_audit(t1.file_id).await.unwrap();
    let del_ver_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "delete_version")
        .collect();
    assert_eq!(
        del_ver_rows.len(),
        1,
        "expected exactly 1 delete_version audit row"
    );
    assert_eq!(del_ver_rows[0].outcome, "success");
}

// -- 7. multipart complete leaves audit rows ----------------------------------

/// @cpt-cf-file-storage-fr-audit-trail
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn multipart_complete_leaves_audit_rows() {
    // Build a custom setup that exposes both the MultipartStore and the
    // InMemoryBackend directly, so the test can simulate the sidecar path
    // (upload_part + upsert_multipart_part) without going through the removed
    // control-plane byte route (ADR-0003 / FEATURE §8 migration).
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
    let multipart_store: Arc<dyn file_storage::domain::ports::MultipartStore> =
        Arc::new(store.clone());
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
        Arc::clone(&multipart_store),
        backends.clone(),
        Arc::clone(&authorizer),
        None,
        Arc::clone(&issuer),
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    // Declare total size = 5 bytes ("part1").
    let part_data = Bytes::from_static(b"part1");
    let declared_size: u64 = part_data.len() as u64;

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            None,
        )
        .await
        .unwrap();

    // Retrieve the backend handle and simulate the sidecar part write.
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    let (backend_etag, part_hash) = backend
        .upload_part(&backend_path, &session.backend_upload_handle, 1, part_data)
        .await
        .unwrap();
    let size = i64::try_from(declared_size).unwrap();
    multipart_store
        .upsert_multipart_part(
            plan.upload_id,
            1,
            &backend_etag,
            part_hash,
            size,
            time::OffsetDateTime::now_utc(),
        )
        .await
        .unwrap();

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();

    let rows = store.list_audit(ticket.file_id).await.unwrap();
    let complete_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.operation == "multipart_complete")
        .collect();
    assert_eq!(
        complete_rows.len(),
        1,
        "expected exactly 1 multipart_complete audit row"
    );
    assert_eq!(complete_rows[0].outcome, "success");

    // There is also a finalize_version row from the complete flow.
    assert_eq!(
        rows.iter()
            .filter(|r| r.operation == "finalize_version")
            .count(),
        1,
        "expected exactly 1 finalize_version audit row from multipart complete"
    );

    // Also verify bind after multipart still adds exactly 1 more patch_content row.
    svc.bind(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();
    let rows2 = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(
        rows2
            .iter()
            .filter(|r| r.operation == "patch_content")
            .count(),
        1,
        "expected exactly 1 patch_content row"
    );

    // dp.put_content is not called separately here (multipart path doesn't use it).
    let _ = dp; // ensure dp is live throughout the test
}

// ── 8. Failed metadata CAS leaves NO audit row (atomicity proof) ──────────────

/// A stale `expected_meta_version` causes the CAS to roll back the entire
/// transaction (both the `meta_version` bump and the audit row). This proves
/// the same-transaction guarantee of `cpt-cf-file-storage-nfr-audit-completeness`.
///
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn failed_metadata_cas_leaves_no_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    // There is 1 audit row: the "create".
    let rows_before = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(rows_before.len(), 1);

    // Attempt a metadata update with a wrong meta_version (stale: 99).
    let patch = CustomMetadataPatch {
        entries: vec![("x".to_owned(), Some("y".to_owned()))],
    };
    let err = svc
        .update_metadata(&ctx, ticket.file_id, patch, Some(99))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed, got {err:?}"
    );

    // The failed CAS must NOT have written any new audit row.
    let rows_after = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(
        rows_after.len(),
        rows_before.len(),
        "no new audit row should appear when the CAS rolls back"
    );
}

// ── 9. Failed bind CAS leaves NO audit row ────────────────────────────────────

/// A stale ETag on bind rolls back the whole transaction; no audit row should
/// be emitted.
///
/// @cpt-cf-file-storage-nfr-audit-completeness
#[tokio::test]
async fn failed_bind_cas_leaves_no_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    // Bind v1 successfully.
    let t1 = svc.create_file(&ctx, new_file(), None).await.unwrap();
    dp.put_content(
        &ctx,
        t1.file_id,
        t1.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, t1.file_id, t1.version_id, None)
        .await
        .unwrap();

    // Presign v2 and finalize it, but attempt to bind with a stale ETag.
    let t2 = svc.presign_version(&ctx, t1.file_id).await.unwrap();
    dp.put_content(
        &ctx,
        t1.file_id,
        t2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .unwrap();

    let rows_before = store.list_audit(t1.file_id).await.unwrap();

    // Wrong ETag → bind fails (precondition), CAS transaction rolls back.
    let err = svc
        .bind(&ctx, t1.file_id, t2.version_id, Some("\"stale-etag\""))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed, got {err:?}"
    );

    let rows_after = store.list_audit(t1.file_id).await.unwrap();
    // Note: the failed bind check happens BEFORE calling bind_atomic (the If-Match
    // guard is checked in the service layer). So the CAS never runs → no rollback
    // needed, but also no audit row written.
    assert_eq!(
        rows_after.len(),
        rows_before.len(),
        "no audit row should be written when bind fails the If-Match check"
    );
}
