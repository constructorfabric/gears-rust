//! Handler-level REST tests for the `POST /files` multipart-intent path in
//! `src/api/rest/handlers.rs::create_file` (upload-flow redesign), plus the
//! adjacent `presign_version` and standalone `initiate_multipart` response
//! shapes.
//!
//! Drives real `axum::Router`s via `Router::oneshot`, mirroring the minimal
//! router-plus-`Extension`-layers pattern used in `api_handlers_test.rs` /
//! `multipart_test.rs` (a router carrying only the route(s) under test).
//!
//! Error-body assertions check the RFC 9457 `context` payload by field name
//! and reason code, not by message substring, per this repo's canonical
//! error-mapping contract (`src/api/rest/error.rs`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::post;
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::authz::{Authorizer, TenantOnlyAuthorizer};
use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerFilter, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");
const BASE: &str = "/api/file-storage/v1";

// -- shared test harness (mirrors api_handlers_test.rs — separate test
// binaries don't share code) ------------------------------------------------

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-multipart-intent-{}.db",
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

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("valid SecurityContext")
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

fn post_req(uri: String, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("serialize body"),
        ))
        .expect("build request")
}

struct Harness {
    svc: Arc<FileService>,
    msvc: Arc<MultipartService>,
    multipart_store: Arc<dyn MultipartStore>,
    subject: SecurityContext,
}

/// One `FileService` + `MultipartService` pair sharing the same `Store`, plus
/// a raw `MultipartStore` handle for asserting on session rows directly
/// (`auto_bind`) the same way `multipart_test.rs`'s `build_redesign_env` does.
async fn build_harness() -> Harness {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn Authorizer> = Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store,
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        base_config(),
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    Harness {
        svc,
        msvc,
        multipart_store,
        subject: ctx(Uuid::now_v7()),
    }
}

fn create_file_router(h: &Harness) -> Router {
    Router::new()
        .route(&format!("{BASE}/files"), post(handlers::create_file))
        .layer(axum::Extension(h.subject.clone()))
        .layer(axum::Extension(Arc::clone(&h.svc)))
        .layer(axum::Extension(Arc::clone(&h.msvc)))
}

fn presign_router(h: &Harness) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/versions"),
            post(handlers::presign_version),
        )
        .layer(axum::Extension(h.subject.clone()))
        .layer(axum::Extension(Arc::clone(&h.svc)))
}

fn initiate_router(h: &Harness) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/multipart"),
            post(handlers::initiate_multipart),
        )
        .layer(axum::Extension(h.subject.clone()))
        .layer(axum::Extension(Arc::clone(&h.msvc)))
}

/// A create-file JSON body with an owner freshly minted per call, so tests
/// don't collide on ownership/quota state.
fn create_body(owner_id: Uuid, extra: &Value) -> Value {
    let mut body = json!({
        "owner_kind": "user",
        "owner_id": owner_id,
        "name": "big.bin",
        "gts_file_type": GTS,
        "mime_type": "application/octet-stream",
    });
    let obj = body.as_object_mut().expect("object body");
    for (k, v) in extra.as_object().expect("extra must be an object") {
        obj.insert(k.clone(), v.clone());
    }
    body
}

/// Declared size that plans to exactly 3 parts at the default min part size
/// (2 full parts + a 3-byte remainder), matching the pattern already
/// validated in `multipart_test.rs`.
const MULTI_PART_DECLARED_SIZE: u64 = 2 * DEFAULT_MIN_PART_SIZE + 3;

// -- create_file: multipart-intent bind validation ---------------------------

#[tokio::test]
async fn create_file_multipart_intent_bind_absent_defaults_to_auto_bind() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    // No `bind` field at all -- must default to the same auto_bind=true
    // behaviour as an explicit `bind: "auto"` (covered separately below).
    let body = create_body(
        owner_id,
        &json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json_body = body_json(resp).await;
    let upload_id: Uuid = json_body["multipart"]["upload_id"]
        .as_str()
        .expect("upload_id string")
        .parse()
        .expect("valid uuid");
    let session = h
        .multipart_store
        .get_multipart_upload(upload_id)
        .await
        .expect("query session")
        .expect("session exists");
    assert!(
        session.auto_bind,
        "an absent bind field must default to auto_bind=true, same as bind:\"auto\""
    );
}

#[tokio::test]
async fn create_file_multipart_intent_bind_auto_plans_multipart_response() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    let body = create_body(
        owner_id,
        &json!({
            "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE },
            "bind": "auto",
        }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get(axum::http::header::LOCATION)
        .expect("Location header on 201")
        .to_str()
        .expect("valid header")
        .to_owned();

    let json_body = body_json(resp).await;
    let file_id: Uuid = json_body["file_id"]
        .as_str()
        .expect("file_id string")
        .parse()
        .expect("valid uuid");
    assert!(
        location.ends_with(&file_id.to_string()),
        "Location must point at the new file: {location}"
    );

    // Single-part `upload_url` must be entirely absent (not null) — the
    // multipart plan is the only upload path in this response.
    assert!(
        json_body.get("upload_url").is_none(),
        "upload_url must be omitted on a multipart-plan response, got: {json_body}"
    );
    let plan = &json_body["multipart"];
    assert!(plan.is_object(), "multipart plan must be present");
    let parts = plan["parts"].as_array().expect("parts array");
    assert_eq!(parts.len(), 3, "declared_size plans to exactly 3 parts");
    assert!(plan["upload_id"].is_string());

    // The plan's own version_id must match the ticket's version_id.
    assert_eq!(json_body["version_id"], plan["version_id"]);

    // `bind: "auto"` (the default) must have been threaded into the
    // multipart session as auto_bind = true — this is what makes `complete`
    // bind the version itself later, with no separate client `bind` call.
    let upload_id: Uuid = plan["upload_id"]
        .as_str()
        .expect("upload_id string")
        .parse()
        .expect("valid uuid");
    let session = h
        .multipart_store
        .get_multipart_upload(upload_id)
        .await
        .expect("query session")
        .expect("session exists");
    assert!(
        session.auto_bind,
        "bind:auto (or absent) must record auto_bind=true on the session"
    );

    // The file row exists but carries no bound content yet — only `complete`
    // binds it, matching the merged create+plan flow's whole point (no
    // orphan single-part pending version).
    let (file, _meta) = h
        .svc
        .get_file_with_metadata(&h.subject, file_id)
        .await
        .expect("file exists");
    assert_eq!(
        file.content_id, None,
        "create_file_bare must not pre-bind any content"
    );
}

#[tokio::test]
async fn create_file_multipart_intent_bind_manual_records_auto_bind_false() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    let body = create_body(
        owner_id,
        &json!({
            "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE },
            "bind": "manual",
        }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json_body = body_json(resp).await;
    let upload_id: Uuid = json_body["multipart"]["upload_id"]
        .as_str()
        .expect("upload_id string")
        .parse()
        .expect("valid uuid");

    let session = h
        .multipart_store
        .get_multipart_upload(upload_id)
        .await
        .expect("query session")
        .expect("session exists");
    assert!(
        !session.auto_bind,
        "bind:manual must record auto_bind=false, keeping the staged \
         explicit-bind behaviour"
    );
}

#[tokio::test]
async fn create_file_bind_invalid_value_returns_400_before_touching_multipart() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    let body = create_body(
        owner_id,
        &json!({
            "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE },
            "bind": "sometimes",
        }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json_body = body_json(resp).await;
    assert_eq!(json_body["context"]["field_violations"][0]["field"], "bind");
    assert_eq!(
        json_body["context"]["field_violations"][0]["reason"],
        "VALIDATION"
    );

    // Bind validation runs before any file row is created — no orphan left
    // behind by the rejected request.
    let files = h
        .svc
        .list_files(
            &h.subject,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id,
            },
            None,
            0,
        )
        .await
        .expect("list_files");
    assert!(
        files.is_empty(),
        "a rejected create must not have created a file row"
    );
}

// -- create_file: multipart intent vs idempotency_key ------------------------

#[tokio::test]
async fn create_file_multipart_with_idempotency_key_returns_400() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    let body = create_body(
        owner_id,
        &json!({
            "multipart": { "declared_size": 10 },
            "idempotency_key": "retry-1",
        }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json_body = body_json(resp).await;
    assert_eq!(
        json_body["context"]["field_violations"][0]["field"],
        "idempotency_key"
    );
    assert_eq!(
        json_body["context"]["field_violations"][0]["reason"],
        "VALIDATION"
    );

    let files = h
        .svc
        .list_files(
            &h.subject,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id,
            },
            None,
            0,
        )
        .await
        .expect("list_files");
    assert!(
        files.is_empty(),
        "the idempotency+multipart conflict must be rejected before create_file_bare runs"
    );
}

// -- create_file: single-part fallback / plain path --------------------------

#[tokio::test]
async fn create_file_multipart_intent_single_part_plan_falls_back_to_upload_url() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    // A tiny declared_size collapses to exactly one part at the default min
    // part size, so the ">= 2 parts" gate must NOT take the multipart
    // branch — this is the documented single-part fallback.
    let body = create_body(
        owner_id,
        &json!({ "multipart": { "declared_size": 100_u64 } }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json_body = body_json(resp).await;
    assert!(
        json_body.get("multipart").is_none(),
        "a one-part plan must fall through to the single-part path, got: {json_body}"
    );
    assert!(
        json_body["upload_url"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the single-part fallback must carry a signed upload_url"
    );
}

#[tokio::test]
async fn create_file_without_multipart_block_returns_single_part_upload_url() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    let body = create_body(owner_id, &json!({}));
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json_body = body_json(resp).await;
    assert!(
        json_body.get("multipart").is_none(),
        "the ordinary create path must never carry a multipart plan"
    );
    assert!(
        json_body["upload_url"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the ordinary create path must carry a single-part upload_url"
    );
    assert!(json_body["version_id"].is_string());
}

// -- create_file: initiate failure compensates the bare file -----------------

#[tokio::test]
async fn create_file_multipart_intent_initiate_failure_compensates_bare_file() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let router = create_file_router(&h);

    // `preferred_part_size: 1` plans fine at the handler's own compute_plan
    // call (it clamps up to the default min part size, still >= 2 parts for
    // this declared_size) but fails `MultipartService::initiate_multipart_
    // upload`'s explicit range check (must be within
    // [DEFAULT_MIN_PART_SIZE, MAX_PART_SIZE]) -- an easy, deterministic way
    // to force the initiate call to fail after the bare file already exists.
    let body = create_body(
        owner_id,
        &json!({
            "multipart": {
                "declared_size": MULTI_PART_DECLARED_SIZE,
                "preferred_part_size": 1_u64,
            },
        }),
    );
    let resp = router
        .oneshot(post_req(format!("{BASE}/files"), &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json_body = body_json(resp).await;
    assert_eq!(
        json_body["context"]["field_violations"][0]["field"],
        "preferred_part_size"
    );

    // The compensation must have deleted the orphan bare file created just
    // before the failed initiate call -- without it, this owner would be
    // left with a permanent, version-less file row.
    let files = h
        .svc
        .list_files(
            &h.subject,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id,
            },
            None,
            0,
        )
        .await
        .expect("list_files");
    assert!(
        files.is_empty(),
        "compensate_failed_multipart_initiate must remove the orphan bare file"
    );
}

// -- presign_version: single-part-only response shape ------------------------

#[tokio::test]
async fn presign_version_returns_upload_url_without_multipart_field() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let ticket = h
        .svc
        .create_file(
            &h.subject,
            NewFile {
                owner_kind: OwnerKind::User,
                owner_id,
                name: "doc.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                mime_type: "text/plain".to_owned(),
                custom_metadata: vec![],
            },
            None,
            false,
        )
        .await
        .expect("create_file");

    let router = presign_router(&h);
    let uri = format!("{BASE}/files/{}/versions", ticket.file_id);
    let resp = router
        .oneshot(post_req(uri, &json!({})))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let json_body = body_json(resp).await;
    assert_eq!(json_body["file_id"], ticket.file_id.to_string());
    assert_ne!(
        json_body["version_id"],
        ticket.version_id.to_string(),
        "presign_version must mint a NEW version, not replay the first one"
    );
    assert!(
        json_body["upload_url"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "presign_version must always carry a single-part upload_url"
    );
    assert!(
        json_body.get("multipart").is_none(),
        "presign_version never returns a multipart plan"
    );
}

// -- initiate_multipart (standalone): never auto-binds -----------------------

#[tokio::test]
async fn initiate_multipart_standalone_never_sets_auto_bind() {
    let h = build_harness().await;
    let owner_id = Uuid::now_v7();
    let file_id = h
        .svc
        .create_file_bare(
            &h.subject,
            NewFile {
                owner_kind: OwnerKind::User,
                owner_id,
                name: "doc.bin".to_owned(),
                gts_file_type: GTS.to_owned(),
                mime_type: "application/octet-stream".to_owned(),
                custom_metadata: vec![],
            },
        )
        .await
        .expect("create_file_bare");

    let router = initiate_router(&h);
    let uri = format!("{BASE}/files/{file_id}/multipart");
    let body = json!({
        "declared_mime": "application/octet-stream",
        "declared_size": MULTI_PART_DECLARED_SIZE,
    });
    let resp = router
        .oneshot(post_req(uri, &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let json_body = body_json(resp).await;
    let upload_id: Uuid = json_body["upload_id"]
        .as_str()
        .expect("upload_id string")
        .parse()
        .expect("valid uuid");

    // The standalone route must ALWAYS pass auto_bind = false, regardless of
    // any client input -- unlike the merged POST /files intent path, this
    // session's `complete` must never bind on its own; the caller binds
    // manually via a separate `POST /files/{id}/bind`.
    let session = h
        .multipart_store
        .get_multipart_upload(upload_id)
        .await
        .expect("query session")
        .expect("session exists");
    assert!(
        !session.auto_bind,
        "standalone initiate_multipart must never record auto_bind=true"
    );
}
