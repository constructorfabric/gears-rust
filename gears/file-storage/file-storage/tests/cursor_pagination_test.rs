//! Cursor-pagination coverage for the three SQL-backed listings: `GET /files`, `GET /files/{id}/versions`,
//! `GET /retention-rules`. Complements the endpoint-specific tests already
//! updated elsewhere (`list_authz_test.rs`, `service_test.rs`,
//! `policy_authz_test.rs`, `domain_coverage_test.rs`) with the pagination
//! contract itself: full-walk coverage (including `created_at` ties at page
//! boundaries), forward-only cursor errors, the manifest-byte-budget/cursor
//! interaction on `/files/{id}/versions`, and non-admin visibility staying
//! SQL-filtered (full pages) on `/retention-rules`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer, actions};
use file_storage::domain::error::DomainError;
use file_storage::domain::pagination;
use file_storage::domain::policy::{AgeRetention, RetentionRuleBody, RetentionScope};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::PolicyStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{File, FileVersion, NewFile, OwnerFilter, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.cursor_pagination_test.file.type.v1~");
const BASE: &str = "/api/file-storage/v1";

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-cursor-pagination-{}.db",
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

/// Matches the platform's real defaults (`FileStorageConfig::default_page_size`/
/// `max_page_size`), so `?limit` clamping tests observe the real ceiling.
fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 25,
        max_page_size: 200,
        idempotency_ttl_secs: 86400,
    }
}

fn ctx(tenant: Uuid, subject: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
}

fn new_file(owner_id: Uuid) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

fn valid_rule_body() -> RetentionRuleBody {
    RetentionRuleBody {
        age: Some(AgeRetention { max_age_days: 30 }),
        inactivity: None,
        metadata: None,
    }
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid JSON body")
}

fn get_req(uri: String) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request")
}

/// Grants `READ`/`WRITE`/`DELETE` unconditionally; gates `ADMIN_POLICY` on
/// `is_admin`. Self-contained copy of the pattern established in
/// `tests/policy_authz_test.rs`/`tests/list_authz_test.rs` (each
/// `tests/*.rs` file is its own integration-test crate).
#[derive(Default)]
struct ScopedTestAuthorizer {
    is_admin: AtomicBool,
}

impl ScopedTestAuthorizer {
    fn new() -> Self {
        Self::default()
    }

    fn set_admin(&self, admin: bool) {
        self.is_admin.store(admin, Ordering::SeqCst);
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
        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

// =============================================================================
// GET /files
// =============================================================================

/// Seeds `n` files for one owner directly via `FileRepo` (bypassing
/// `FileService::create_file`) so `created_at` can be pinned exactly --
/// `instants[i]` for file index `i` -- to deterministically exercise the
/// same-`created_at` tie-break at a page boundary.
async fn seed_files(
    db: &Arc<DBProvider<DbError>>,
    tenant: Uuid,
    owner_id: Uuid,
    instants: &[OffsetDateTime],
) -> Vec<Uuid> {
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let scope = AccessScope::allow_all();
    let mut ids = Vec::with_capacity(instants.len());
    for &created_at in instants {
        let file_id = Uuid::now_v7();
        let file = File {
            file_id,
            tenant_id: tenant,
            owner_kind: OwnerKind::User,
            owner_id,
            name: "doc.bin".to_owned(),
            gts_file_type: GTS.to_owned(),
            content_id: None,
            meta_version: 0,
            created_at,
            last_modified_at: created_at,
        };
        files.create(&conn, &scope, &file).await.expect("create");
        ids.push(file_id);
    }
    ids
}

/// Canonical order for `/files`/`/files/{id}/versions`/`/retention-rules`:
/// `created_at DESC`, id column `DESC`.
fn expected_order(mut items: Vec<(OffsetDateTime, Uuid)>) -> Vec<Uuid> {
    items.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    items.into_iter().map(|(_, id)| id).collect()
}

#[tokio::test]
async fn files_cursor_walk_covers_all_items_exactly_once_including_created_at_ties() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let base = OffsetDateTime::now_utc();
    // 7 files across 3 distinct instants -- several files share `base` and
    // `base + 1s` so a small `limit` walk crosses a `created_at` tie.
    let instants = [
        base,
        base,
        base,
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(2),
        base + time::Duration::seconds(2),
    ];
    let ids = seed_files(&db, tenant, owner, &instants).await;
    let expected = expected_order(instants.iter().copied().zip(ids.iter().copied()).collect());

    let ctx = ctx(tenant, owner);
    let owner_filter = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner,
    };

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = svc
            .list_files(&ctx, owner_filter, Some(2), cursor.as_deref())
            .await
            .expect("list_files page");
        seen.extend(page.items.iter().map(|f| f.file_id));
        match page.page_info.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= expected.len(), "walk must terminate");
    }

    assert_eq!(
        seen, expected,
        "cursor walk must cover every file exactly once, in canonical order"
    );
}

#[tokio::test]
async fn files_cursor_rejects_a_different_owner() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner_a = Uuid::now_v7();
    let owner_b = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    seed_files(&db, tenant, owner_a, &[now, now]).await;
    seed_files(&db, tenant, owner_b, &[now]).await;

    let ctx_a = ctx(tenant, owner_a);
    let filter_for_a = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner_a,
    };
    let page = svc
        .list_files(&ctx_a, filter_for_a, Some(1), None)
        .await
        .expect("first page");
    let cursor = page.page_info.next_cursor.expect("more pages remain");

    let filter_for_b = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner_b,
    };
    let ctx_b = ctx(tenant, owner_b);
    let err = svc
        .list_files(&ctx_b, filter_for_b, Some(1), Some(&cursor))
        .await
        .expect_err("a cursor issued for a different owner must be rejected");
    assert!(
        matches!(err, DomainError::Cursor(_)),
        "expected a cursor error, got {err:?}"
    );
}

fn files_router(ctx: SecurityContext, svc: Arc<FileService>) -> Router {
    Router::new()
        .route(&format!("{BASE}/files"), get(handlers::list_files))
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(svc))
}

async fn build_files_harness() -> (Arc<FileService>, SecurityContext, Uuid) {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ));
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    svc.create_file(&ctx(tenant, owner), new_file(owner), None, false)
        .await
        .expect("seed one file");
    (svc, ctx(tenant, owner), owner)
}

#[tokio::test]
async fn files_garbage_cursor_is_rejected_as_400() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/files?owner_kind=user&owner_id={owner}&cursor=not-a-real-cursor"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn files_offset_query_param_is_rejected_as_400() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/files?owner_kind=user&owner_id={owner}&offset=0"
        )))
        .await
        .expect("dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "the old offset param must be rejected, not silently ignored"
    );
    let body = body_json(resp).await;
    assert!(
        body["context"]["field_violations"]
            .as_array()
            .is_some_and(|v| !v.is_empty()),
        "must carry a canonical field_violations payload, got {body}"
    );
}

#[tokio::test]
async fn files_limit_zero_is_rejected_as_400() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/files?owner_kind=user&owner_id={owner}&limit=0"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn files_limit_above_max_is_clamped_to_200() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/files?owner_kind=user&owner_id={owner}&limit=9999"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["page_info"]["limit"], 200);
    assert!(body["items"].as_array().unwrap().len() <= 200);
}

// =============================================================================
// GET /files/{id}/versions
// =============================================================================

fn new_version(file_id: Uuid, version_id: Uuid, created_at: OffsetDateTime) -> FileVersion {
    FileVersion {
        file_id,
        version_id,
        mime_type: "text/plain".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: "whole-sha256".to_owned(),
        part_count: None,
        status: VersionStatus::Available,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: format!("/{file_id}/{version_id}"),
        created_at,
        bound_on_finalize: false,
    }
}

async fn seed_file_and_versions(
    db: &Arc<DBProvider<DbError>>,
    tenant: Uuid,
    owner: Uuid,
    instants: &[OffsetDateTime],
) -> (Uuid, Vec<Uuid>) {
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();
    let file_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    files
        .create(
            &conn,
            &scope,
            &File {
                file_id,
                tenant_id: tenant,
                owner_kind: OwnerKind::User,
                owner_id: owner,
                name: "doc.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                content_id: None,
                meta_version: 0,
                created_at: now,
                last_modified_at: now,
            },
        )
        .await
        .expect("create file");
    let mut ids = Vec::with_capacity(instants.len());
    for &created_at in instants {
        let version_id = Uuid::now_v7();
        versions
            .insert(&conn, &scope, &new_version(file_id, version_id, created_at))
            .await
            .expect("insert version");
        ids.push(version_id);
    }
    (file_id, ids)
}

#[tokio::test]
async fn versions_cursor_walk_covers_all_items_exactly_once_including_created_at_ties() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let base = OffsetDateTime::now_utc();
    let instants = [
        base,
        base,
        base,
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(1),
        base + time::Duration::seconds(2),
    ];
    let (file_id, ids) = seed_file_and_versions(&db, tenant, owner, &instants).await;
    let expected = expected_order(instants.iter().copied().zip(ids.iter().copied()).collect());

    let ctx = ctx(tenant, owner);
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = svc
            .list_versions(&ctx, file_id, Some(2), cursor.as_deref())
            .await
            .expect("list_versions page");
        seen.extend(page.items.iter().map(|v| v.version_id));
        match page.page_info.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= expected.len(), "walk must terminate");
    }

    assert_eq!(
        seen, expected,
        "cursor walk must cover every version exactly once, in canonical order"
    );
}

#[tokio::test]
async fn versions_cursor_rejects_a_different_file() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let (file_a, _) = seed_file_and_versions(&db, tenant, owner, &[now, now]).await;
    let (file_b, _) = seed_file_and_versions(&db, tenant, owner, &[now]).await;

    let ctx = ctx(tenant, owner);
    let page = svc
        .list_versions(&ctx, file_a, Some(1), None)
        .await
        .expect("first page");
    let cursor = page.page_info.next_cursor.expect("more pages remain");

    let err = svc
        .list_versions(&ctx, file_b, Some(1), Some(&cursor))
        .await
        .expect_err("a cursor issued for a different file must be rejected");
    assert!(
        matches!(err, DomainError::Cursor(_)),
        "expected a cursor error, got {err:?}"
    );
}

fn versions_router(ctx: SecurityContext, svc: Arc<FileService>) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/versions"),
            get(handlers::list_versions),
        )
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(svc))
}

#[tokio::test]
async fn versions_offset_query_param_is_rejected_as_400() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ));
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let (file_id, _) =
        seed_file_and_versions(&db, tenant, owner, &[OffsetDateTime::now_utc()]).await;

    let router = versions_router(ctx(tenant, owner), svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/files/{file_id}/versions?offset=0")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn versions_limit_zero_is_rejected_as_400() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ));
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let (file_id, _) =
        seed_file_and_versions(&db, tenant, owner, &[OffsetDateTime::now_utc()]).await;

    let router = versions_router(ctx(tenant, owner), svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/files/{file_id}/versions?limit=0")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Manifest-byte-budget truncation must still produce a `next_cursor`, and
/// resuming from it must return exactly the remaining versions with no gap
/// or duplicate -- see `FileService::list_versions_with_manifests`'s doc
/// comment. Manifests are inserted directly via `VersionRepo::insert_manifest`
/// (bypassing a real multipart upload, which this budget interaction does not
/// need) so the test stays cheap.
#[tokio::test]
async fn versions_manifest_budget_truncation_forces_next_cursor_with_no_gap_or_duplicate() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let svc = FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    );

    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let base = OffsetDateTime::now_utc();
    // Three composite versions, newest first once listed (created_at DESC):
    // v2 (base+2), v1 (base+1), v0 (base). Each manifest is ~3 MiB -- under
    // the 4 MiB aggregate budget alone, but v2 + v1 together (~6 MiB) exceed
    // it, so the page must cut short right after v2.
    let conn = db.conn().expect("conn");
    let files = FileRepo::new();
    let versions = VersionRepo::new();
    let scope = AccessScope::allow_all();
    let file_id = Uuid::now_v7();
    files
        .create(
            &conn,
            &scope,
            &File {
                file_id,
                tenant_id: tenant,
                owner_kind: OwnerKind::User,
                owner_id: owner,
                name: "doc.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                content_id: None,
                meta_version: 0,
                created_at: base,
                last_modified_at: base,
            },
        )
        .await
        .expect("create file");

    let manifest_text = "x".repeat(3 * 1024 * 1024);
    let mut version_ids = Vec::new();
    for i in 0..3u8 {
        let version_id = Uuid::now_v7();
        let created_at = base + time::Duration::seconds(i64::from(i));
        let mut v = new_version(file_id, version_id, created_at);
        v.hash_mode = "multipart-composite-sha256".to_owned();
        v.part_count = Some(2);
        versions
            .insert(&conn, &scope, &v)
            .await
            .expect("insert composite version");
        versions
            .insert_manifest(&conn, &scope, version_id, &manifest_text, created_at)
            .await
            .expect("insert manifest");
        version_ids.push(version_id);
    }
    // Newest-first order: v2 (base+2), v1 (base+1), v0 (base).
    let v2 = version_ids[2];
    let v1 = version_ids[1];
    let v0 = version_ids[0];

    let ctx = ctx(tenant, owner);
    let page1 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), None)
        .await
        .expect("page 1");
    assert_eq!(
        page1.items.len(),
        1,
        "the manifest budget must cut the page short after the first (largest-budget) version"
    );
    assert_eq!(page1.items[0].0.version_id, v2);
    let cursor = page1
        .page_info
        .next_cursor
        .clone()
        .expect("a budget-truncated page must still carry a next_cursor");

    // Each ~3 MiB manifest is under the 4 MiB budget alone, but any two
    // together (~6 MiB) exceed it -- so the budget also cuts page 2 short to
    // just `v1`, forcing a second next_cursor; only page 3 reaches `v0`.
    let page2 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(&cursor))
        .await
        .expect("page 2");
    assert_eq!(
        page2
            .items
            .iter()
            .map(|(v, _)| v.version_id)
            .collect::<Vec<_>>(),
        vec![v1],
        "page 2 must resume exactly at v1 -- no gap, no repeat of v2"
    );
    let cursor2 = page2
        .page_info
        .next_cursor
        .clone()
        .expect("page 2 is budget-truncated too, so it must also carry a next_cursor");

    let page3 = svc
        .list_versions_with_manifests(&ctx, file_id, Some(10), Some(&cursor2))
        .await
        .expect("page 3");
    assert_eq!(
        page3
            .items
            .iter()
            .map(|(v, _)| v.version_id)
            .collect::<Vec<_>>(),
        vec![v0],
        "page 3 must resume exactly at v0 -- no gap, no repeat of v1"
    );
    assert!(
        page3.page_info.next_cursor.is_none(),
        "must be the last page"
    );
}

// =============================================================================
// GET /retention-rules
// =============================================================================

async fn build_retention_harness() -> (
    Arc<FileService>,
    Arc<PolicyService>,
    Arc<ScopedTestAuthorizer>,
    Uuid,
    Uuid,
) {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authz = Arc::new(ScopedTestAuthorizer::new());
    let authorizer: Arc<dyn Authorizer> = Arc::clone(&authz) as Arc<dyn Authorizer>;
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        Arc::clone(&authorizer),
        base_config(),
        None,
        None,
    ));
    let policy_svc = Arc::new(PolicyService::new(
        policy_store,
        authorizer,
        base_config().default_page_size,
        base_config().max_page_size,
    ));
    let tenant = Uuid::now_v7();
    let subject = Uuid::now_v7();
    (svc, policy_svc, authz, tenant, subject)
}

/// Full-visibility set a non-admin caller must see, walked page by page, must
/// match exactly what the pre-SQL-filter application logic used to compute
/// (see `tests/policy_authz_test.rs`'s equivalent single-page assertions),
/// and every page but the last must be exactly `limit`-sized -- proof the
/// filter runs in SQL, not after an unconditional fetch.
#[tokio::test]
async fn retention_rules_non_admin_cursor_walk_matches_full_visibility_set_and_pages_are_full() {
    let (svc, policy_svc, authz, tenant, subject) = build_retention_harness().await;
    let ctx_subject = ctx(tenant, subject);
    let other_user = Uuid::now_v7();
    let ctx_other = ctx(tenant, other_user);

    authz.set_admin(true);
    // Visible to `subject`: tenant-scope rule, subject's own user-scope rule,
    // subject's own file's file-scope rule.
    let tenant_rule = policy_svc
        .create_retention_rule(
            &ctx_subject,
            RetentionScope::Tenant,
            None,
            valid_rule_body(),
        )
        .await
        .expect("create tenant rule");
    let subject_user_rule = policy_svc
        .create_retention_rule(
            &ctx_subject,
            RetentionScope::User,
            Some(subject),
            valid_rule_body(),
        )
        .await
        .expect("create subject's user rule");
    let own_file = svc
        .create_file_bare(&ctx_subject, new_file(subject))
        .await
        .expect("create own file");
    let subject_file_rule = policy_svc
        .create_retention_rule(
            &ctx_subject,
            RetentionScope::File,
            Some(own_file),
            valid_rule_body(),
        )
        .await
        .expect("create subject's file rule");
    // Not visible to `subject`: another user's user-scope rule, another
    // owner's file-scope rule.
    policy_svc
        .create_retention_rule(
            &ctx_subject,
            RetentionScope::User,
            Some(other_user),
            valid_rule_body(),
        )
        .await
        .expect("create other user's rule");
    let other_file = svc
        .create_file_bare(&ctx_other, new_file(other_user))
        .await
        .expect("create other's file");
    policy_svc
        .create_retention_rule(
            &ctx_subject,
            RetentionScope::File,
            Some(other_file),
            valid_rule_body(),
        )
        .await
        .expect("create other's file rule");

    let expected: std::collections::HashSet<Uuid> = [
        tenant_rule.rule_id,
        subject_user_rule.rule_id,
        subject_file_rule.rule_id,
    ]
    .into_iter()
    .collect();

    authz.set_admin(false);
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = policy_svc
            .list_retention_rules(&ctx_subject, Some(2), cursor.as_deref())
            .await
            .expect("list_retention_rules page");
        let page_len = page.items.len();
        seen.extend(page.items.iter().map(|r| r.rule_id));
        match page.page_info.next_cursor.clone() {
            Some(next) => {
                assert_eq!(
                    page_len, 2,
                    "every page but the last must be exactly `limit`-sized -- the \
                     visibility filter runs in SQL, not after fetch"
                );
                cursor = Some(next);
            }
            None => break,
        }
        assert!(seen.len() <= expected.len() + 3, "walk must terminate");
    }

    let seen_set: std::collections::HashSet<Uuid> = seen.into_iter().collect();
    assert_eq!(
        seen_set, expected,
        "non-admin cursor walk must return exactly the visible set"
    );
}

#[tokio::test]
async fn retention_rules_admin_cursor_walk_covers_every_rule_in_tenant() {
    let (svc, policy_svc, authz, tenant, subject) = build_retention_harness().await;
    let ctx_subject = ctx(tenant, subject);
    authz.set_admin(true);

    let mut created = Vec::new();
    for _ in 0..5 {
        let r = policy_svc
            .create_retention_rule(
                &ctx_subject,
                RetentionScope::Tenant,
                None,
                valid_rule_body(),
            )
            .await
            .expect("create rule");
        created.push(r.rule_id);
    }
    let _ = &svc;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = policy_svc
            .list_retention_rules(&ctx_subject, Some(2), cursor.as_deref())
            .await
            .expect("list_retention_rules page");
        seen.extend(page.items.iter().map(|r| r.rule_id));
        match page.page_info.next_cursor.clone() {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= created.len(), "walk must terminate");
    }

    let seen_set: std::collections::HashSet<Uuid> = seen.into_iter().collect();
    let expected_set: std::collections::HashSet<Uuid> = created.into_iter().collect();
    assert_eq!(
        seen_set, expected_set,
        "admin must see every rule in the tenant"
    );
}

fn retention_rules_router(ctx: SecurityContext, policy_svc: Arc<PolicyService>) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/retention-rules"),
            get(handlers::list_retention_rules),
        )
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(policy_svc))
}

#[tokio::test]
async fn retention_rules_offset_query_param_is_rejected_as_400() {
    let (_svc, policy_svc, authz, tenant, subject) = build_retention_harness().await;
    authz.set_admin(true);
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/retention-rules?offset=0")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retention_rules_limit_zero_is_rejected_as_400() {
    let (_svc, policy_svc, authz, tenant, subject) = build_retention_harness().await;
    authz.set_admin(true);
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/retention-rules?limit=0")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retention_rules_garbage_cursor_is_rejected_as_400() {
    let (_svc, policy_svc, authz, tenant, subject) = build_retention_harness().await;
    authz.set_admin(true);
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/retention-rules?cursor=not-a-real-cursor"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A cursor from a listing built under a different order (`domain::pagination`'s
/// `s` field) than the endpoint's canonical one must be rejected -- proves
/// `ORDER_MISMATCH` is reachable, not just `INVALID_CURSOR`/`FILTER_MISMATCH`.
#[tokio::test]
async fn retention_rules_cursor_with_wrong_order_field_is_rejected() {
    let (_svc, policy_svc, authz, tenant, subject) = build_retention_harness().await;
    authz.set_admin(true);
    let bogus = pagination::encode(
        OffsetDateTime::now_utc(),
        Uuid::now_v7(),
        pagination::FILES_ID_FIELD, // wrong id field for /retention-rules
        None,
    )
    .expect("encode");
    let err = policy_svc
        .list_retention_rules(&ctx(tenant, subject), Some(2), Some(&bogus))
        .await
        .expect_err("a cursor built for a different listing's order must be rejected");
    assert!(matches!(err, DomainError::Cursor(_)));
}
