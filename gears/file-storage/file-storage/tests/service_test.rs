//! End-to-end control-plane service tests against a real temp-file `SQLite` DB
//! plus the in-memory storage backend and the tenant-only authorizer. These
//! exercise the full P1 flows (create, upload, bind, download, list, versions,
//! metadata, delete) as Rust calls, with no HTTP server.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::etag;
use file_storage::domain::ports::DataPlanePort;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::{Claims, Issuer};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile, OwnerFilter, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn build_service() -> (Arc<FileService>, DataPlaneService) {
    let (svc, dp, _store) = build_service_with_page_sizes(50, 1000).await;
    (svc, dp)
}

/// Like [`build_service`], but with caller-chosen `default_page_size`/
/// `max_page_size` — used by pagination tests that need a small
/// `max_page_size` to keep version-seeding fast (P2 2.2) — and also returns
/// the [`Store`] handle, for tests (P2 2.7) that need to drive the
/// store/repo layer directly to simulate a race the service's own
/// pre-transaction checks cannot reach deterministically in a single-threaded
/// test.
async fn build_service_with_page_sizes(
    default_page_size: u64,
    max_page_size: u64,
) -> (Arc<FileService>, DataPlaneService, Store) {
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
        default_page_size,
        max_page_size,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
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
    (svc, dp, store)
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

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    let versions = svc
        .list_versions(&ctx, ticket.file_id, None, 0)
        .await
        .unwrap();
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
    let t1 = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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

    // New version, then bind with a bogus If-Match.
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

    let t = svc.create_file(&ctx_a, new_file(), None, false).await.unwrap();
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
    let t = svc.create_file(&ctx, nf, None, false).await.unwrap();

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
    let t = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
    let t1 = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    let t = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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

    // No If-Match: precondition required.
    let err = svc
        .delete_file(&ctx, ticket.file_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed when If-Match absent, got {err:?}"
    );

    // Wrong ETag: precondition failed.
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
    let t = svc.create_file(&ctx, nf, None, false).await.unwrap();

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

// NOTE: `list_files_returns_each_files_custom_metadata` (exercising
// `FileService::list_metadata_for_files` batching) moved to the in-crate
// `domain::service::read_ops_tests` module (P2 remediation, item 13):
// `list_metadata_for_files` takes raw file ids with no `SecurityContext` of
// its own and trusts the caller to have already authorized them, so it was
// tightened from `pub` to `pub(crate)` -- which this external `tests/`
// integration binary, compiled against the crate's public API only, can no
// longer reach directly.

/// `GET /files/{id}/versions` must cap at `ServiceConfig::max_page_size` even
/// when a file has more versions than that — both with no explicit `limit`
/// (clamped to `max_page_size`) and with an explicit `limit` above
/// `max_page_size` — and must return the newest page (P2 2.2).
#[tokio::test]
async fn list_versions_caps_at_max_page_size() {
    // `default_page_size == max_page_size` so the no-explicit-limit case
    // below exercises the `max_page_size` cap directly (per the plan: "call
    // `list_versions` with no explicit limit, assert the returned length
    // equals `max_page_size`").
    let max_page_size = 10u64;
    let (svc, dp, _store) = build_service_with_page_sizes(max_page_size, max_page_size).await;
    let ctx = ctx(Uuid::now_v7());

    let total = max_page_size + 5;
    let mut created = Vec::with_capacity(usize::try_from(total).expect("total fits usize"));

    let t0 = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    dp.put_content(
        &ctx,
        t0.file_id,
        t0.version_id,
        "text/plain",
        Bytes::from_static(b"v0"),
    )
    .await
    .unwrap();
    created.push(t0.version_id);

    for i in 1..total {
        let t = svc.presign_version(&ctx, t0.file_id).await.unwrap();
        dp.put_content(
            &ctx,
            t0.file_id,
            t.version_id,
            "text/plain",
            Bytes::from(format!("v{i}")),
        )
        .await
        .unwrap();
        created.push(t.version_id);
    }
    assert_eq!(
        created.len() as u64,
        total,
        "sanity: seeded max_page_size + 5"
    );

    // No explicit limit → clamps to max_page_size (primary outcome).
    let page = svc.list_versions(&ctx, t0.file_id, None, 0).await.unwrap();
    assert_eq!(page.len() as u64, max_page_size);

    // Secondary artifact: the page is exactly the newest `max_page_size`
    // versions, newest-first — the last `max_page_size` created ids, in
    // reverse creation order.
    let expected: Vec<Uuid> = created
        .iter()
        .rev()
        .take(usize::try_from(max_page_size).expect("max_page_size fits usize"))
        .copied()
        .collect();
    let actual: Vec<Uuid> = page.iter().map(|v| v.version_id).collect();
    assert_eq!(actual, expected, "must be the newest-first page");

    // A caller-supplied limit above max_page_size is still clamped.
    let clamped = svc
        .list_versions(&ctx, t0.file_id, Some(max_page_size + 100), 0)
        .await
        .unwrap();
    assert_eq!(clamped.len() as u64, max_page_size);
}

// ── P2 2.7: delete_version vs bind race cannot dangle `files.content_id` ────

/// Deleting the version `content_id` currently points at must be rejected
/// (`Conflict`/`version_not_found`), and the row must survive.
///
/// @cpt-cf-file-storage-fr-audit-trail
#[tokio::test]
async fn delete_current_version_is_rejected() {
    let (svc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    // v1 = A, bound as current.
    let t1 = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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

    // v2 = B, a second version, so the file is multi-version but A stays current.
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

    // Deleting A (current) must be rejected, not silently succeed.
    let err = svc
        .delete_version(&ctx, t1.file_id, t1.version_id)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::Conflict { .. } | DomainError::VersionNotFound { .. }
        ),
        "expected Conflict or VersionNotFound, got {err:?}"
    );

    // The row must survive and content_id must still resolve.
    let versions = svc.list_versions(&ctx, t1.file_id, None, 0).await.unwrap();
    assert!(
        versions.iter().any(|v| v.version_id == t1.version_id),
        "current version must survive the rejected delete"
    );
    let file = svc.get_file(&ctx, t1.file_id).await.unwrap();
    assert_eq!(file.content_id, Some(t1.version_id));
}

/// Deterministic reproduction of the pre-fix dangle: a version's "is this
/// current?" check (as `delete_version` performs it, against a pre-transaction
/// snapshot) can go stale if a concurrent `bind` promotes that exact version
/// to current afterwards. This drives the store/repo layer directly, in the
/// exact order such a race would leave things, to prove the DB-level guard
/// (P2 2.7) refuses the delete instead of leaving `files.content_id` dangling
/// — the outer service call cannot reach this window in a single-threaded
/// test, since its own checks are always freshly re-read.
#[tokio::test]
async fn delete_version_then_bind_cannot_dangle() {
    let (svc, dp, store) = build_service_with_page_sizes(50, 1000).await;
    let ctx = ctx(Uuid::now_v7());

    // v1 = A, bound as current.
    let t1 = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
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

    // v2 = B, uploaded but not yet current — this is the version a would-be
    // "T1" observed as non-current before racing ahead to delete it.
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
    let pre_bind = store.get_version(t1.file_id, t2.version_id).await.unwrap();
    assert_eq!(pre_bind.map(|v| v.is_current), Some(false));

    // "T2" wins the race: bind promotes B to current before T1's delete lands.
    let cur = svc.get_file(&ctx, t1.file_id).await.unwrap();
    svc.bind(
        &ctx,
        t1.file_id,
        t2.version_id,
        etag::etag_for(&cur).as_deref(),
    )
    .await
    .unwrap();

    // "T1"'s delete of B now executes, against the store directly (bypassing
    // FileService::delete_version's own — necessarily fresh, in this
    // single-threaded test — pre-check) to simulate the delete landing after
    // the interleaved bind. Pre-fix this deleted the row unconditionally;
    // post-fix the DB-level `is_current = false` guard refuses it.
    let audit = AuditEntry::success(
        ctx.subject_tenant_id(),
        "user",
        ctx.subject_id(),
        Some(t1.file_id),
        AuditOperation::DeleteVersion,
        serde_json::json!({ "version_id": t2.version_id }),
    );
    let removed = store
        .delete_version(t1.file_id, t2.version_id, audit)
        .await
        .unwrap();
    assert!(
        !removed,
        "guarded delete of the (now-current) version must not remove the row"
    );

    // content_id must always resolve to an existing version row.
    let file = svc.get_file(&ctx, t1.file_id).await.unwrap();
    assert_eq!(file.content_id, Some(t2.version_id));
    let still_there = store.get_version(t1.file_id, t2.version_id).await.unwrap();
    assert!(
        still_there.is_some(),
        "content_id must never dangle at a deleted version"
    );
}

/// P2 1.11: `download_url`'s minted `op = get` token must carry the
/// version's real stored MIME and content ETag as `content_type`/`etag`
/// claims, so the sidecar (no DB access) can echo real `Content-Type`/`ETag`
/// response headers. Decodes the token's base64url JSON payload directly —
/// acceptable in a test asserting the minter's own internal contract, even
/// though the token is otherwise opaque to every other party (ADR-0004's
/// Token Opacity Contract).
#[tokio::test]
async fn download_url_token_carries_version_mime_and_etag() {
    let (svc, dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"hello world"),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let dl = svc.download_url(&ctx, ticket.file_id, None).await.unwrap();

    let token = dl
        .download_url
        .split("fs-token=")
        .nth(1)
        .expect("download_url carries an fs-token query param");
    let payload_b64 = token
        .split('.')
        .next()
        .expect("token has a payload segment");
    let payload = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .expect("valid base64url payload");
    let claims: Claims = serde_json::from_slice(&payload).expect("valid claims JSON");

    assert_eq!(claims.content_type, "text/plain", "declared file mime_type");
    let expected_etag = etag::content_etag(ticket.file_id, ticket.version_id);
    assert_eq!(claims.etag, expected_etag);
    assert_eq!(
        claims.etag, dl.etag,
        "one source of truth as the ticket's own etag"
    );
}
