//! Cursor-pagination contract for `/files`, `/files/{id}/versions`, `/retention-rules` and
//! `/storages`:
//! full walks in both directions (including `created_at` ties) and cursor errors.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

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
use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer};
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

/// Real default page sizes, so `?limit` clamping tests observe the real ceiling.
fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 25,
        max_page_size: 200,
        idempotency_ttl_secs: 86400,
    }
}

/// Parse a wire token the way the `OData` extractor does before the services see it.
fn cur(token: &str) -> toolkit_odata::CursorV1 {
    toolkit_odata::CursorV1::decode(token).expect("decode cursor")
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

/// Grants every action within the caller's tenant.
struct ScopedTestAuthorizer;

#[async_trait]
impl Authorizer for ScopedTestAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        _action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

/// Seeds `n` files for one owner via `FileRepo` with pinned `created_at` (`instants[i]`) to
/// exercise the tie-break at page boundaries.
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

/// Canonical order for every listing: `created_at DESC`, id column `DESC`.
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
    // 7 files over 3 instants, so a small `limit` walk crosses a `created_at` tie.
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
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = svc
            .list_files(&ctx, owner_filter, Some(2), cursor.as_deref().map(cur))
            .await
            .expect("list_files page");
        prev_cursors.push(page.page_info.prev_cursor.clone());
        let page_ids: Vec<Uuid> = page.items.iter().map(|f| f.file_id).collect();
        seen.extend(page_ids.iter().copied());
        pages.push(page_ids);
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
    assert_eq!(
        prev_cursors[0], None,
        "the very first page (no cursor supplied) must have prev_cursor = null"
    );
    assert!(pages.len() > 1, "test must exercise more than one page");

    // Walking back from the last page's `prev_cursor` reproduces the same pages.
    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = svc
            .list_files(&ctx, owner_filter, Some(2), Some(cur(&back_cursor)))
            .await
            .expect("list_files backward page");
        back_pages.push(page.items.iter().map(|f| f.file_id).collect());
        match page.page_info.prev_cursor {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    let expected_back = &pages[..pages.len() - 1];
    assert_eq!(
        back_pages, expected_back,
        "walking prev_cursor backward from the last page must reproduce every \
         earlier page, in the same canonical order"
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
        .list_files(&ctx_b, filter_for_b, Some(1), Some(cur(&cursor)))
        .await
        .expect_err("a cursor issued for a different owner must be rejected");
    assert!(
        matches!(err, DomainError::Cursor(_)),
        "expected a cursor error, got {err:?}"
    );
}

#[tokio::test]
async fn files_backward_cursor_rejects_a_different_owner() {
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
    let page1 = svc
        .list_files(&ctx_a, filter_for_a, Some(1), None)
        .await
        .expect("page 1");
    let next = page1.page_info.next_cursor.expect("more pages remain");
    let page2 = svc
        .list_files(&ctx_a, filter_for_a, Some(1), Some(cur(&next)))
        .await
        .expect("page 2");
    let prev = page2
        .page_info
        .prev_cursor
        .expect("page 2 must have a predecessor");

    let filter_for_b = OwnerFilter {
        owner_kind: OwnerKind::User,
        owner_id: owner_b,
    };
    let ctx_b = ctx(tenant, owner_b);
    let err = svc
        .list_files(&ctx_b, filter_for_b, Some(1), Some(cur(&prev)))
        .await
        .expect_err("a backward cursor issued for a different owner must be rejected");
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
    svc.create_file(&ctx(tenant, owner), new_file(owner), None)
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
    let body = body_json(resp).await;
    assert!(
        body.to_string().contains("INVALID_CURSOR"),
        "an undecodable cursor must be INVALID_CURSOR: {body}"
    );
}

#[tokio::test]
async fn files_offset_query_param_is_rejected_as_400() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    // The old offset param must be rejected, not silently ignored.
    assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&offset=0"),
        &[("offset", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;
}

#[tokio::test]
async fn files_limit_zero_is_rejected_as_400() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&limit=0"),
        &[("$top", "INVALID_LIMIT")],
    )
    .await;
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
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = svc
            .list_versions(&ctx, file_id, Some(2), cursor.as_deref().map(cur))
            .await
            .expect("list_versions page");
        prev_cursors.push(page.page_info.prev_cursor.clone());
        let page_ids: Vec<Uuid> = page.items.iter().map(|v| v.version_id).collect();
        seen.extend(page_ids.iter().copied());
        pages.push(page_ids);
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
    assert_eq!(
        prev_cursors[0], None,
        "the very first page (no cursor supplied) must have prev_cursor = null"
    );
    assert!(pages.len() > 1, "test must exercise more than one page");

    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = svc
            .list_versions(&ctx, file_id, Some(2), Some(cur(&back_cursor)))
            .await
            .expect("list_versions backward page");
        back_pages.push(page.items.iter().map(|v| v.version_id).collect());
        match page.page_info.prev_cursor {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    let expected_back = &pages[..pages.len() - 1];
    assert_eq!(
        back_pages, expected_back,
        "walking prev_cursor backward from the last page must reproduce every \
         earlier page, in the same canonical order"
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
        .list_versions(&ctx, file_b, Some(1), Some(cur(&cursor)))
        .await
        .expect_err("a cursor issued for a different file must be rejected");
    assert!(
        matches!(err, DomainError::Cursor(_)),
        "expected a cursor error, got {err:?}"
    );
}

#[tokio::test]
async fn versions_backward_cursor_rejects_a_different_file() {
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
    let page1 = svc
        .list_versions(&ctx, file_a, Some(1), None)
        .await
        .expect("page 1");
    let next = page1.page_info.next_cursor.expect("more pages remain");
    let page2 = svc
        .list_versions(&ctx, file_a, Some(1), Some(cur(&next)))
        .await
        .expect("page 2");
    let prev = page2
        .page_info
        .prev_cursor
        .expect("page 2 must have a predecessor");

    let err = svc
        .list_versions(&ctx, file_b, Some(1), Some(cur(&prev)))
        .await
        .expect_err("a backward cursor issued for a different file must be rejected");
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

async fn build_retention_harness() -> (Arc<FileService>, Arc<PolicyService>, Uuid, Uuid) {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(ScopedTestAuthorizer);
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
    (svc, policy_svc, tenant, subject)
}

#[tokio::test]
async fn retention_rules_cursor_walk_covers_every_rule_in_tenant() {
    let (svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let ctx_subject = ctx(tenant, subject);

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
    let mut pages: Vec<Vec<Uuid>> = Vec::new();
    let mut prev_cursors: Vec<Option<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = policy_svc
            .list_retention_rules(&ctx_subject, Some(2), cursor.as_deref().map(cur))
            .await
            .expect("list_retention_rules page");
        prev_cursors.push(page.page_info.prev_cursor.clone());
        let page_ids: Vec<Uuid> = page.items.iter().map(|r| r.rule_id).collect();
        seen.extend(page_ids.iter().copied());
        pages.push(page_ids);
        match page.page_info.next_cursor.clone() {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= created.len(), "walk must terminate");
    }

    let seen_set: std::collections::HashSet<Uuid> = seen.iter().copied().collect();
    let expected_set: std::collections::HashSet<Uuid> = created.into_iter().collect();
    assert_eq!(
        seen_set, expected_set,
        "the walk must return every rule in the tenant"
    );
    assert_eq!(
        prev_cursors[0], None,
        "the very first page (no cursor supplied) must have prev_cursor = null"
    );
    assert!(pages.len() > 1, "test must exercise more than one page");

    let mut back_pages: Vec<Vec<Uuid>> = Vec::new();
    let mut back_cursor = prev_cursors
        .last()
        .cloned()
        .flatten()
        .expect("the last page must have a predecessor");
    loop {
        let page = policy_svc
            .list_retention_rules(&ctx_subject, Some(2), Some(cur(&back_cursor)))
            .await
            .expect("list_retention_rules backward page");
        back_pages.push(page.items.iter().map(|r| r.rule_id).collect());
        match page.page_info.prev_cursor {
            Some(prev) => back_cursor = prev,
            None => break,
        }
    }
    back_pages.reverse();
    let expected_back = &pages[..pages.len() - 1];
    assert_eq!(
        back_pages, expected_back,
        "walking prev_cursor backward from the last page must reproduce every \
         earlier page, in the same canonical order"
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
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/retention-rules?offset=0")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retention_rules_limit_zero_is_rejected_as_400() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    let resp = router
        .oneshot(get_req(format!("{BASE}/retention-rules?limit=0")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn retention_rules_garbage_cursor_is_rejected_as_400() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    let resp = router
        .oneshot(get_req(format!(
            "{BASE}/retention-rules?cursor=not-a-real-cursor"
        )))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A cursor built under a different order must be rejected (`ORDER_MISMATCH`).
#[tokio::test]
async fn retention_rules_cursor_with_wrong_order_field_is_rejected() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let bogus = pagination::encode(
        OffsetDateTime::now_utc(),
        Uuid::now_v7(),
        pagination::FILES_ID_FIELD, // wrong id field for /retention-rules
        None,
        pagination::Direction::Forward,
    )
    .expect("encode");
    let err = policy_svc
        .list_retention_rules(&ctx(tenant, subject), Some(2), Some(cur(&bogus)))
        .await
        .expect_err("a cursor built for a different listing's order must be rejected");
    assert!(matches!(err, DomainError::Cursor(_)));
}

/// Same as above for a backward cursor.
#[tokio::test]
async fn retention_rules_backward_cursor_with_wrong_order_field_is_rejected() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let bogus = pagination::encode(
        OffsetDateTime::now_utc(),
        Uuid::now_v7(),
        pagination::FILES_ID_FIELD, // wrong id field for /retention-rules
        None,
        pagination::Direction::Backward,
    )
    .expect("encode");
    let err = policy_svc
        .list_retention_rules(&ctx(tenant, subject), Some(2), Some(cur(&bogus)))
        .await
        .expect_err("a backward cursor built for a different listing's order must be rejected");
    assert!(matches!(err, DomainError::Cursor(_)));
}

// --- REST query-string contract: `$top`/`$skiptoken` synonyms and rejected options ---

/// `(field, reason)` of every violation of a canonical 400 body, in order.
fn violations(body: &Value) -> Vec<(String, String)> {
    body["context"]["field_violations"]
        .as_array()
        .unwrap_or_else(|| panic!("no field_violations in {body}"))
        .iter()
        .map(|v| {
            (
                v["field"].as_str().expect("field").to_owned(),
                v["reason"].as_str().expect("reason").to_owned(),
            )
        })
        .collect()
}

async fn assert_rejected(router: &Router, uri: String, expected: &[(&str, &str)]) -> Value {
    let resp = router
        .clone()
        .oneshot(get_req(uri.clone()))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
    let body = body_json(resp).await;
    let got = violations(&body);
    let expected: Vec<(String, String)> = expected
        .iter()
        .map(|(f, r)| ((*f).to_owned(), (*r).to_owned()))
        .collect();
    assert_eq!(got, expected, "{uri}: {body}");
    body
}

async fn get_ok(router: &Router, uri: String) -> Value {
    let resp = router
        .clone()
        .oneshot(get_req(uri.clone()))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    body_json(resp).await
}

#[tokio::test]
async fn files_top_is_a_synonym_of_limit() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let body = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$top=1"),
    )
    .await;
    assert_eq!(body["page_info"]["limit"], 1);
}

#[tokio::test]
async fn files_skiptoken_is_a_synonym_of_cursor() {
    let (svc, subject, owner) = build_files_harness().await;
    svc.create_file(&subject, new_file(owner), None)
        .await
        .expect("seed a second file");
    let router = files_router(subject, svc);
    let first = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$top=1"),
    )
    .await;
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .expect("a second file exists");
    let second = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$skiptoken={next}"),
    )
    .await;
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_ne!(
        second["items"][0]["file_id"], first["items"][0]["file_id"],
        "the continuation page must hold the other file"
    );
}

#[tokio::test]
async fn files_both_spellings_of_one_parameter_are_rejected() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let both_limits = assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&limit=1&$top=1"),
        &[("query", "INVALID_QUERY_PARAMS")],
    )
    .await;
    assert_eq!(both_limits["status"], 400);
    assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&cursor=X&$skiptoken=X"),
        &[("query", "INVALID_QUERY_PARAMS")],
    )
    .await;
}

#[tokio::test]
async fn files_skip_is_rejected_with_a_skiptoken_hint() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let body = assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$skip=1"),
        &[("$skip", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;
    let description = body["context"]["field_violations"][0]["description"]
        .as_str()
        .expect("description");
    assert!(description.contains("$skiptoken"), "{description}");
}

#[tokio::test]
async fn files_filter_orderby_and_select_are_each_rejected() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let prefix = format!("{BASE}/files?owner_kind=user&owner_id={owner}");
    for (query, field) in [
        ("$filter=name%20eq%20'a'", "$filter"),
        ("$orderby=created_at%20desc", "$orderby"),
        ("$select=file_id", "$select"),
    ] {
        assert_rejected(
            &router,
            format!("{prefix}&{query}"),
            &[(field, "UNSUPPORTED_QUERY_PARAM")],
        )
        .await;
    }
    // All offences of one request come back together.
    assert_rejected(
        &router,
        format!("{prefix}&$filter=name%20eq%20'a'&$select=file_id"),
        &[
            ("$filter", "UNSUPPORTED_QUERY_PARAM"),
            ("$select", "UNSUPPORTED_QUERY_PARAM"),
        ],
    )
    .await;
}

#[tokio::test]
async fn files_unknown_plain_keys_are_all_reported() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    let body = assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&offset=0&foo=1&offset=2"),
        &[
            ("offset", "UNSUPPORTED_QUERY_PARAM"),
            ("foo", "UNSUPPORTED_QUERY_PARAM"),
        ],
    )
    .await;
    let description = body["context"]["field_violations"][0]["description"]
        .as_str()
        .expect("description");
    assert!(
        description.contains("owner_kind") && description.contains("$top"),
        "the error must list what the listing accepts: {description}"
    );
}

#[tokio::test]
async fn files_zero_limit_is_invalid_limit_in_either_spelling() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    for spelling in ["$top", "limit"] {
        assert_rejected(
            &router,
            format!("{BASE}/files?owner_kind=user&owner_id={owner}&{spelling}=0"),
            &[("$top", "INVALID_LIMIT")],
        )
        .await;
    }
}

#[tokio::test]
async fn files_miscased_system_option_is_rejected() {
    let (svc, subject, owner) = build_files_harness().await;
    let router = files_router(subject, svc);
    assert_rejected(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&$Top=1"),
        &[("$Top", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;
}

/// The three listings that take no owner pair share one query contract.
async fn assert_plain_listing_contract(router: &Router, path: &str) {
    let body = get_ok(router, format!("{BASE}{path}?$top=1")).await;
    assert_eq!(body["page_info"]["limit"], 1, "{path}");
    assert_rejected(
        router,
        format!("{BASE}{path}?$skip=1"),
        &[("$skip", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;
    assert_rejected(
        router,
        format!("{BASE}{path}?foo=1"),
        &[("foo", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;
    assert_rejected(
        router,
        format!("{BASE}{path}?$filter=x%20eq%201&$orderby=id"),
        &[
            ("$filter", "UNSUPPORTED_QUERY_PARAM"),
            ("$orderby", "UNSUPPORTED_QUERY_PARAM"),
        ],
    )
    .await;
}

#[tokio::test]
async fn versions_query_contract() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let svc = Arc::new(FileService::new(
        store,
        backends,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        Arc::new(TenantOnlyAuthorizer),
        base_config(),
        None,
        None,
    ));
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let (file_id, _) = seed_file_and_versions(
        &db,
        tenant,
        owner,
        &[OffsetDateTime::now_utc(), OffsetDateTime::now_utc()],
    )
    .await;
    let router = versions_router(ctx(tenant, owner), svc);
    assert_plain_listing_contract(&router, &format!("/files/{file_id}/versions")).await;
}

#[tokio::test]
async fn retention_rules_query_contract() {
    let (_svc, policy_svc, tenant, subject) = build_retention_harness().await;
    let router = retention_rules_router(ctx(tenant, subject), policy_svc);
    assert_plain_listing_contract(&router, "/retention-rules").await;
}

fn storages_router(ctx: SecurityContext, svc: Arc<FileService>) -> Router {
    Router::new()
        .route(&format!("{BASE}/files"), get(handlers::list_files))
        .route(&format!("{BASE}/storages"), get(handlers::list_storages))
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(svc))
}

/// Three backends registered out of id order, plus two files for cursors of another listing.
async fn build_storages_harness() -> (Router, Uuid) {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let backends = ["mem-b", "mem-c", "mem-a"]
        .into_iter()
        .map(|id| Arc::new(InMemoryBackend::new(id)) as Arc<dyn StorageBackend>)
        .collect();
    let backends = BackendRegistry::new(backends, "mem-a").expect("registry");
    let svc = Arc::new(FileService::new(
        store,
        backends,
        Arc::new(Issuer::generate(3600).expect("issuer")),
        Arc::new(TenantOnlyAuthorizer),
        base_config(),
        None,
        None,
    ));
    let tenant = Uuid::now_v7();
    let owner = Uuid::now_v7();
    for _ in 0..2 {
        svc.create_file(&ctx(tenant, owner), new_file(owner), None)
            .await
            .expect("seed file");
    }
    (storages_router(ctx(tenant, owner), svc), owner)
}

fn storage_ids(body: &Value) -> Vec<&str> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect()
}

#[tokio::test]
async fn storages_are_paged_by_id_in_both_directions() {
    let (router, _owner) = build_storages_harness().await;

    let all = get_ok(&router, format!("{BASE}/storages")).await;
    assert_eq!(storage_ids(&all), ["mem-a", "mem-b", "mem-c"]);
    assert!(all["page_info"]["next_cursor"].is_null());
    assert!(all["page_info"]["prev_cursor"].is_null());

    let first = get_ok(&router, format!("{BASE}/storages?limit=1")).await;
    assert_eq!(storage_ids(&first), ["mem-a"]);
    assert_eq!(first["page_info"]["limit"], 1);
    assert!(first["page_info"]["prev_cursor"].is_null());
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .expect("more storages follow");

    let second = get_ok(
        &router,
        format!("{BASE}/storages?limit=1&$skiptoken={next}"),
    )
    .await;
    assert_eq!(storage_ids(&second), ["mem-b"]);
    let prev = second["page_info"]["prev_cursor"]
        .as_str()
        .expect("a page precedes");
    assert!(second["page_info"]["next_cursor"].is_string());

    let back = get_ok(&router, format!("{BASE}/storages?limit=1&cursor={prev}")).await;
    assert_eq!(storage_ids(&back), ["mem-a"]);
    assert!(back["page_info"]["prev_cursor"].is_null());
    assert!(back["page_info"]["next_cursor"].is_string());
}

#[tokio::test]
async fn storages_reject_unsupported_options_and_foreign_cursors() {
    let (router, owner) = build_storages_harness().await;
    assert_rejected(
        &router,
        format!("{BASE}/storages?$skip=1"),
        &[("$skip", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;
    assert_rejected(
        &router,
        format!("{BASE}/storages?foo=1"),
        &[("foo", "UNSUPPORTED_QUERY_PARAM")],
    )
    .await;

    let files = get_ok(
        &router,
        format!("{BASE}/files?owner_kind=user&owner_id={owner}&limit=1"),
    )
    .await;
    let files_cursor = files["page_info"]["next_cursor"]
        .as_str()
        .expect("a second file exists");
    let resp = router
        .clone()
        .oneshot(get_req(format!("{BASE}/storages?cursor={files_cursor}")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body.to_string().contains("ORDER_MISMATCH"),
        "a /files cursor must be an order mismatch on /storages: {body}"
    );
}
