//! Audit-trail integration tests: one audit row per write, none on a rolled-back mutation.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
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

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-audit-test-{}.db", Uuid::now_v7().simple()));
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

#[tokio::test]
async fn finalize_upload_leaves_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
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

#[tokio::test]
async fn delete_file_leaves_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let file_id = ticket.file_id;

    svc.delete_file(&ctx, file_id, Some("*")).await.unwrap();

    // Audit rows survive file deletion (outbox, no FK cascade).
    let rows = store.list_audit(file_id).await.unwrap();
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

#[tokio::test]
async fn delete_version_leaves_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

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

/// A random `version_id` on a single-version file must 404, not delete the file.
#[tokio::test]
async fn delete_version_single_version_file_wrong_id_returns_not_found() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

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

    let wrong_version_id = Uuid::now_v7();
    let err = svc
        .delete_version(&ctx, t1.file_id, wrong_version_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::VersionNotFound { .. }),
        "expected VersionNotFound, got {err:?}"
    );

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), t1.file_id)
        .await
        .unwrap();
    assert!(file.is_some(), "files row must still exist");
    let version = store.get_version(t1.file_id, t1.version_id).await.unwrap();
    assert!(version.is_some(), "file_versions row must still exist");
}

/// Deleting the only version by its real id still deletes the whole file.
#[tokio::test]
async fn delete_version_single_version_file_matching_id_deletes_whole_file() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

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

    svc.delete_version(&ctx, t1.file_id, t1.version_id)
        .await
        .unwrap();

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), t1.file_id)
        .await
        .unwrap();
    assert!(file.is_none(), "files row must be gone");
}

#[tokio::test]
async fn multipart_complete_leaves_audit_rows() {
    // Expose the `MultipartStore` and `InMemoryBackend` to simulate the sidecar path
    // (`upload_part` + `upsert_multipart_part`).
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

    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    let (backend_etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            1,
            0,
            part_data,
        )
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

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
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

    assert_eq!(
        rows.iter()
            .filter(|r| r.operation == "finalize_version")
            .count(),
        1,
        "expected exactly 1 finalize_version audit row from multipart complete"
    );

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

    let _ = dp; // ensure dp is live throughout the test
}

/// A stale `expected_meta_version` rolls back the `meta_version` bump and the audit row together.
#[tokio::test]
async fn failed_metadata_cas_leaves_no_audit_row() {
    let (svc, _msvc, _dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let rows_before = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(rows_before.len(), 1);

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

    let rows_after = store.list_audit(ticket.file_id).await.unwrap();
    assert_eq!(
        rows_after.len(),
        rows_before.len(),
        "no new audit row should appear when the CAS rolls back"
    );
}

#[tokio::test]
async fn failed_bind_cas_leaves_no_audit_row() {
    let (svc, _msvc, dp, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

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

    let err = svc
        .bind(&ctx, t1.file_id, t2.version_id, Some("\"stale-etag\""))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed, got {err:?}"
    );

    let rows_after = store.list_audit(t1.file_id).await.unwrap();
    // The If-Match guard fails in the service layer before `bind_atomic`, so no CAS runs
    // and no audit row is written.
    assert_eq!(
        rows_after.len(),
        rows_before.len(),
        "no audit row should be written when bind fails the If-Match check"
    );
}
