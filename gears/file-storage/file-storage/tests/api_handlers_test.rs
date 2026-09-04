//! Handler-level REST tests for `src/api/rest/handlers.rs`, targeting the
//! recently changed branches this file's coverage gap centers on: the new
//! READ gate on `list_storages`/`get_storage`, `update_metadata`'s
//! `If-Match-Metadata` header parsing, `bind`'s CAS branches, and the
//! token-authenticated s2s callbacks `finalize_version` /
//! `report_multipart_part`.
//!
//! Drives real `axum::Router`s via `Router::oneshot`, mirroring the minimal
//! router-plus-`Extension`-layers pattern used in `enforce_test.rs` and
//! `multipart_test.rs` (a router carrying only the route(s) under test, not
//! the full app).
//!
//! Error-body assertions check the RFC 9457 `context` payload by field name
//! and reason code, not by message substring, per this repo's canonical
//! error-mapping contract (`src/api/rest/error.rs`, pinned by
//! `error_mapping_test.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, patch, post};
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
use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer, actions};
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{DEFAULT_MIN_PART_SIZE, MultipartPlan};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::{DataPlanePort, MultipartStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::{Issuer, Verifier};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");
const BASE: &str = "/api/file-storage/v1";

// -- shared test harness -----------------------------------------------------

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-api-handlers-{}.db", Uuid::now_v7().simple()));
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

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
}

fn new_file(owner: Uuid, mime: &str) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: owner,
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: mime.to_owned(),
        custom_metadata: vec![],
    }
}

fn base_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    }
}

/// Read a router response body as parsed JSON.
async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid JSON body")
}

/// Pull the `fs-token` query value out of a signed sidecar URL, matching the
/// extraction pattern used in `enforce_test.rs` / `multipart_test.rs`.
fn token_from_url(url: &str) -> String {
    let start = url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len();
    url[start..].to_owned()
}

/// Denies `actions::READ` while `allow_read` is false; otherwise grants like
/// `TenantOnlyAuthorizer`. Local to this file -- exercises the READ gate
/// recently added to `list_storages`/`get_storage`.
struct ReadGateAuthorizer {
    allow_read: AtomicBool,
}

impl ReadGateAuthorizer {
    fn new(allow_read: bool) -> Self {
        Self {
            allow_read: AtomicBool::new(allow_read),
        }
    }
}

#[async_trait]
impl Authorizer for ReadGateAuthorizer {
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        _gts_file_type: &str,
        _file_id: Option<Uuid>,
    ) -> Result<AccessScope, DomainError> {
        if action == actions::READ && !self.allow_read.load(Ordering::SeqCst) {
            return Err(DomainError::Forbidden);
        }
        Ok(AccessScope::for_tenant(ctx.subject_tenant_id()))
    }
}

// -- 1. list_storages / get_storage: READ gate -------------------------------

async fn build_storages_svc(allow_read: bool) -> Arc<FileService> {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(ReadGateAuthorizer::new(allow_read));
    let store = Store::new(Arc::clone(&db));
    Arc::new(FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ))
}

fn storages_router(svc: &Arc<FileService>, ctx: &SecurityContext) -> Router {
    Router::new()
        .route(&format!("{BASE}/storages"), get(handlers::list_storages))
        .route(
            &format!("{BASE}/storages/{{id}}"),
            get(handlers::get_storage),
        )
        .layer(axum::Extension(ctx.clone()))
        .layer(axum::Extension(Arc::clone(svc)))
}

fn get_req(uri: String) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request")
}

#[tokio::test]
async fn list_storages_with_read_permission_returns_backend_capabilities() {
    let svc = build_storages_svc(true).await;
    let subject = ctx(Uuid::now_v7());
    let router = storages_router(&svc, &subject);

    let resp = router
        .oneshot(get_req(format!("{BASE}/storages")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let items = body.as_array().expect("array body");
    assert_eq!(items.len(), 1, "expected exactly the one 'mem' backend");
    assert_eq!(items[0]["id"], "mem");
    assert_eq!(items[0]["capabilities"]["multipart_native"], true);
    assert_eq!(items[0]["capabilities"]["range_native"], false);
    assert_eq!(items[0]["capabilities"]["encryption_native"], false);
}

#[tokio::test]
async fn list_storages_without_read_permission_returns_403() {
    let svc = build_storages_svc(false).await;
    let subject = ctx(Uuid::now_v7());
    let router = storages_router(&svc, &subject);

    let resp = router
        .oneshot(get_req(format!("{BASE}/storages")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(body["context"]["reason"], "ACCESS_DENIED");
}

#[tokio::test]
async fn get_storage_with_read_permission_returns_backend() {
    let svc = build_storages_svc(true).await;
    let subject = ctx(Uuid::now_v7());
    let router = storages_router(&svc, &subject);

    let resp = router
        .oneshot(get_req(format!("{BASE}/storages/mem")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["id"], "mem");
    assert_eq!(body["capabilities"]["multipart_native"], true);
}

#[tokio::test]
async fn get_storage_without_read_permission_returns_403() {
    let svc = build_storages_svc(false).await;
    let subject = ctx(Uuid::now_v7());
    let router = storages_router(&svc, &subject);

    let resp = router
        .oneshot(get_req(format!("{BASE}/storages/mem")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(body["context"]["reason"], "ACCESS_DENIED");
}

#[tokio::test]
async fn get_storage_unknown_id_returns_400_with_backend_id_field() {
    let svc = build_storages_svc(true).await;
    let subject = ctx(Uuid::now_v7());
    let router = storages_router(&svc, &subject);

    let resp = router
        .oneshot(get_req(format!("{BASE}/storages/does-not-exist")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["field_violations"][0]["field"],
        "backend_id"
    );
    assert_eq!(
        body["context"]["field_violations"][0]["reason"],
        "UNKNOWN_BACKEND"
    );
}

// -- 2. update_metadata: If-Match-Metadata header parsing --------------------

async fn build_metadata_harness() -> (Arc<FileService>, SecurityContext, Uuid, i64) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ));
    let subject = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");
    let (file, _meta) = svc
        .get_file_with_metadata(&subject, ticket.file_id)
        .await
        .expect("get_file_with_metadata");
    (svc, subject, ticket.file_id, file.meta_version)
}

fn metadata_router(svc: &Arc<FileService>, ctx: &SecurityContext) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}"),
            patch(handlers::update_metadata),
        )
        .layer(axum::Extension(ctx.clone()))
        .layer(axum::Extension(Arc::clone(svc)))
}

fn patch_req(uri: String, if_match: Option<&str>, body: &Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(v) = if_match {
        builder = builder.header("if-match-metadata", v);
    }
    builder
        .body(Body::from(
            serde_json::to_vec(body).expect("serialize body"),
        ))
        .expect("build request")
}

#[tokio::test]
async fn update_metadata_correct_if_match_applies_and_returns_updated_metadata() {
    let (svc, subject, file_id, meta_version) = build_metadata_harness().await;
    let router = metadata_router(&svc, &subject);

    let body = json!({ "custom_metadata": { "color": "blue" } });
    let uri = format!("{BASE}/files/{file_id}");
    let req = patch_req(uri, Some(&meta_version.to_string()), &body);
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_body = body_json(resp).await;
    let entries = resp_body["custom_metadata"].as_array().expect("array");
    assert!(
        entries
            .iter()
            .any(|e| e["key"] == "color" && e["value"] == "blue"),
        "expected color=blue in response body, got: {resp_body}"
    );
    assert_ne!(
        resp_body["meta_version"].as_i64(),
        Some(meta_version),
        "meta_version must change after a successful patch"
    );

    // Confirm the DB actually changed, not just the response echo.
    let (_, meta) = svc
        .get_file_with_metadata(&subject, file_id)
        .await
        .expect("re-fetch");
    assert!(meta.iter().any(|e| e.key == "color" && e.value == "blue"));
}

#[tokio::test]
async fn update_metadata_malformed_if_match_returns_400_and_leaves_metadata_unchanged() {
    let (svc, subject, file_id, _meta_version) = build_metadata_harness().await;
    let router = metadata_router(&svc, &subject);

    let body = json!({ "custom_metadata": { "color": "blue" } });
    let uri = format!("{BASE}/files/{file_id}");
    let req = patch_req(uri, Some("not-a-number"), &body);
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp_body = body_json(resp).await;
    assert_eq!(
        resp_body["context"]["field_violations"][0]["field"],
        "if-match-metadata"
    );
    assert_eq!(
        resp_body["context"]["field_violations"][0]["reason"],
        "VALIDATION"
    );

    let (_, meta) = svc
        .get_file_with_metadata(&subject, file_id)
        .await
        .expect("re-fetch");
    assert!(
        !meta.iter().any(|e| e.key == "color"),
        "a malformed header must not apply the patch"
    );
}

#[tokio::test]
async fn update_metadata_absent_if_match_applies_unconditionally() {
    let (svc, subject, file_id, _meta_version) = build_metadata_harness().await;
    let router = metadata_router(&svc, &subject);

    let body = json!({ "custom_metadata": { "color": "green" } });
    let uri = format!("{BASE}/files/{file_id}");
    let req = patch_req(uri, None, &body);
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_body = body_json(resp).await;
    let entries = resp_body["custom_metadata"].as_array().expect("array");
    assert!(
        entries
            .iter()
            .any(|e| e["key"] == "color" && e["value"] == "green"),
        "an absent If-Match-Metadata must still apply the patch unconditionally"
    );
}

// -- 3. bind: success + rebind-without-If-Match conflict ---------------------

async fn build_bind_harness() -> (Arc<FileService>, DataPlaneService, SecurityContext) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        base_config(),
        None,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let subject = ctx(Uuid::now_v7());
    (svc, dp, subject)
}

fn bind_router(svc: &Arc<FileService>, ctx: &SecurityContext) -> Router {
    Router::new()
        .route(&format!("{BASE}/files/{{id}}/bind"), post(handlers::bind))
        .layer(axum::Extension(ctx.clone()))
        .layer(axum::Extension(Arc::clone(svc)))
}

fn bind_req(uri: String, if_match: Option<&str>, version_id: Uuid) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(v) = if_match {
        builder = builder.header("if-match", v);
    }
    let body = json!({ "version_id": version_id });
    builder
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build request")
}

#[tokio::test]
async fn bind_first_bind_returns_file_with_bound_content() {
    let (svc, dp, subject) = build_bind_harness().await;
    let ticket = svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");
    dp.put_content(
        &subject,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"hello"),
    )
    .await
    .expect("put_content finalizes the version");

    let router = bind_router(&svc, &subject);
    let uri = format!("{BASE}/files/{}/bind", ticket.file_id);
    let resp = router
        .oneshot(bind_req(uri, None, ticket.version_id))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["content_id"], ticket.version_id.to_string());
    assert!(body["etag"].is_string(), "a bound file must carry an etag");

    let (file, _) = svc
        .get_file_with_metadata(&subject, ticket.file_id)
        .await
        .expect("re-fetch");
    assert_eq!(
        file.content_id,
        Some(ticket.version_id),
        "the first bind must persist the content pointer"
    );
}

#[tokio::test]
async fn bind_rebind_without_if_match_returns_precondition_conflict() {
    let (svc, dp, subject) = build_bind_harness().await;
    let ticket = svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");
    dp.put_content(
        &subject,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"v1"),
    )
    .await
    .expect("finalize v1");

    let router = bind_router(&svc, &subject);
    let first_uri = format!("{BASE}/files/{}/bind", ticket.file_id);
    let resp = router
        .clone()
        .oneshot(bind_req(first_uri, None, ticket.version_id))
        .await
        .expect("dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the first bind (no content yet) must succeed unconditionally"
    );

    // A second version, finalized but not yet bound.
    let v2 = svc
        .presign_version(&subject, ticket.file_id)
        .await
        .expect("presign v2");
    dp.put_content(
        &subject,
        v2.file_id,
        v2.version_id,
        "text/plain",
        Bytes::from_static(b"v2"),
    )
    .await
    .expect("finalize v2");

    let rebind_uri = format!("{BASE}/files/{}/bind", ticket.file_id);
    let resp = router
        .oneshot(bind_req(rebind_uri, None, v2.version_id))
        .await
        .expect("dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "rebinding already-bound content without If-Match must be rejected"
    );

    let body = body_json(resp).await;
    assert_eq!(body["context"]["violations"][0]["type"], "IF_MATCH");

    // The rejected rebind must not have swapped the content pointer.
    let (file, _) = svc
        .get_file_with_metadata(&subject, ticket.file_id)
        .await
        .expect("re-fetch");
    assert_eq!(file.content_id, Some(ticket.version_id));
}

// -- 4/5. finalize_version + report_multipart_part: token failure branches --

struct MultiHarness {
    svc: Arc<FileService>,
    msvc: Arc<MultipartService>,
    verifier: Arc<Verifier>,
    finalize_auth: Arc<handlers::FinalizeAuth>,
}

async fn build_multi_harness() -> (MultiHarness, SecurityContext) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        base_config(),
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    // No internal-secret gate configured: reproduces the token-only trust
    // model, matching `enforce_test.rs` / `multipart_test.rs`.
    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(None));
    let subject = ctx(Uuid::now_v7());
    (
        MultiHarness {
            svc,
            msvc,
            verifier,
            finalize_auth,
        },
        subject,
    )
}

fn finalize_router(h: &MultiHarness) -> Router {
    let path = format!("{BASE}/files/{{file_id}}/versions/{{version_id}}/finalize");
    Router::new()
        .route(&path, post(handlers::finalize_version))
        .layer(axum::Extension(Arc::clone(&h.verifier)))
        .layer(axum::Extension(Arc::clone(&h.finalize_auth)))
        .layer(axum::Extension(Arc::clone(&h.svc)))
}

fn report_part_router(h: &MultiHarness) -> Router {
    let path = format!(
        "{BASE}/files/{{file_id}}/versions/{{version_id}}/multipart/\
        {{upload_id}}/parts/{{part_number}}/report"
    );
    Router::new()
        .route(&path, post(handlers::report_multipart_part))
        .layer(axum::Extension(Arc::clone(&h.verifier)))
        .layer(axum::Extension(Arc::clone(&h.finalize_auth)))
        .layer(axum::Extension(Arc::clone(&h.msvc)))
}

fn finalize_uri(file_id: Uuid, version_id: Uuid) -> String {
    format!("{BASE}/files/{file_id}/versions/{version_id}/finalize")
}

fn report_uri(file_id: Uuid, version_id: Uuid, upload_id: Uuid, part_number: u32) -> String {
    format!(
        "{BASE}/files/{file_id}/versions/{version_id}/multipart/\
        {upload_id}/parts/{part_number}/report"
    )
}

fn finalize_req(uri: String, token: Option<&str>, size: i64, hash_hex: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("x-fs-token", t);
    }
    let body = json!({ "size": size, "hash_hex": hash_hex });
    builder
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build request")
}

fn report_part_req(
    uri: String,
    token: Option<&str>,
    backend_etag: &str,
    hash_hex: &str,
    size: i64,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("x-fs-token", t);
    }
    let body = json!({ "backend_etag": backend_etag, "hash_hex": hash_hex, "size": size });
    builder
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build request")
}

/// A 3-part plan (min-part-size floor gives [min, min, 3]), matching the
/// validated pattern in `multipart_test.rs`'s
/// `multipart_complete_uses_reported_parts_not_empty_list`.
async fn plan_with_three_parts(
    h: &MultiHarness,
    subject: &SecurityContext,
    file_id: Uuid,
) -> MultipartPlan {
    let declared_size = 2 * DEFAULT_MIN_PART_SIZE + 3;
    let plan = h
        .msvc
        .initiate_multipart_upload(
            subject,
            file_id,
            "application/octet-stream",
            declared_size,
            None,
            None,
            false,
        )
        .await
        .expect("initiate_multipart_upload");
    assert_eq!(
        plan.parts.len(),
        3,
        "declared_size must plan exactly 3 parts"
    );
    plan
}

// -- finalize_version failure branches ---------------------------------------

#[tokio::test]
async fn finalize_version_missing_token_returns_403() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");

    let router = finalize_router(&h);
    let uri = finalize_uri(ticket.file_id, ticket.version_id);
    let req = finalize_req(uri, None, 5, &hex::encode([0u8; 32]));
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["reason"],
        "INVALID_TOKEN: missing x-fs-token header"
    );
}

#[tokio::test]
async fn finalize_version_wrong_op_token_returns_403() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");
    // A multipart-part token minted for the SAME file: file_id and
    // version_id (the plan's own pending version) both resolve, isolating
    // the `claims.op != Op::Put` check.
    let plan = h
        .msvc
        .initiate_multipart_upload(
            &subject,
            ticket.file_id,
            "text/plain",
            10,
            None,
            None,
            false,
        )
        .await
        .expect("initiate_multipart_upload");
    let token = token_from_url(&plan.parts[0].upload_url);

    let router = finalize_router(&h);
    let uri = finalize_uri(ticket.file_id, plan.version_id);
    let req = finalize_req(uri, Some(&token), 5, &hex::encode([0u8; 32]));
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["reason"],
        "INVALID_TOKEN: token does not authorize finalization of this version"
    );
}

#[tokio::test]
async fn finalize_version_token_for_different_version_returns_403() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");
    let token = token_from_url(&ticket.upload_url);

    let router = finalize_router(&h);
    // Same file, but a version_id the token was never minted for.
    let uri = finalize_uri(ticket.file_id, Uuid::now_v7());
    let req = finalize_req(uri, Some(&token), 5, &hex::encode([0u8; 32]));
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["reason"],
        "INVALID_TOKEN: token does not authorize finalization of this version"
    );
}

#[tokio::test]
async fn finalize_version_invalid_hash_length_returns_400() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "text/plain"),
            None,
            false,
        )
        .await
        .expect("create_file");
    let token = token_from_url(&ticket.upload_url);

    let router = finalize_router(&h);
    let uri = finalize_uri(ticket.file_id, ticket.version_id);
    // 16 bytes: valid hex, but not the 32 a SHA-256 digest decodes to.
    let req = finalize_req(uri, Some(&token), 5, &hex::encode([0u8; 16]));
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(body["context"]["field_violations"][0]["field"], "hash_hex");
    assert_eq!(
        body["context"]["field_violations"][0]["reason"],
        "VALIDATION"
    );
}

// -- report_multipart_part failure branches ----------------------------------

#[tokio::test]
async fn report_multipart_part_missing_token_returns_403() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "application/octet-stream"),
            None,
            false,
        )
        .await
        .expect("create_file");
    let plan = plan_with_three_parts(&h, &subject, ticket.file_id).await;
    let part = &plan.parts[0];

    let router = report_part_router(&h);
    let uri = report_uri(
        ticket.file_id,
        plan.version_id,
        plan.upload_id,
        part.part_number,
    );
    let req = report_part_req(uri, None, "etag-1", &hex::encode([1u8; 32]), 100);
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["reason"],
        "INVALID_TOKEN: missing x-fs-token header"
    );
}

#[tokio::test]
async fn report_multipart_part_wrong_op_token_returns_403() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "application/octet-stream"),
            None,
            false,
        )
        .await
        .expect("create_file");
    // A single-part Put token from the SAME file: op mismatches
    // multipart_part, isolating the op check.
    let put_token = token_from_url(&ticket.upload_url);
    let plan = plan_with_three_parts(&h, &subject, ticket.file_id).await;
    let part = &plan.parts[0];

    let router = report_part_router(&h);
    let uri = report_uri(
        ticket.file_id,
        plan.version_id,
        plan.upload_id,
        part.part_number,
    );
    let req = report_part_req(
        uri,
        Some(&put_token),
        "etag-1",
        &hex::encode([1u8; 32]),
        100,
    );
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["reason"],
        "INVALID_TOKEN: token does not authorize reporting this part"
    );
}

#[tokio::test]
async fn report_multipart_part_wrong_part_number_returns_403() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "application/octet-stream"),
            None,
            false,
        )
        .await
        .expect("create_file");
    let plan = plan_with_three_parts(&h, &subject, ticket.file_id).await;
    let token_for_part_0 = token_from_url(&plan.parts[0].upload_url);
    let other_part_number = plan.parts[1].part_number;

    let router = report_part_router(&h);
    // Path names a different part than the one the token authorizes.
    let uri = report_uri(
        ticket.file_id,
        plan.version_id,
        plan.upload_id,
        other_part_number,
    );
    let req = report_part_req(
        uri,
        Some(&token_for_part_0),
        "etag-2",
        &hex::encode([2u8; 32]),
        100,
    );
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = body_json(resp).await;
    assert_eq!(
        body["context"]["reason"],
        "INVALID_TOKEN: token does not authorize reporting this part"
    );
}

#[tokio::test]
async fn report_multipart_part_invalid_hash_length_returns_400() {
    let (h, subject) = build_multi_harness().await;
    let ticket = h
        .svc
        .create_file(
            &subject,
            new_file(Uuid::now_v7(), "application/octet-stream"),
            None,
            false,
        )
        .await
        .expect("create_file");
    let plan = plan_with_three_parts(&h, &subject, ticket.file_id).await;
    let part = &plan.parts[0];
    let token = token_from_url(&part.upload_url);

    let router = report_part_router(&h);
    let uri = report_uri(
        ticket.file_id,
        plan.version_id,
        plan.upload_id,
        part.part_number,
    );
    // 10 bytes: valid hex, but not the 32 a SHA-256 digest decodes to.
    let req = report_part_req(uri, Some(&token), "etag-1", &hex::encode([1u8; 10]), 100);
    let resp = router.oneshot(req).await.expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    assert_eq!(body["context"]["field_violations"][0]["field"], "hash_hex");
    assert_eq!(
        body["context"]["field_violations"][0]["reason"],
        "VALIDATION"
    );
}

// -- 6. error-shape doctrine: RFC 9457 problem+json, no leaked internals -----

#[tokio::test]
async fn error_response_is_rfc9457_problem_json_without_stack_traces() {
    let svc = build_storages_svc(true).await;
    let subject = ctx(Uuid::now_v7());
    let router = storages_router(&svc, &subject);

    let resp = router
        .oneshot(get_req(format!("{BASE}/storages/does-not-exist")))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let content_type = resp
        .headers()
        .get("content-type")
        .expect("content-type header present")
        .to_str()
        .expect("valid header value")
        .to_owned();
    assert!(
        content_type.contains("application/problem+json"),
        "expected application/problem+json, got: {content_type}"
    );

    let body = body_json(resp).await;
    assert!(body["status"].is_number());
    assert!(body["title"].is_string());
    assert!(body["detail"].is_string());
    assert!(body.get("stack").is_none(), "must not leak a stack field");
    assert!(body.get("trace").is_none(), "must not leak a trace field");
    assert!(
        body.get("backtrace").is_none(),
        "must not leak a backtrace field"
    );
}
