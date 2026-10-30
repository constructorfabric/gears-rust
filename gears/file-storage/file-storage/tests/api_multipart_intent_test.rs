#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::FileService;
use file_storage::infra::external_clients::{
    QuotaClient, QuotaDecision, UsageDelta, UsageReporter,
};
use file_storage::infra::storage::Store;
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

/// Capturing `UsageReporter`.
#[derive(Default)]
struct FakeUsage {
    deltas: std::sync::Mutex<Vec<UsageDelta>>,
}

#[async_trait]
impl UsageReporter for FakeUsage {
    async fn report(&self, delta: UsageDelta) {
        self.deltas.lock().expect("lock").push(delta);
    }
}

/// Always-allowing `QuotaClient` that records every requested size.
#[derive(Default)]
struct CountingQuota {
    requested: std::sync::Mutex<Vec<u64>>,
}

#[async_trait]
impl QuotaClient for CountingQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        self.requested.lock().expect("lock").push(additional_bytes);
        Ok(QuotaDecision::Allowed)
    }
}

struct Harness {
    svc: Arc<FileService>,
    msvc: Arc<MultipartService>,
    multipart_store: Arc<dyn MultipartStore>,
    store: Store,
    backend: Arc<dyn file_storage::infra::backend::StorageBackend>,
    dsn: String,
    usage: Arc<FakeUsage>,
    subject: SecurityContext,
}

async fn build_harness() -> Harness {
    build_harness_with(StackBuilder::new("multipart-intent")).await
}

async fn build_harness_with(builder: StackBuilder) -> Harness {
    let usage = Arc::new(FakeUsage::default());
    let stack = builder
        .usage_reporter(Arc::clone(&usage) as Arc<dyn UsageReporter>)
        .build()
        .await;
    Harness {
        svc: stack.svc,
        msvc: stack.msvc,
        multipart_store: stack.multipart_store,
        store: stack.store,
        backend: stack.backend,
        dsn: stack.dsn,
        usage,
        subject: ctx(Uuid::now_v7()),
    }
}

/// Usage deltas reported so far, after giving fire-and-forget reports a moment to land.
async fn usage_deltas(h: &Harness) -> Vec<UsageDelta> {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    h.usage.deltas.lock().expect("lock").clone()
}

async fn raw_exec(dsn: &str, sql: &str) {
    let conn = Database::connect(dsn).await.expect("raw connect");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"));
}

async fn raw_count(dsn: &str, table: &str) -> i64 {
    let conn = Database::connect(dsn).await.expect("raw connect");
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        format!("SELECT COUNT(*) AS c FROM {table}"),
    ))
    .await
    .expect("count query")
    .expect("one row")
    .try_get::<i64>("", "c")
    .expect("i64 column c")
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

/// `bind` is a closed enum: an unknown value is rejected at deserialization with 422, the toolkit status for a body that does not deserialize (with or
/// without a multipart intent) and nothing is written.
#[tokio::test]
async fn create_file_unknown_bind_value_returns_422() {
    for extra in [
        json!({ "bind": "bogus" }),
        json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE }, "bind": "bogus" }),
    ] {
        let h = build_harness().await;
        let owner_id = Uuid::now_v7();
        let resp = post_create(&h, owner_id, &extra).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{extra}");
        // The bare test router has no problem+json layer; the extractor's text names the value.
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8_lossy(&bytes);
        assert!(
            body.contains("bogus"),
            "the error must name the rejected value: {body}"
        );
        assert_no_files(&h, owner_id, "a rejected create left a file row").await;
    }
}

/// Rejected creates answer 400 with the offending field and write nothing: no file, version,
/// outbox event, audit row or usage delta (every check runs before the first write).
#[tokio::test]
async fn create_file_rejections_return_400_and_leave_no_file() {
    // (row, extra body, field violated)
    let rows = [
        (
            "multipart with idempotency key",
            json!({ "multipart": { "declared_size": 10 }, "idempotency_key": "retry-1" }),
            "idempotency_key",
        ),
        (
            // The part-size hint is validated before the file is committed.
            "out-of-range part size is rejected before any write",
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
        for table in ["files", "file_versions", "events_outbox", "audit_outbox"] {
            assert_eq!(
                raw_count(&h.dsn, table).await,
                0,
                "{row}: a rejected create wrote to {table}"
            );
        }
        assert!(
            usage_deltas(&h).await.is_empty(),
            "{row}: a rejected create reported usage"
        );
    }
}

/// Parse `(file_id, upload_id, version_id)` out of a composite-create response.
fn created_ids(body: &Value) -> (Uuid, Uuid, Uuid) {
    let id = |v: &Value| -> Uuid { v.as_str().expect("string id").parse().expect("uuid") };
    (
        id(&body["file_id"]),
        id(&body["multipart"]["upload_id"]),
        id(&body["multipart"]["version_id"]),
    )
}

async fn event_types(h: &Harness, file_id: Uuid) -> Vec<String> {
    h.store
        .list_file_events(file_id)
        .await
        .expect("events")
        .into_iter()
        .map(|e| e.event_type)
        .collect()
}

/// A composite create creates the file and its pending version together, so aborting the session
/// leaves nothing behind: the file is deleted (`file.deleted`, usage `-1`).
#[tokio::test]
async fn composite_create_then_abort_deletes_the_file() {
    let h = build_harness().await;
    let resp = post_create(
        &h,
        Uuid::now_v7(),
        &json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let (file_id, upload_id, version_id) = created_ids(&body_json(resp).await);
    // File and pending version exist from the single create transaction.
    assert!(
        h.store
            .get_version(file_id, version_id)
            .await
            .unwrap()
            .is_some()
    );

    h.msvc
        .abort_multipart_upload(&h.subject, file_id, upload_id)
        .await
        .expect("abort");

    assert!(matches!(
        h.svc.get_file(&h.subject, file_id).await,
        Err(DomainError::FileNotFound { .. })
    ));
    assert_eq!(
        event_types(&h, file_id).await,
        vec!["file.created".to_owned(), "file.deleted".to_owned()]
    );
    let counts: Vec<i64> = usage_deltas(&h)
        .await
        .iter()
        .map(|d| d.file_count_delta)
        .collect();
    assert_eq!(counts, vec![1, -1]);
}

/// A separate `POST /files/{id}/multipart` on a file that already has an available bound version:
/// aborting the session removes only the session's version; the file and its version stay.
#[tokio::test]
async fn standalone_multipart_abort_keeps_file_with_other_versions() {
    let h = build_harness().await;
    let ticket = h
        .svc
        .create_file(&h.subject, common::new_file("keep.bin"), None, false)
        .await
        .expect("create");
    let file_id = ticket.file_id;

    // First session: complete with auto-bind, so the file has an available, bound version.
    let first = common::session_with_part(
        &h.msvc,
        &h.multipart_store,
        &h.backend,
        &h.subject,
        file_id,
        b"AAAAA",
        true,
    )
    .await;
    h.msvc
        .complete_multipart_upload(&h.subject, file_id, first.upload_id, None)
        .await
        .expect("complete");

    let second = h
        .msvc
        .initiate_multipart_upload(
            &h.subject,
            file_id,
            "application/octet-stream",
            5,
            None,
            false,
        )
        .await
        .expect("initiate");
    h.msvc
        .abort_multipart_upload(&h.subject, file_id, second.upload_id)
        .await
        .expect("abort");

    h.svc
        .get_file(&h.subject, file_id)
        .await
        .expect("the file must survive the abort");
    assert!(
        h.store
            .get_version(file_id, first.version_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        h.store
            .get_version(file_id, second.version_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !event_types(&h, file_id)
            .await
            .contains(&"file.deleted".to_owned())
    );
}

/// A failure after the create transaction (here: the session row cannot be written) removes the
/// pending version and the file again.
#[tokio::test]
async fn composite_create_session_failure_leaves_no_file_and_no_version() {
    let h = build_harness().await;
    raw_exec(
        &h.dsn,
        "ALTER TABLE multipart_uploads RENAME TO multipart_uploads_gone",
    )
    .await;

    let owner_id = Uuid::now_v7();
    let resp = post_create(
        &h,
        owner_id,
        &json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } }),
    )
    .await;
    assert!(
        resp.status().is_server_error(),
        "session persistence failure must surface as a server error, got {}",
        resp.status()
    );
    assert_no_files(&h, owner_id, "a failed composite create left a file").await;
    assert_eq!(raw_count(&h.dsn, "files").await, 0);
    assert_eq!(raw_count(&h.dsn, "file_versions").await, 0);
    let counts: Vec<i64> = usage_deltas(&h)
        .await
        .iter()
        .map(|d| d.file_count_delta)
        .collect();
    assert_eq!(counts, vec![1, -1]);
}

/// The quota is checked once on a composite create, for the declared size.
#[tokio::test]
async fn composite_create_checks_quota_once_for_the_declared_size() {
    let quota = Arc::new(CountingQuota::default());
    let h = build_harness_with(
        StackBuilder::new("multipart-intent-quota")
            .quota(Arc::clone(&quota) as Arc<dyn QuotaClient>),
    )
    .await;
    let resp = post_create(
        &h,
        Uuid::now_v7(),
        &json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        *quota.requested.lock().expect("lock"),
        vec![MULTI_PART_DECLARED_SIZE]
    );
}
