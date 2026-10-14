#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use serde_json::{Value, json};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::FileService;
use file_storage::infra::signed_url::Issuer;
use file_storage_sdk::{OwnerFilter, OwnerKind};

mod common;
use common::{GTS, body_json, build_stack, ctx, new_file};

const BASE: &str = "/api/file-storage/v1";

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

async fn build_harness() -> Harness {
    let stack = build_stack(
        "multipart-intent",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        None,
    )
    .await;
    Harness {
        svc: stack.svc,
        msvc: stack.msvc,
        multipart_store: stack.multipart_store,
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

/// Declared size that plans to exactly 3 parts at the default min part size (2 full parts + a
/// 3-byte remainder), matching the pattern already validated in `multipart_test.rs`.
const MULTI_PART_DECLARED_SIZE: u64 = 2 * DEFAULT_MIN_PART_SIZE + 3;

async fn assert_no_files(h: &Harness, owner_id: Uuid, msg: &str) {
    let files = h
        .svc
        .list_files(
            &h.subject,
            OwnerFilter {
                owner_kind: OwnerKind::User,
                owner_id,
            },
            None,
            None,
        )
        .await
        .expect("list_files")
        .items;
    assert!(files.is_empty(), "{msg}");
}

/// `POST /files` for `owner_id` with `extra` merged over the base body.
async fn post_create(h: &Harness, owner_id: Uuid, extra: &Value) -> axum::response::Response {
    create_file_router(h)
        .oneshot(post_req(
            format!("{BASE}/files"),
            &create_body(owner_id, extra),
        ))
        .await
        .expect("dispatch")
}

/// `POST /files` with a multi-part intent and the given `bind` (`None` = field absent); returns
/// the persisted session's `auto_bind`.
async fn created_session_auto_bind(h: &Harness, bind: Option<&str>) -> bool {
    let mut extra = json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } });
    if let Some(bind) = bind {
        extra["bind"] = json!(bind);
    }
    let resp = post_create(h, Uuid::now_v7(), &extra).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let upload_id: Uuid = body_json(resp).await["multipart"]["upload_id"]
        .as_str()
        .expect("upload_id string")
        .parse()
        .expect("valid uuid");
    h.multipart_store
        .get_multipart_upload(upload_id)
        .await
        .expect("query session")
        .expect("session exists")
        .auto_bind
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

    // Single-part `upload_url` must be entirely absent (not null) — the multipart plan is the only
    // upload path in this response.
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

    // The file row exists but carries no bound content yet — only `complete` binds it, matching the
    // merged create+plan flow's whole point (no orphan single-part pending version).
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
async fn create_file_multipart_intent_bind_mode_sets_session_auto_bind() {
    let h = build_harness().await;
    // No `bind` field at all must default to the same auto_bind=true as an explicit "auto"
    // (covered by the plan test below).
    assert!(
        created_session_auto_bind(&h, None).await,
        "an absent bind field must default to auto_bind=true, same as bind:\"auto\""
    );
    assert!(
        !created_session_auto_bind(&h, Some("manual")).await,
        "bind:manual must record auto_bind=false, keeping the staged explicit-bind behaviour"
    );
}

/// Rejected creates answer 400 with the offending field and leave no file row behind (the bind
/// and idempotency checks run before the bare file is created; a failed initiate is compensated).
#[tokio::test]
async fn create_file_rejections_return_400_and_leave_no_file() {
    // (row, extra body, field violated, expected reason if pinned)
    let rows = [
        (
            "invalid bind with multipart intent",
            json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE }, "bind": "sometimes" }),
            "bind",
            Some("VALIDATION"),
        ),
        (
            // The single-part path parses `bind` too.
            "invalid bind without multipart intent",
            json!({ "bind": "sometimes" }),
            "bind",
            Some("VALIDATION"),
        ),
        (
            "multipart with idempotency key",
            json!({ "multipart": { "declared_size": 10 }, "idempotency_key": "retry-1" }),
            "idempotency_key",
            Some("VALIDATION"),
        ),
        (
            // compensate_failed_multipart_initiate must delete the orphan bare file created just
            // before the failed initiate call.
            "initiate failure compensates the bare file",
            json!({
                "multipart": {
                    "declared_size": MULTI_PART_DECLARED_SIZE,
                    "preferred_part_size": 1_u64,
                },
            }),
            "preferred_part_size",
            None,
        ),
    ];
    for (row, extra, field, reason) in rows {
        let h = build_harness().await;
        let owner_id = Uuid::now_v7();
        let resp = post_create(&h, owner_id, &extra).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{row}");

        let json_body = body_json(resp).await;
        let violation = &json_body["context"]["field_violations"][0];
        assert_eq!(violation["field"], field, "{row}");
        if let Some(reason) = reason {
            assert_eq!(violation["reason"], reason, "{row}");
        }
        assert_no_files(
            &h,
            owner_id,
            &format!("{row}: a rejected create left a file row"),
        )
        .await;
    }
}

/// Paths that must return a single-part `upload_url` and no multipart plan: a tiny declared_size
/// collapses to exactly one part at the default min part size (the ">= 2 parts" gate must NOT take
/// the multipart branch -- the documented single-part fallback), and so does the ordinary create.
#[tokio::test]
async fn create_file_single_part_paths_return_upload_url_without_multipart_plan() {
    for (row, extra) in [
        (
            "one-part multipart intent falls back",
            json!({ "multipart": { "declared_size": 100_u64 } }),
        ),
        ("no multipart block", json!({})),
    ] {
        let h = build_harness().await;
        let resp = post_create(&h, Uuid::now_v7(), &extra).await;
        assert_eq!(resp.status(), StatusCode::CREATED, "{row}");

        let json_body = body_json(resp).await;
        assert!(
            json_body.get("multipart").is_none(),
            "{row}: must carry no multipart plan, got: {json_body}"
        );
        assert!(
            json_body["upload_url"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{row}: must carry a signed single-part upload_url"
        );
        assert!(json_body["version_id"].is_string(), "{row}");
    }
}

/// Whether the `fs-token` in `upload_url` carries the `bind_on_finalize` claim.
fn bind_claim(h: &Harness, upload_url: &str) -> bool {
    let token = &upload_url[upload_url.find("fs-token=").expect("fs-token") + "fs-token=".len()..];
    h.svc
        .verifier()
        .verify(token, time::OffsetDateTime::now_utc())
        .expect("token verifies")
        .bind_on_finalize
}

#[tokio::test]
async fn presign_version_returns_upload_url_without_multipart_field() {
    let h = build_harness().await;
    // An auto-bind file: its own create ticket carries the bind claim, a presigned version must not.
    let ticket = h
        .svc
        .create_file(&h.subject, new_file("doc.bin"), None, true)
        .await
        .expect("create_file");
    assert!(
        bind_claim(&h, &ticket.upload_url),
        "create_file(auto) carries the claim"
    );

    let uri = format!("{BASE}/files/{}/versions", ticket.file_id);
    let resp = presign_router(&h)
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
    let upload_url = json_body["upload_url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("presign_version must always carry a single-part upload_url");
    assert!(
        !bind_claim(&h, upload_url),
        "presign_version must never auto-bind"
    );
    assert!(
        json_body.get("multipart").is_none(),
        "presign_version never returns a multipart plan"
    );
}

#[tokio::test]
async fn initiate_multipart_standalone_never_sets_auto_bind() {
    let h = build_harness().await;
    let file_id = h
        .svc
        .create_file_bare(&h.subject, new_file("doc.bin"))
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
