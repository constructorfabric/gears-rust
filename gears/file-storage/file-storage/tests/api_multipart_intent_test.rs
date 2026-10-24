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
use file_storage_sdk::{OwnerFilter, OwnerKind};

mod common;
use common::{GTS, StackBuilder, body_json, ctx};

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
    let stack = StackBuilder::new("multipart-intent").build().await;
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
/// 3-byte remainder), (see `multipart_test.rs`).
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

/// Whether the `fs-token` in `upload_url` carries the `bind_on_finalize` claim.
fn bind_claim(h: &Harness, upload_url: &str) -> bool {
    let token = &upload_url[upload_url.find("fs-token=").expect("fs-token") + "fs-token=".len()..];
    h.svc
        .verifier()
        .verify(token, time::OffsetDateTime::now_utc())
        .expect("token verifies")
        .bind_on_finalize
}

/// `bind` absent defaults to auto, `"manual"` opts out: a multipart intent records it as the
/// session's `auto_bind`, a single-part create as the upload token's `bind_on_finalize` claim.
#[tokio::test]
async fn create_file_bind_mode_controls_session_auto_bind_and_bind_claim() {
    let h = build_harness().await;
    for (bind, expected) in [(None, true), (Some("auto"), true), (Some("manual"), false)] {
        let mut extra = json!({});
        if let Some(bind) = bind {
            extra["bind"] = json!(bind);
        }

        let resp = post_create(&h, Uuid::now_v7(), &extra).await;
        assert_eq!(resp.status(), StatusCode::CREATED, "bind={bind:?}");
        let upload_url = body_json(resp).await["upload_url"]
            .as_str()
            .expect("upload_url")
            .to_owned();
        assert_eq!(
            bind_claim(&h, &upload_url),
            expected,
            "bind={bind:?}: claim"
        );

        extra["multipart"] = json!({ "declared_size": MULTI_PART_DECLARED_SIZE });
        let resp = post_create(&h, Uuid::now_v7(), &extra).await;
        assert_eq!(resp.status(), StatusCode::CREATED, "bind={bind:?}");
        let upload_id: Uuid = body_json(resp).await["multipart"]["upload_id"]
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
        assert_eq!(session.auto_bind, expected, "bind={bind:?}: auto_bind");
    }
}

/// Rejected creates answer 400 with the offending field and leave no file row behind (the bind
/// and idempotency checks run before the bare file is created; a failed initiate is compensated).
#[tokio::test]
async fn create_file_rejections_return_400_and_leave_no_file() {
    // (row, extra body, field violated)
    let rows = [
        (
            "invalid bind with multipart intent",
            json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE }, "bind": "sometimes" }),
            "bind",
        ),
        (
            // The single-part path parses `bind` too.
            "invalid bind without multipart intent",
            json!({ "bind": "sometimes" }),
            "bind",
        ),
        (
            "multipart with idempotency key",
            json!({ "multipart": { "declared_size": 10 }, "idempotency_key": "retry-1" }),
            "idempotency_key",
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
        ),
    ];
    for (row, extra, field) in rows {
        let h = build_harness().await;
        let owner_id = Uuid::now_v7();
        let resp = post_create(&h, owner_id, &extra).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{row}");

        let json_body = body_json(resp).await;
        let violation = &json_body["context"]["field_violations"][0];
        assert_eq!(violation["field"], field, "{row}");
        assert_no_files(
            &h,
            owner_id,
            &format!("{row}: a rejected create left a file row"),
        )
        .await;
    }
}
