//! End-to-end control-plane service tests against a real temp-file `SQLite` DB
//! plus the in-memory storage backend and the tenant-only authorizer. These
//! exercise the full P1 flows (create, upload, bind, download, list, versions,
//! metadata, delete) as Rust calls, with no HTTP server.

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
use file_storage::domain::etag;
use file_storage::domain::ports::DataPlanePort;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile, OwnerFilter, OwnerKind};

const GTS: &str = "gts.cf.fstorage.file.type.v1~x.test.v1~";

async fn build_service() -> (Arc<FileService>, DataPlaneService) {
    // A unique temp *file* DB: the service opens a connection per call, so every
    // connection must see the same database. A bare `sqlite::memory:` gives each
    // pooled connection its own empty DB; a temp file is shared by construction.
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-test-{}.db", Uuid::now_v7().simple()));
    // Build a cross-platform SQLite file URL: forward slashes only, and an
    // absolute path must lead with '/' so the URL keeps sqlite's triple-slash
    // form. On Windows `C:\dir\f.db` becomes `/C:/dir/f.db`; on Unix the path
    // already starts with '/'.
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
    let db: Arc<DBProvider<DbError>> = Arc::new(DBProvider::new(db));

    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
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
    let svc = Arc::new(FileService::new(
        store, backends, issuer, authorizer, cfg, None, None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    (svc, dp)
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
        name: "doc.txt".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "text/plain".to_owned(),
        custom_metadata: vec![CustomMetadataEntry {
            key: "tag".to_owned(),
            value: "a".to_owned(),
        }],
    }
}

/// create → upload bytes → bind → the file now has content + an ETag.
#[tokio::test]
async fn full_upload_bind_download_lifecycle() {
    let (svc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();
    assert!(ticket.upload_url.contains("fs-token="), "signed upload URL");
    assert!(ticket.upload_url.starts_with("http://sidecar.test"));

    // Before bind there is no content.
    let pre = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    assert!(pre.content_id.is_none());
    assert_eq!(etag::etag_for(&pre), None);

    // Upload bytes (in-process equivalent of the sidecar stream-and-finalize).
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"hello world"),
    )
    .await
    .unwrap();

    // Bind the uploaded version as current (first bind: no If-Match).
    let bound = svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();
    assert_eq!(bound.content_id, Some(ticket.version_id));
    let etag = etag::etag_for(&bound).expect("etag after bind");
    assert!(etag.starts_with('"') && etag.ends_with('"'));

    // download-url pins the current content + returns its ETag.
    let dl = svc.download_url(&ctx, ticket.file_id, None).await.unwrap();
    assert_eq!(dl.version_id, ticket.version_id);
    assert_eq!(dl.etag, etag);
    assert!(dl.download_url.contains("fs-token="));

    // Read the bytes back through the backend.
    let bytes = dp
        .read_content(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();
    assert_eq!(bytes, Bytes::from_static(b"hello world"));

    // Versions: exactly one, current, available.
    let versions = svc.list_versions(&ctx, ticket.file_id).await.unwrap();
    assert_eq!(versions.len(), 1);
    assert!(versions[0].is_current);
    assert_eq!(versions[0].size, 11);

    // Custom metadata round-trips.
    let (_f, meta) = svc
        .get_file_with_metadata(&ctx, ticket.file_id)
        .await
        .unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0].key, "tag");
}

#[tokio::test]
async fn bind_with_wrong_if_match_returns_precondition_failed() {
    let (svc, dp) = build_service().await;
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

    // New version, then bind with a bogus If-Match → 412.
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
    let err = svc
        .bind(&ctx, t1.file_id, t2.version_id, Some("\"deadbeef\""))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "got {err:?}"
    );

    // Binding with the correct current ETag succeeds.
    let current = svc.get_file(&ctx, t1.file_id).await.unwrap();
    let etag = etag::etag_for(&current).unwrap();
    let bound = svc
        .bind(&ctx, t1.file_id, t2.version_id, Some(&etag))
        .await
        .unwrap();
    assert_eq!(bound.content_id, Some(t2.version_id));
}

#[tokio::test]
async fn tenant_isolation_hides_other_tenants_files() {
    let (svc, _dp) = build_service().await;
    let ctx_a = ctx(Uuid::now_v7());
    let ctx_b = ctx(Uuid::now_v7());

    let t = svc.create_file(&ctx_a, new_file(), None).await.unwrap();
    // Tenant B cannot see tenant A's file.
    let err = svc.get_file(&ctx_b, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
    // Tenant A can.
    assert!(svc.get_file(&ctx_a, t.file_id).await.is_ok());
}

#[tokio::test]
async fn content_type_mismatch_is_rejected() {
    let (svc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let mut nf = new_file();
    nf.mime_type = "image/png".to_owned();
    let t = svc.create_file(&ctx, nf, None).await.unwrap();

    // Declared png, but the bytes are a PDF signature → mismatch.
    let err = dp
        .put_content(
            &ctx,
            t.file_id,
            t.version_id,
            "image/png",
            Bytes::from_static(b"%PDF-1.4\n%%EOF"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MimeMismatch { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn update_metadata_merges_and_bumps_meta_version() {
    let (svc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc.create_file(&ctx, new_file(), None).await.unwrap();

    let patch = CustomMetadataPatch {
        entries: vec![
            ("tag".to_owned(), Some("b".to_owned())),     // overwrite
            ("color".to_owned(), Some("red".to_owned())), // add
        ],
    };
    let updated = svc
        .update_metadata(&ctx, t.file_id, patch, None)
        .await
        .unwrap();
    assert_eq!(updated.meta_version, 1, "meta_version bumped");

    let (_f, meta) = svc.get_file_with_metadata(&ctx, t.file_id).await.unwrap();
    let map: std::collections::BTreeMap<_, _> =
        meta.into_iter().map(|e| (e.key, e.value)).collect();
    assert_eq!(map.get("tag"), Some(&"b".to_owned()));
    assert_eq!(map.get("color"), Some(&"red".to_owned()));

    // Delete a key via merge patch (null).
    let del = CustomMetadataPatch {
        entries: vec![("color".to_owned(), None)],
    };
    svc.update_metadata(&ctx, t.file_id, del, None)
        .await
        .unwrap();
    let (_f, meta2) = svc.get_file_with_metadata(&ctx, t.file_id).await.unwrap();
    assert!(meta2.iter().all(|e| e.key != "color"), "color removed");
}

#[tokio::test]
async fn restore_prior_version_rebinds_pointer() {
    let (svc, dp) = build_service().await;
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
        etag::etag_for(&cur).as_deref(),
    )
    .await
    .unwrap();

    // Restore v1 (a pointer swap, no re-upload).
    let restored = svc
        .restore_version(&ctx, t1.file_id, t1.version_id)
        .await
        .unwrap();
    assert_eq!(restored.content_id, Some(t1.version_id));
}

#[tokio::test]
async fn delete_file_then_get_returns_not_found() {
    let (svc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc.create_file(&ctx, new_file(), None).await.unwrap();
    // No bound content yet: use "*" (wildcard If-Match).
    svc.delete_file(&ctx, t.file_id, Some("*")).await.unwrap();
    let err = svc.get_file(&ctx, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn download_url_pending_version_is_rejected() {
    let (svc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    // Create a file — the first version is pending (upload not yet finalized).
    let ticket = svc.create_file(&ctx, new_file(), None).await.unwrap();

    // Requesting a signed URL for a pending version must fail with Conflict.
    let err = svc
        .download_url(&ctx, ticket.file_id, Some(ticket.version_id))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict for pending version, got {err:?}"
    );
}

#[tokio::test]
async fn delete_file_if_match_required_and_enforced() {
    let (svc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    // Create, upload, and bind a file so it has a real content ETag.
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
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let file = svc.get_file(&ctx, ticket.file_id).await.unwrap();
    let current_etag = etag::etag_for(&file).expect("file must have an ETag after bind");

    // No If-Match → 412 (required).
    let err = svc
        .delete_file(&ctx, ticket.file_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed when If-Match absent, got {err:?}"
    );

    // Wrong ETag → 412.
    let err = svc
        .delete_file(&ctx, ticket.file_id, Some("\"wrong-etag\""))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed for wrong ETag, got {err:?}"
    );

    // Correct ETag → success.
    svc.delete_file(&ctx, ticket.file_id, Some(&current_etag))
        .await
        .unwrap();
    let err = svc.get_file(&ctx, ticket.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "file must be gone after successful delete, got {err:?}"
    );
}

#[tokio::test]
async fn list_files_filters_by_owner() {
    let (svc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();
    let mut nf = new_file();
    nf.owner_id = owner;
    let t = svc.create_file(&ctx, nf, None).await.unwrap();

    let found = svc
        .list_files(
            &ctx,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id: owner,
            },
            Some(10),
            0,
        )
        .await
        .unwrap();
    assert!(found.iter().any(|f| f.file_id == t.file_id));

    let empty = svc
        .list_files(
            &ctx,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id: Uuid::now_v7(),
            },
            Some(10),
            0,
        )
        .await
        .unwrap();
    assert!(empty.is_empty());
}
