//! Tests for P2-M3: multipart upload and upload idempotency.

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
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend,
};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = "gts.cf.fstorage.file.type.v1~x.test.v1~";

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-mp-test-{}.db", Uuid::now_v7().simple()));
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

/// Build both `FileService` (file CRUD + bind) and `MultipartService` (multipart
/// ops) sharing the same store, backends, and authorizer via `Clone` / `Arc`.
async fn build_service_with_config(
    idempotency_ttl_secs: u64,
) -> (Arc<FileService>, Arc<MultipartService>, DataPlaneService) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer = Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs,
    };
    let store = Store::new(Arc::clone(&db));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> = authorizer;
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
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc));
    (svc, msvc, dp)
}

async fn build_service() -> (Arc<FileService>, Arc<MultipartService>, DataPlaneService) {
    build_service_with_config(86400).await
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
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

// ── 1. Multipart happy path ──────────────────────────────────────────────────

/// @cpt-cf-file-storage-fr-multipart-upload
#[tokio::test]
async fn multipart_happy_path_in_memory() {
    let (svc, msvc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    // Create the file (pending, no content yet).
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    // Initiate multipart.
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "application/octet-stream")
        .await
        .unwrap();
    assert_eq!(session.file_id, ticket.file_id);
    assert_eq!(session.state.as_str(), "in_progress");

    // Upload two parts.
    let part1_data = Bytes::from_static(b"Hello, ");
    let part2_data = Bytes::from_static(b"World!");
    let p1 = msvc
        .upload_multipart_part(&ctx, ticket.file_id, session.upload_id, 1, part1_data)
        .await
        .unwrap();
    let p2 = msvc
        .upload_multipart_part(&ctx, ticket.file_id, session.upload_id, 2, part2_data)
        .await
        .unwrap();
    assert_eq!(p1.part_number, 1);
    assert_eq!(p2.part_number, 2);

    // Complete: marks the version available.
    msvc.complete_multipart_upload(&ctx, ticket.file_id, session.upload_id)
        .await
        .unwrap();

    // Bind the completed version.
    svc.bind(&ctx, ticket.file_id, session.version_id, None)
        .await
        .unwrap();

    // Read back via data plane.
    let content = dp
        .read_content(&ctx, ticket.file_id, session.version_id, None)
        .await
        .unwrap();
    assert_eq!(content, Bytes::from_static(b"Hello, World!"));
}

// ── 2. LocalFsBackend rejects multipart ──────────────────────────────────────

/// @cpt-cf-file-storage-fr-multipart-upload
#[tokio::test]
async fn multipart_rejected_on_local_fs() {
    let db = build_db().await;
    let tmp = std::env::temp_dir().join(format!("cf-fs-localfs-{}", Uuid::now_v7().simple()));
    std::fs::create_dir_all(&tmp).unwrap();
    let local: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("local-fs", &tmp));
    let backends = BackendRegistry::new(vec![local], "local-fs").expect("registry");
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
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> = authorizer;
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
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
    ));

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let err = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "application/octet-stream")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartNotSupported { .. }),
        "expected MultipartNotSupported, got {err:?}"
    );
}

// ── 3. Part re-upload is idempotent ──────────────────────────────────────────

/// @cpt-cf-file-storage-fr-multipart-upload
#[tokio::test]
async fn multipart_resumable_part_reupload() {
    let (svc, msvc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    let session = msvc
        .initiate_multipart_upload(&ctx, ticket.file_id, "application/octet-stream")
        .await
        .unwrap();

    // Upload part 1 twice — second upload wins.
    msvc.upload_multipart_part(
        &ctx,
        ticket.file_id,
        session.upload_id,
        1,
        Bytes::from_static(b"FIRST"),
    )
    .await
    .unwrap();
    msvc.upload_multipart_part(
        &ctx,
        ticket.file_id,
        session.upload_id,
        1,
        Bytes::from_static(b"SECOND"),
    )
    .await
    .unwrap();

    msvc.complete_multipart_upload(&ctx, ticket.file_id, session.upload_id)
        .await
        .unwrap();
    svc.bind(&ctx, ticket.file_id, session.version_id, None)
        .await
        .unwrap();

    let content = dp
        .read_content(&ctx, ticket.file_id, session.version_id, None)
        .await
        .unwrap();
    // Final content should be "SECOND" (the second upload of part 1 replaces the first).
    assert_eq!(content, Bytes::from_static(b"SECOND"));
}

// ── 4. Idempotency: same key returns same file ────────────────────────────────

/// @cpt-cf-file-storage-fr-upload-idempotency
#[tokio::test]
async fn idempotency_same_key_returns_same_file() {
    let (svc, _msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let mut nf = new_file();
    let owner_id = nf.owner_id;
    let key = "idem-key-1".to_owned();

    let t1 = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()))
        .await
        .unwrap();

    // Second request with the same key → same file_id returned.
    nf.owner_id = owner_id; // same owner
    let t2 = svc.create_file(&ctx, nf, Some(key)).await.unwrap();

    assert_eq!(
        t1.file_id, t2.file_id,
        "idempotent retry must return the same file_id"
    );
    assert_eq!(t1.version_id, t2.version_id);
}

// ── 5. Different owner → different file ──────────────────────────────────────

/// @cpt-cf-file-storage-fr-upload-idempotency
#[tokio::test]
async fn idempotency_different_owner_different_file() {
    let (svc, _msvc, _dp) = build_service().await;
    let tenant = Uuid::now_v7();
    let ctx_a = ctx(tenant);
    let ctx_b = ctx(tenant); // same tenant, different subject (different owner_id in NewFile)

    let key = "shared-key".to_owned();

    let mut nf_a = new_file();
    nf_a.owner_id = Uuid::now_v7();
    let mut nf_b = new_file();
    nf_b.owner_id = Uuid::now_v7(); // different owner_id

    let t_a = svc
        .create_file(&ctx_a, nf_a, Some(key.clone()))
        .await
        .unwrap();
    let t_b = svc.create_file(&ctx_b, nf_b, Some(key)).await.unwrap();

    assert_ne!(
        t_a.file_id, t_b.file_id,
        "different owners must get distinct files even with the same key"
    );
}

// ── 6. Idempotency expiry creates a fresh file ────────────────────────────────

/// @cpt-cf-file-storage-fr-upload-idempotency
#[tokio::test]
async fn idempotency_expiry_creates_new_file() {
    // Very short TTL: 1 second.
    let (svc, _msvc, _dp) = build_service_with_config(1).await;
    let ctx = ctx(Uuid::now_v7());
    let mut nf = new_file();
    let owner_id = nf.owner_id;

    let key = "expiry-key".to_owned();
    let t1 = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()))
        .await
        .unwrap();

    // Wait for the key to expire.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    nf.owner_id = owner_id;
    let t2 = svc.create_file(&ctx, nf, Some(key)).await.unwrap();

    assert_ne!(
        t1.file_id, t2.file_id,
        "after expiry, the same key must create a new file"
    );
}
