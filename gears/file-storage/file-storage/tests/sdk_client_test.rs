//! Tests for `FileStorageLocalClient` (the SDK Level-1 in-process client),
//! `gears/file-storage/file-storage-sdk`.
//!
//! Three kinds of coverage, per method where applicable:
//! 1. Success + one characteristic failure (the same `CanonicalError`
//!    category the REST API would answer for the identical operation —
//!    404/403/409/412-shaped, via the shared `From<DomainError> for
//!    CanonicalError` ladder both surfaces go through).
//! 2. REST-equivalence: `create` → `get`, `bind`, and `delete` with a wrong
//!    `If-Match` produce the same outcome (success shape or error category)
//!    whether driven through the SDK client or through the real REST
//!    handlers (`api::rest::handlers`, wired into a minimal `axum::Router`
//!    exactly like `tests/api_handlers_test.rs` does).
//! 3. The "service owner" scenario: an `app`-typed `SecurityContext` creates
//!    a file under its own `(owner_kind: app, owner_id: <self>)` without
//!    `ADMIN_POLICY`, uploads (writes bytes to the backend + finalizes,
//!    mirroring what the sidecar does — the same simulation
//!    `tests/finalize_test.rs` and `tests/common/mod.rs` use), and reads it
//!    back; a cross-owner create (`owner_id != subject_id`) still requires
//!    `ADMIN_POLICY`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::authz::{Authorizer, actions};
use file_storage::domain::error::DomainError;
use file_storage::domain::local_client::FileStorageLocalClient;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{MultipartStore, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend,
};
use file_storage::infra::content::hash;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{
    CreateFileOutcome, CustomMetadataEntry, CustomMetadataPatch, FileFetch, FileStorageClientV1,
    FileStorageError, MultipartCompleteOutcome, NewFile, OwnerFilter, OwnerKind, PolicyBody,
    PolicyScope, RetentionRuleBody, RetentionScope,
};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");
const BASE: &str = "/api/file-storage/v1";
const DEFAULT_BACKEND: &str = "mem";
const DURABLE_BACKEND: &str = "fs";

// ── shared test authorizer ───────────────────────────────────────────────────

/// Grants `READ`/`WRITE`/`DELETE` unconditionally, but gates `ADMIN_POLICY`
/// behind `is_admin` and (for the handful of methods with no other natural
/// failure mode) `READ` behind `deny_read`. Mirrors `ScopedTestAuthorizer` in
/// `tests/list_authz_test.rs`/`tests/policy_authz_test.rs` — duplicated here
/// rather than shared, following those files' own precedent (each
/// `tests/*.rs` file is a separate compilation unit).
#[derive(Default)]
struct ScopedTestAuthorizer {
    is_admin: AtomicBool,
    deny_read: AtomicBool,
}

impl ScopedTestAuthorizer {
    fn set_admin(&self, admin: bool) {
        self.is_admin.store(admin, Ordering::SeqCst);
    }

    fn set_deny_read(&self, deny: bool) {
        self.deny_read.store(deny, Ordering::SeqCst);
    }
}

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::ADMIN_POLICY {
            return if self.is_admin.load(Ordering::SeqCst) {
                Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
            } else {
                Err(DomainError::Forbidden)
            };
        }
        if action == actions::READ && self.deny_read.load(Ordering::SeqCst) {
            return Err(DomainError::Forbidden);
        }
        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

// ── harness ──────────────────────────────────────────────────────────────────

struct Harness {
    client: FileStorageLocalClient,
    file_svc: Arc<FileService>,
    multipart_svc: Arc<MultipartService>,
    backend: Arc<dyn StorageBackend>,
    multipart_store: Arc<dyn MultipartStore>,
    authz: Arc<ScopedTestAuthorizer>,
}

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-sdk-client-test-{}.db",
        Uuid::now_v7().simple()
    ));
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

async fn build_harness() -> Harness {
    let db = build_db().await;

    // `mem` (default): `multipart_native: true`, non-durable — exercises the
    // multipart paths. `fs`: durable — the `migrate_backend` success target
    // (a durable destination needs no `ADMIN_POLICY` elevation).
    let mem: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new(DEFAULT_BACKEND));
    let mut fs_root = std::env::temp_dir();
    fs_root.push(format!(
        "cf-fs-sdk-client-fsroot-{}",
        Uuid::now_v7().simple()
    ));
    let fs: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new(
        DURABLE_BACKEND,
        fs_root.to_string_lossy().as_ref(),
    ));
    let backends =
        BackendRegistry::new(vec![Arc::clone(&mem), fs], DEFAULT_BACKEND).expect("registry");

    let issuer = Arc::new(file_storage::infra::signed_url::Issuer::generate(3600).expect("issuer"));
    let authz = Arc::new(ScopedTestAuthorizer::default());
    let authorizer: Arc<dyn Authorizer> = Arc::clone(&authz) as Arc<dyn Authorizer>;
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };

    let store = Store::new(Arc::clone(&db));
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());

    let file_svc = Arc::new(FileService::new(
        store,
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let multipart_svc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    let policy_svc = Arc::new(PolicyService::new(policy_store, authorizer));

    let client = FileStorageLocalClient::new(
        Arc::clone(&file_svc),
        Arc::clone(&multipart_svc),
        Arc::clone(&policy_svc),
    );

    Harness {
        client,
        file_svc,
        multipart_svc,
        backend: mem,
        multipart_store,
        authz,
    }
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
}

fn ctx_with(tenant: Uuid, subject: Uuid, subject_type: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .subject_type(subject_type)
        .build()
        .expect("valid SecurityContext")
}

fn new_file(owner_kind: OwnerKind, owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind,
        owner_id,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

async fn write_bytes(backend: &Arc<dyn StorageBackend>, path: &str, data: &'static [u8]) {
    use futures::stream::{self, BoxStream};
    let bytes = Bytes::from_static(data);
    let len = bytes.len() as u64;
    let stream: BoxStream<'static, std::io::Result<Bytes>> =
        Box::pin(stream::once(async move { Ok(bytes) }));
    backend
        .put_stream(path, stream, Some(len))
        .await
        .expect("put_stream");
}

/// Create a file (single-part, `auto_bind: false`), then simulate what the
/// sidecar does after a successful `PUT`: write the bytes straight to the
/// backend and call `FileService::finalize_upload` directly under the same
/// `ctx` — mirroring `tests/finalize_test.rs`'s own simulation (no HTTP, no
/// signed-token round trip; that machinery is exercised elsewhere and is
/// orthogonal to what this file tests). Returns `(file_id, version_id)` of
/// the now-`Available` (but not yet bound) version.
async fn create_and_finalize(
    h: &Harness,
    ctx: &SecurityContext,
    owner_kind: OwnerKind,
    owner_id: Uuid,
    data: &'static [u8],
) -> (Uuid, Uuid) {
    let outcome = h
        .client
        .create_file(ctx, new_file(owner_kind, owner_id), None, false, None)
        .await
        .expect("create_file");
    let ticket = match outcome {
        CreateFileOutcome::SinglePart(t) => t,
        CreateFileOutcome::Multipart { .. } => panic!("expected single-part outcome"),
    };
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_bytes(&h.backend, &path, data).await;
    h.file_svc
        .finalize_upload(
            ctx,
            ticket.file_id,
            ticket.version_id,
            i64::try_from(data.len()).unwrap(),
            hash::sha256(data),
        )
        .await
        .expect("finalize_upload");
    (ticket.file_id, ticket.version_id)
}

// ── per-method: success + one characteristic failure ────────────────────────

#[tokio::test]
async fn create_file_success_and_policy_rejection() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, true, None)
        .await
        .expect("create_file succeeds with no policy configured");
    assert!(matches!(outcome, CreateFileOutcome::SinglePart(_)));

    // Restrict allowed mime types to something that excludes the default
    // "application/octet-stream" test fixture — the same
    // `PolicyMimeNotAllowed` → `InvalidArgument` category the REST
    // `POST /files` answers as `400`.
    h.authz.set_admin(true);
    h.client
        .put_policy(
            &ctx,
            PolicyScope::Tenant,
            None,
            PolicyBody {
                allowed_mime_types: vec!["image/*".to_owned()],
                ..Default::default()
            },
        )
        .await
        .expect("put_policy");
    h.authz.set_admin(false);

    let err = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, true, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::InvalidArgument { .. }),
        "expected InvalidArgument, got {err:?}"
    );
}

#[tokio::test]
async fn get_file_success_and_not_found() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, _) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"hello").await;

    let fetch = h
        .client
        .get_file(&ctx, file_id, None)
        .await
        .expect("get_file");
    match fetch {
        FileFetch::Modified { record, .. } => assert_eq!(record.file.file_id, file_id),
        FileFetch::NotModified => panic!("expected Modified"),
    }

    let err = h
        .client
        .get_file(&ctx, Uuid::now_v7(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn get_file_if_none_match_returns_not_modified() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, version_id) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"hi").await;
    h.client
        .bind(&ctx, file_id, version_id, None)
        .await
        .expect("bind");

    let fetch = h
        .client
        .get_file(&ctx, file_id, None)
        .await
        .expect("get_file");
    let etag = match fetch {
        FileFetch::Modified { etag, .. } => etag.expect("bound file has an etag"),
        FileFetch::NotModified => panic!("expected Modified"),
    };

    let fetch2 = h
        .client
        .get_file(&ctx, file_id, Some(&etag))
        .await
        .expect("get_file with If-None-Match");
    assert!(matches!(fetch2, FileFetch::NotModified));
}

#[tokio::test]
async fn list_files_success_and_cross_owner_forbidden() {
    let h = build_harness().await;
    let ctx_a = ctx(Uuid::now_v7());
    let tenant = ctx_a.subject_tenant_id();
    let ctx_b = ctx(tenant);
    let owner_a = ctx_a.subject_id();

    h.client
        .create_file(&ctx_a, new_file(OwnerKind::User, owner_a), None, true, None)
        .await
        .expect("create_file");

    let page = h
        .client
        .list_files(
            &ctx_a,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id: owner_a,
            },
            Some(10),
            0,
        )
        .await
        .expect("self-owner list succeeds");
    assert_eq!(page.items.len(), 1);

    let err = h
        .client
        .list_files(
            &ctx_b,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id: owner_a,
            },
            Some(10),
            0,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::PermissionDenied { .. }),
        "expected PermissionDenied, got {err:?}"
    );
}

/// A regression test for `FileStorageLocalClient::list_files`: it must
/// attach each file's custom metadata (via `FileService::list_files_with_metadata`),
/// not the bare file records `FileService::list_files` itself returns — the
/// pre-fix `FileStorageLocalClient::list_files` mapped the service's plain
/// `Vec<File>` straight into `FileRecord`-less items and always came back with
/// empty `custom_metadata`, unlike `GET /files`.
#[tokio::test]
async fn list_files_includes_custom_metadata() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, true, None)
        .await
        .expect("create_file");
    let file_id = match outcome {
        CreateFileOutcome::SinglePart(t) => t.file_id,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };
    h.client
        .update_metadata(
            &ctx,
            file_id,
            CustomMetadataPatch {
                entries: vec![("k".to_owned(), Some("v".to_owned()))],
            },
            None,
        )
        .await
        .expect("update_metadata");

    let page = h
        .client
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
        .expect("list_files");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].file.file_id, file_id);
    assert_eq!(
        page.items[0].custom_metadata,
        vec![CustomMetadataEntry {
            key: "k".to_owned(),
            value: "v".to_owned(),
        }]
    );
}

#[tokio::test]
async fn update_metadata_success_and_precondition_failed() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, _) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"data").await;

    let record = h
        .client
        .update_metadata(
            &ctx,
            file_id,
            CustomMetadataPatch {
                entries: vec![("k".to_owned(), Some("v".to_owned()))],
            },
            None,
        )
        .await
        .expect("update_metadata");
    assert_eq!(record.file.meta_version, 1);

    let err = h
        .client
        .update_metadata(
            &ctx,
            file_id,
            CustomMetadataPatch {
                entries: vec![("k".to_owned(), Some("v2".to_owned()))],
            },
            Some(999), // stale expected meta_version
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::FailedPrecondition { .. }),
        "expected FailedPrecondition, got {err:?}"
    );
}

#[tokio::test]
async fn delete_file_success_and_missing_if_match() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, _) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"gone soon").await;

    let err = h.client.delete_file(&ctx, file_id, None).await.unwrap_err();
    assert!(
        matches!(err, FileStorageError::FailedPrecondition { .. }),
        "expected FailedPrecondition for a missing If-Match, got {err:?}"
    );

    h.client
        .delete_file(&ctx, file_id, Some("*"))
        .await
        .expect("unconditional delete with If-Match: * succeeds");

    let err = h.client.get_file(&ctx, file_id, None).await.unwrap_err();
    assert!(matches!(err, FileStorageError::NotFound { .. }));
}

#[tokio::test]
async fn download_url_success_and_no_content_conflict() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, version_id) =
        create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"downloadable").await;

    let err = h
        .client
        .download_url(&ctx, file_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::Aborted { .. }),
        "expected Aborted (conflict) before any content is bound, got {err:?}"
    );

    h.client
        .bind(&ctx, file_id, version_id, None)
        .await
        .expect("bind");
    let ticket = h
        .client
        .download_url(&ctx, file_id, None)
        .await
        .expect("download_url after bind");
    assert_eq!(ticket.version_id, version_id);
    assert!(ticket.download_url.contains("/download/"));
}

#[tokio::test]
async fn list_versions_success_and_unknown_file() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, version_id) =
        create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"versioned").await;

    let page = h
        .client
        .list_versions(&ctx, file_id, Some(10), 0)
        .await
        .expect("list_versions");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].version.version_id, version_id);
    assert!(page.items[0].manifest.is_none());

    let err = h
        .client
        .list_versions(&ctx, Uuid::now_v7(), Some(10), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, FileStorageError::NotFound { .. }));
}

#[tokio::test]
async fn presign_version_success_and_unknown_file() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, _) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"v1").await;

    let ticket = h
        .client
        .presign_version(&ctx, file_id)
        .await
        .expect("presign_version");
    assert_eq!(ticket.file_id, file_id);

    let err = h
        .client
        .presign_version(&ctx, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(err, FileStorageError::NotFound { .. }));
}

#[tokio::test]
async fn bind_success_and_precondition_failed() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, v1) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"v1").await;

    let record = h
        .client
        .bind(&ctx, file_id, v1, None)
        .await
        .expect("first bind is unconditional");
    assert_eq!(record.file.content_id, Some(v1));

    let ticket2 = h
        .client
        .presign_version(&ctx, file_id)
        .await
        .expect("presign v2");
    let path2 = backend_path(file_id, ticket2.version_id);
    write_bytes(&h.backend, &path2, b"v2").await;
    h.file_svc
        .finalize_upload(&ctx, file_id, ticket2.version_id, 2, hash::sha256(b"v2"))
        .await
        .expect("finalize v2");

    let err = h
        .client
        .bind(&ctx, file_id, ticket2.version_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::FailedPrecondition { .. }),
        "rebind without If-Match must fail, got {err:?}"
    );
}

#[tokio::test]
async fn delete_version_success_and_current_version_conflict() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, v1) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"v1").await;
    h.client
        .bind(&ctx, file_id, v1, None)
        .await
        .expect("bind v1");

    let ticket2 = h
        .client
        .presign_version(&ctx, file_id)
        .await
        .expect("presign v2");
    let path2 = backend_path(file_id, ticket2.version_id);
    write_bytes(&h.backend, &path2, b"v2").await;
    h.file_svc
        .finalize_upload(&ctx, file_id, ticket2.version_id, 2, hash::sha256(b"v2"))
        .await
        .expect("finalize v2");

    // Deleting the current (bound) v1 while a non-current v2 still exists is
    // rejected — deleting the *only* version would instead be equivalent to
    // deleting the whole file (a different, allowed path), so this must run
    // before v2 is removed.
    let err = h
        .client
        .delete_version(&ctx, file_id, v1)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::Aborted { .. }),
        "expected Aborted (conflict) deleting the current version, got {err:?}"
    );

    // Deleting the non-current v2 succeeds.
    h.client
        .delete_version(&ctx, file_id, ticket2.version_id)
        .await
        .expect("delete non-current version");
}

#[tokio::test]
async fn initiate_multipart_success_and_invalid_part_size() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, false, None)
        .await
        .expect("create_file");
    let file_id = match outcome {
        CreateFileOutcome::SinglePart(t) => t.file_id,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };

    let plan = h
        .client
        .initiate_multipart(
            &ctx,
            file_id,
            "application/octet-stream",
            12 * 1024 * 1024,
            None,
            None,
        )
        .await
        .expect("initiate_multipart");
    assert!(plan.parts.len() >= 2);

    let err = h
        .client
        .initiate_multipart(
            &ctx,
            file_id,
            "application/octet-stream",
            1024,
            Some(1), // far below DEFAULT_MIN_PART_SIZE
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::InvalidArgument { .. }),
        "expected InvalidArgument, got {err:?}"
    );
}

#[tokio::test]
async fn introspect_and_complete_multipart_success_and_failures() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, false, None)
        .await
        .expect("create_file");
    let file_id = match outcome {
        CreateFileOutcome::SinglePart(t) => t.file_id,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };
    let plan = h
        .client
        .initiate_multipart(
            &ctx,
            file_id,
            "application/octet-stream",
            12 * 1024 * 1024,
            None,
            None,
        )
        .await
        .expect("initiate_multipart");

    // introspect: success + unknown upload_id.
    let status = h
        .client
        .introspect_multipart(&ctx, file_id, plan.upload_id)
        .await
        .expect("introspect_multipart");
    assert_eq!(status.missing.len(), plan.parts.len());
    let err = h
        .client
        .introspect_multipart(&ctx, file_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(err, FileStorageError::NotFound { .. }));

    // complete: parts-missing conflict, then success after reporting all parts.
    let err = h
        .client
        .complete_multipart(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::Aborted { .. }),
        "expected Aborted (parts missing), got {err:?}"
    );

    for part in &plan.parts {
        let size = usize::try_from(part.size).unwrap();
        let data = vec![0u8; size];
        let backend_path_str = backend_path(file_id, plan.version_id);
        let session = h
            .multipart_store
            .get_multipart_upload(plan.upload_id)
            .await
            .expect("get_multipart_upload")
            .expect("session exists");
        let (etag, digest) = h
            .backend
            .upload_part_stream(
                &backend_path_str,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                Box::pin(futures::stream::once(async move {
                    Ok::<_, std::io::Error>(Bytes::from(data))
                })),
                part.size,
            )
            .await
            .expect("upload_part_stream");
        h.multipart_store
            .upsert_multipart_part(
                plan.upload_id,
                i32::try_from(part.part_number).unwrap(),
                &etag,
                digest,
                part.size.try_into().unwrap(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .expect("upsert_multipart_part");
    }

    let outcome = h
        .client
        .complete_multipart(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("complete_multipart");
    match outcome {
        MultipartCompleteOutcome::Completed(completed) => {
            assert_eq!(completed.version_id, plan.version_id);
        }
        MultipartCompleteOutcome::Completing { .. } => panic!("expected Completed"),
    }
}

#[tokio::test]
async fn abort_multipart_success_and_not_in_progress() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, false, None)
        .await
        .expect("create_file");
    let file_id = match outcome {
        CreateFileOutcome::SinglePart(t) => t.file_id,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };
    let plan = h
        .client
        .initiate_multipart(
            &ctx,
            file_id,
            "application/octet-stream",
            12 * 1024 * 1024,
            None,
            None,
        )
        .await
        .expect("initiate_multipart");

    h.client
        .abort_multipart(&ctx, file_id, plan.upload_id)
        .await
        .expect("abort_multipart");

    let err = h
        .client
        .abort_multipart(&ctx, file_id, plan.upload_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::Aborted { .. }),
        "expected Aborted (not in_progress) on a double-abort, got {err:?}"
    );
}

#[tokio::test]
async fn transfer_ownership_success_and_nil_owner_rejected() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, _) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"transfer me").await;

    let new_owner = Uuid::now_v7();
    let record = h
        .client
        .transfer_ownership(&ctx, file_id, OwnerKind::User, new_owner)
        .await
        .expect("transfer_ownership");
    assert_eq!(record.file.owner_id, new_owner);

    let err = h
        .client
        .transfer_ownership(&ctx, file_id, OwnerKind::User, Uuid::nil())
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::InvalidArgument { .. }),
        "expected InvalidArgument for a nil new_owner_id, got {err:?}"
    );
}

#[tokio::test]
async fn migrate_backend_success_and_unknown_backend() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let (file_id, _) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"migrate me").await;

    h.client
        .migrate_backend(&ctx, file_id, DURABLE_BACKEND)
        .await
        .expect("migrate_backend to a durable destination");

    let err = h
        .client
        .migrate_backend(&ctx, file_id, "does-not-exist")
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::InvalidArgument { .. }),
        "expected InvalidArgument for an unknown backend id, got {err:?}"
    );
}

#[tokio::test]
async fn list_storages_success_and_read_denied() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());

    let storages = h.client.list_storages(&ctx).await.expect("list_storages");
    assert_eq!(storages.len(), 2);
    assert!(storages.iter().any(|s| s.id == DEFAULT_BACKEND));
    assert!(storages.iter().any(|s| s.id == DURABLE_BACKEND));

    h.authz.set_deny_read(true);
    let err = h.client.list_storages(&ctx).await.unwrap_err();
    assert!(matches!(err, FileStorageError::PermissionDenied { .. }));
}

#[tokio::test]
async fn get_storage_success_and_unknown_id() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());

    let storage = h
        .client
        .get_storage(&ctx, DEFAULT_BACKEND)
        .await
        .expect("get_storage");
    assert_eq!(storage.id, DEFAULT_BACKEND);
    assert!(storage.capabilities.multipart_native);

    let err = h.client.get_storage(&ctx, "nope").await.unwrap_err();
    assert!(matches!(err, FileStorageError::InvalidArgument { .. }));
}

#[tokio::test]
async fn get_policy_success_and_malformed_user_scope() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());

    let policy = h
        .client
        .get_policy(&ctx, PolicyScope::Tenant, None)
        .await
        .expect("get_policy");
    assert!(policy.is_none(), "no tenant policy has been set yet");

    let err = h
        .client
        .get_policy(&ctx, PolicyScope::User, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::InvalidArgument { .. }),
        "a user-scope query with no scope_owner_id must be rejected, got {err:?}"
    );
}

#[tokio::test]
async fn get_effective_policy_success_and_foreign_owner_forbidden() {
    let h = build_harness().await;
    let ctx_a = ctx(Uuid::now_v7());
    let tenant = ctx_a.subject_tenant_id();
    let ctx_b = ctx(tenant);

    let ep = h
        .client
        .get_effective_policy(&ctx_a, None)
        .await
        .expect("get_effective_policy for self");
    assert!(ep.max_bytes.is_none());

    let err = h
        .client
        .get_effective_policy(&ctx_b, Some(ctx_a.subject_id()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::PermissionDenied { .. }),
        "expected PermissionDenied for a foreign user_owner_id, got {err:?}"
    );
}

#[tokio::test]
async fn put_policy_success_and_tenant_scope_forbidden() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let stored = h
        .client
        .put_policy(
            &ctx,
            PolicyScope::User,
            Some(owner),
            PolicyBody {
                allowed_mime_types: vec!["text/plain".to_owned()],
                ..Default::default()
            },
        )
        .await
        .expect("self-service user-scope put_policy");
    assert_eq!(stored.scope, PolicyScope::User);

    let err = h
        .client
        .put_policy(&ctx, PolicyScope::Tenant, None, PolicyBody::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::PermissionDenied { .. }),
        "tenant-scope put_policy must require ADMIN_POLICY, got {err:?}"
    );
}

#[tokio::test]
async fn list_retention_rules_success_and_read_denied() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());

    let rules = h
        .client
        .list_retention_rules(&ctx)
        .await
        .expect("list_retention_rules");
    assert!(rules.is_empty());

    h.authz.set_deny_read(true);
    let err = h.client.list_retention_rules(&ctx).await.unwrap_err();
    assert!(matches!(err, FileStorageError::PermissionDenied { .. }));
}

#[tokio::test]
async fn create_retention_rule_success_and_empty_body_rejected() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    h.authz.set_admin(true);

    let rule = h
        .client
        .create_retention_rule(
            &ctx,
            RetentionScope::Tenant,
            None,
            RetentionRuleBody {
                age: Some(file_storage_sdk::AgeRetention { max_age_days: 30 }),
                ..Default::default()
            },
        )
        .await
        .expect("create_retention_rule");
    assert_eq!(rule.scope, RetentionScope::Tenant);

    let err = h
        .client
        .create_retention_rule(
            &ctx,
            RetentionScope::Tenant,
            None,
            RetentionRuleBody::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::InvalidArgument { .. }),
        "a rule with no criteria at all must be rejected, got {err:?}"
    );
}

#[tokio::test]
async fn delete_retention_rule_success_and_unknown_id() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    h.authz.set_admin(true);
    let rule = h
        .client
        .create_retention_rule(
            &ctx,
            RetentionScope::Tenant,
            None,
            RetentionRuleBody {
                age: Some(file_storage_sdk::AgeRetention { max_age_days: 30 }),
                ..Default::default()
            },
        )
        .await
        .expect("create_retention_rule");

    h.client
        .delete_retention_rule(&ctx, rule.rule_id)
        .await
        .expect("delete_retention_rule");

    let err = h
        .client
        .delete_retention_rule(&ctx, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(err, FileStorageError::NotFound { .. }));
}

// ── REST equivalence ─────────────────────────────────────────────────────────

/// `dto::CreateFileReq` derives `Deserialize` only (it's a request DTO), so
/// the equivalence test builds the request body as raw JSON instead of
/// serializing the struct.
fn create_req_json(owner: Uuid, name: &str) -> Value {
    json!({
        "owner_kind": "user",
        "owner_id": owner,
        "name": name,
        "gts_file_type": GTS,
        "mime_type": "application/octet-stream",
        "custom_metadata": [],
        "bind": "manual",
    })
}

fn rest_router(
    svc: &Arc<FileService>,
    msvc: &Arc<MultipartService>,
    ctx: &SecurityContext,
) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files"),
            get(handlers::list_files).post(handlers::create_file),
        )
        .route(
            &format!("{BASE}/files/{{id}}"),
            get(handlers::get_file)
                .patch(handlers::update_metadata)
                .delete(handlers::delete_file),
        )
        .route(&format!("{BASE}/files/{{id}}/bind"), post(handlers::bind))
        .layer(axum::Extension(ctx.clone()))
        .layer(axum::Extension(Arc::clone(msvc)))
        .layer(axum::Extension(Arc::clone(svc)))
}

async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid JSON body")
}

fn json_req(
    method: &str,
    uri: String,
    if_match: Option<&str>,
    body: Option<&Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(v) = if_match {
        builder = builder.header("if-match", v);
    }
    let payload = body.map_or_else(Vec::new, |b| serde_json::to_vec(b).expect("serialize"));
    builder.body(Body::from(payload)).expect("build request")
}

/// `create` → `get` produce the same observable file shape whether driven
/// through the SDK client or through the real REST handlers.
#[tokio::test]
async fn equivalence_create_then_get() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    // SDK side.
    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, false, None)
        .await
        .expect("sdk create_file");
    let sdk_ticket = match outcome {
        CreateFileOutcome::SinglePart(t) => t,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };
    let sdk_fetch = h
        .client
        .get_file(&ctx, sdk_ticket.file_id, None)
        .await
        .expect("sdk get_file");
    let sdk_record = match sdk_fetch {
        FileFetch::Modified { record, .. } => record,
        FileFetch::NotModified => unreachable!(),
    };

    // REST side, over the same services and ctx.
    let router = rest_router(&h.file_svc, &h.multipart_svc, &ctx);
    let create_resp = router
        .clone()
        .oneshot(json_req(
            "POST",
            format!("{BASE}/files"),
            None,
            Some(&create_req_json(owner, "doc.bin")),
        ))
        .await
        .expect("dispatch create");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let create_body = body_json(create_resp).await;
    let rest_file_id = create_body["file_id"].as_str().expect("file_id").to_owned();

    let get_resp = router
        .oneshot(json_req(
            "GET",
            format!("{BASE}/files/{rest_file_id}"),
            None,
            None,
        ))
        .await
        .expect("dispatch get");
    assert_eq!(get_resp.status(), StatusCode::OK);
    let rest_body = body_json(get_resp).await;

    // Compare the shape (excluding identity/timestamp fields, which
    // necessarily differ between the two separately-created files).
    assert_eq!(rest_body["owner_kind"], "user");
    assert_eq!(rest_body["owner_id"], sdk_record.file.owner_id.to_string());
    assert_eq!(rest_body["gts_file_type"], GTS);
    assert_eq!(rest_body["meta_version"], 0);
    assert!(rest_body["content_id"].is_null());
    assert_eq!(sdk_record.file.meta_version, 0);
    assert!(sdk_record.file.content_id.is_none());
    assert_eq!(rest_body["custom_metadata"], json!([]));
    assert_eq!(sdk_record.custom_metadata.len(), 0);
}

/// `list_files` (SDK) and `GET /files` (REST) attach custom metadata the same
/// way — both go through the shared `FileService::list_files_with_metadata`.
#[tokio::test]
async fn equivalence_list_files_with_metadata() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();
    let owner_filter = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner,
    };

    // SDK side: create + attach metadata via the SDK client.
    let outcome = h
        .client
        .create_file(&ctx, new_file(OwnerKind::User, owner), None, false, None)
        .await
        .expect("sdk create_file");
    let sdk_file_id = match outcome {
        CreateFileOutcome::SinglePart(t) => t.file_id,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };
    h.client
        .update_metadata(
            &ctx,
            sdk_file_id,
            CustomMetadataPatch {
                entries: vec![("owner".to_owned(), Some("sdk".to_owned()))],
            },
            None,
        )
        .await
        .expect("sdk update_metadata");

    let sdk_page = h
        .client
        .list_files(&ctx, owner_filter, Some(10), 0)
        .await
        .expect("sdk list_files");
    let sdk_record = sdk_page
        .items
        .iter()
        .find(|r| r.file.file_id == sdk_file_id)
        .expect("sdk-created file present in its own listing");
    assert_eq!(
        sdk_record.custom_metadata,
        vec![CustomMetadataEntry {
            key: "owner".to_owned(),
            value: "sdk".to_owned(),
        }]
    );

    // REST side, over the same services/ctx/owner: create + metadata patch
    // driven through the real handlers instead, then listed via the real
    // `GET /files` handler.
    let router = rest_router(&h.file_svc, &h.multipart_svc, &ctx);
    let create_resp = router
        .clone()
        .oneshot(json_req(
            "POST",
            format!("{BASE}/files"),
            None,
            Some(&create_req_json(owner, "listed.bin")),
        ))
        .await
        .expect("dispatch create");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let create_body = body_json(create_resp).await;
    let rest_file_id = create_body["file_id"].as_str().expect("file_id").to_owned();

    let patch_resp = router
        .clone()
        .oneshot(json_req(
            "PATCH",
            format!("{BASE}/files/{rest_file_id}"),
            None,
            Some(&json!({ "custom_metadata": { "owner": "rest" } })),
        ))
        .await
        .expect("dispatch update_metadata");
    assert_eq!(patch_resp.status(), StatusCode::OK);

    let list_resp = router
        .oneshot(json_req(
            "GET",
            format!("{BASE}/files?owner_kind=user&owner_id={owner}"),
            None,
            None,
        ))
        .await
        .expect("dispatch list");
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = body_json(list_resp).await;
    let rest_items = list_body.as_array().expect("array body");

    // Both the sdk-created and rest-created file are owned by the same
    // subject, so both appear in this single owner-scoped REST listing —
    // each carrying the metadata attached through its own surface.
    let rest_entry = rest_items
        .iter()
        .find(|f| f["file_id"] == rest_file_id)
        .expect("rest-created file present in the REST listing");
    assert_eq!(
        rest_entry["custom_metadata"],
        json!([{ "key": "owner", "value": "rest" }])
    );
    let sdk_entry_via_rest = rest_items
        .iter()
        .find(|f| f["file_id"] == sdk_file_id.to_string())
        .expect("sdk-created file also present in the REST listing");
    assert_eq!(
        sdk_entry_via_rest["custom_metadata"],
        json!([{ "key": "owner", "value": "sdk" }])
    );
}

/// `bind`'s success shape and its rebind-without-`If-Match` failure are the
/// same through both surfaces.
#[tokio::test]
async fn equivalence_bind() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    // SDK side: create + finalize twice, bind v1, then attempt an
    // unconditional rebind onto v2.
    let (sdk_file, sdk_v1) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"sdk-v1").await;
    h.client
        .bind(&ctx, sdk_file, sdk_v1, None)
        .await
        .expect("sdk first bind");
    let sdk_v2_ticket = h
        .client
        .presign_version(&ctx, sdk_file)
        .await
        .expect("presign v2");
    write_bytes(
        &h.backend,
        &backend_path(sdk_file, sdk_v2_ticket.version_id),
        b"sdk-v2",
    )
    .await;
    h.file_svc
        .finalize_upload(
            &ctx,
            sdk_file,
            sdk_v2_ticket.version_id,
            6,
            hash::sha256(b"sdk-v2"),
        )
        .await
        .expect("finalize sdk v2");
    let sdk_err = h
        .client
        .bind(&ctx, sdk_file, sdk_v2_ticket.version_id, None)
        .await
        .unwrap_err();

    // REST side: identical sequence via the real handlers.
    let (rest_file, rest_v1) =
        create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"rest-v1").await;
    let router = rest_router(&h.file_svc, &h.multipart_svc, &ctx);
    let first_bind = router
        .clone()
        .oneshot(json_req(
            "POST",
            format!("{BASE}/files/{rest_file}/bind"),
            None,
            Some(&json!({ "version_id": rest_v1 })),
        ))
        .await
        .expect("dispatch first bind");
    assert_eq!(first_bind.status(), StatusCode::OK);

    let v2_ticket = h
        .client
        .presign_version(&ctx, rest_file)
        .await
        .expect("presign rest v2");
    write_bytes(
        &h.backend,
        &backend_path(rest_file, v2_ticket.version_id),
        b"rest-v2",
    )
    .await;
    h.file_svc
        .finalize_upload(
            &ctx,
            rest_file,
            v2_ticket.version_id,
            7,
            hash::sha256(b"rest-v2"),
        )
        .await
        .expect("finalize rest v2");

    let rebind = router
        .oneshot(json_req(
            "POST",
            format!("{BASE}/files/{rest_file}/bind"),
            None,
            Some(&json!({ "version_id": v2_ticket.version_id })),
        ))
        .await
        .expect("dispatch rebind");

    assert_eq!(
        rebind.status(),
        StatusCode::BAD_REQUEST,
        "rebinding without If-Match must be rejected over REST too"
    );
    assert!(
        matches!(sdk_err, FileStorageError::FailedPrecondition { .. }),
        "expected the same FailedPrecondition category over the SDK, got {sdk_err:?}"
    );
}

/// `delete` with a wrong `If-Match` fails the same way through both
/// surfaces; an unconditional (`"*"`) delete then succeeds on both.
#[tokio::test]
async fn equivalence_delete_wrong_if_match() {
    let h = build_harness().await;
    let ctx = ctx(Uuid::now_v7());
    let owner = ctx.subject_id();

    let (sdk_file, sdk_v1) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"sdk").await;
    h.client
        .bind(&ctx, sdk_file, sdk_v1, None)
        .await
        .expect("bind");
    let sdk_err = h
        .client
        .delete_file(&ctx, sdk_file, Some("\"not-the-real-etag\""))
        .await
        .unwrap_err();
    h.client
        .delete_file(&ctx, sdk_file, Some("*"))
        .await
        .expect("unconditional delete succeeds");

    let (rest_file, rest_v1) = create_and_finalize(&h, &ctx, OwnerKind::User, owner, b"rest").await;
    let router = rest_router(&h.file_svc, &h.multipart_svc, &ctx);
    router
        .clone()
        .oneshot(json_req(
            "POST",
            format!("{BASE}/files/{rest_file}/bind"),
            None,
            Some(&json!({ "version_id": rest_v1 })),
        ))
        .await
        .expect("dispatch bind");

    let wrong_delete = router
        .clone()
        .oneshot(json_req(
            "DELETE",
            format!("{BASE}/files/{rest_file}"),
            Some("\"not-the-real-etag\""),
            None,
        ))
        .await
        .expect("dispatch wrong-etag delete");
    assert_eq!(wrong_delete.status(), StatusCode::BAD_REQUEST);
    assert!(
        matches!(sdk_err, FileStorageError::FailedPrecondition { .. }),
        "expected the same FailedPrecondition category over the SDK, got {sdk_err:?}"
    );

    let real_delete = router
        .oneshot(json_req(
            "DELETE",
            format!("{BASE}/files/{rest_file}"),
            Some("*"),
            None,
        ))
        .await
        .expect("dispatch unconditional delete");
    assert_eq!(real_delete.status(), StatusCode::NO_CONTENT);
}

// ── service-owner scenario ───────────────────────────────────────────────────

/// A backend gear's own app identity creates, uploads, and reads back a file
/// under its own `(owner_kind: app, owner_id: <self>)` — the self-service
/// fast path needs no `ADMIN_POLICY` grant (mirrors
/// `tests/list_authz_test.rs::list_files_owner_kind_match_without_admin_is_allowed`,
/// extended through the full create→finalize→bind→download flow this SDK
/// exposes). `download_url` only ever mints a signed URL (Level 1 SDK: no
/// bytes flow through it), so this stops at that ticket rather than driving
/// bytes through an actual sidecar HTTP endpoint.
#[tokio::test]
async fn service_owner_self_service_create_upload_and_read() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let app_id = Uuid::now_v7();
    let ctx_app = ctx_with(tenant, app_id, "app");

    let outcome = h
        .client
        .create_file(
            &ctx_app,
            new_file(OwnerKind::App, app_id),
            None,
            false,
            None,
        )
        .await
        .expect("app subject creates its own file without ADMIN_POLICY");
    let ticket = match outcome {
        CreateFileOutcome::SinglePart(t) => t,
        CreateFileOutcome::Multipart { .. } => unreachable!(),
    };

    // Simulate the sidecar: write the bytes, then the finalize callback.
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_bytes(&h.backend, &path, b"generated media").await;
    h.file_svc
        .finalize_upload(
            &ctx_app,
            ticket.file_id,
            ticket.version_id,
            15,
            hash::sha256(b"generated media"),
        )
        .await
        .expect("finalize_upload");

    let fetch = h
        .client
        .get_file(&ctx_app, ticket.file_id, None)
        .await
        .expect("get_file sees the finalized version's file row");
    match fetch {
        FileFetch::Modified { record, .. } => {
            assert_eq!(record.file.owner_kind, OwnerKind::App);
            assert_eq!(record.file.owner_id, app_id);
        }
        FileFetch::NotModified => panic!("expected Modified"),
    }
    let versions = h
        .client
        .list_versions(&ctx_app, ticket.file_id, Some(10), 0)
        .await
        .expect("list_versions");
    assert_eq!(versions.items.len(), 1);

    h.client
        .bind(&ctx_app, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind");
    let download = h
        .client
        .download_url(&ctx_app, ticket.file_id, None)
        .await
        .expect("download_url is issued once content is bound");
    assert_eq!(download.version_id, ticket.version_id);
}

/// The same `app` subject creating a file under a DIFFERENT owner
/// (`owner_id != subject_id`) still requires `ADMIN_POLICY` — the
/// self-service fast path is narrowly scoped to the caller's own identity.
#[tokio::test]
async fn service_owner_cross_owner_create_requires_admin_policy() {
    let h = build_harness().await;
    let tenant = Uuid::now_v7();
    let app_id = Uuid::now_v7();
    let victim_app_id = Uuid::now_v7();
    let ctx_app = ctx_with(tenant, app_id, "app");

    let err = h
        .client
        .create_file(
            &ctx_app,
            new_file(OwnerKind::App, victim_app_id),
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FileStorageError::PermissionDenied { .. }),
        "cross-owner create must require ADMIN_POLICY, got {err:?}"
    );

    // Positive control: the same cross-owner create succeeds once
    // `ADMIN_POLICY` is granted.
    h.authz.set_admin(true);
    let outcome = h
        .client
        .create_file(
            &ctx_app,
            new_file(OwnerKind::App, victim_app_id),
            None,
            false,
            None,
        )
        .await
        .expect("admin-authorized cross-owner create succeeds");
    assert!(matches!(outcome, CreateFileOutcome::SinglePart(_)));
}
