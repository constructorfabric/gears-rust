//! REST layer of the upload flow through real handlers: `POST /files` (multipart intent, bind modes,
//! rejections), version presign, multipart initiate/abort, the `finalize_version` bind headers and
//! the `complete_multipart` responses.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Extension, Json, Router};
use bytes::Bytes;
use serde_json::{Value, json};
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers::{self, FinalizeAuth, FinalizeUploadReq};
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
use file_storage::infra::content::hash;
use file_storage::infra::external_clients::{QuotaClient, QuotaDecision};
use file_storage_sdk::{OwnerFilter, OwnerKind};

mod common;
use common::{
    FakeUsageReporter, GTS, MULTI_PART_DECLARED_SIZE, Stack, StackBuilder, backend_path, body_json,
    ctx, new_file, put_claims, put_part, token_of,
};

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
    stack: Stack,
    usage: Arc<FakeUsageReporter>,
    subject: SecurityContext,
}

async fn build_harness() -> Harness {
    build_harness_with(StackBuilder::new("multipart-intent")).await
}

async fn build_harness_with(builder: StackBuilder) -> Harness {
    let usage = FakeUsageReporter::new();
    let stack = builder.usage_reporter(usage.clone()).build().await;
    Harness {
        stack,
        usage,
        subject: ctx(Uuid::now_v7()),
    }
}

fn create_file_router(h: &Harness) -> Router {
    Router::new()
        .route(&format!("{BASE}/files"), post(handlers::create_file))
        .layer(axum::Extension(h.subject.clone()))
        .layer(axum::Extension(Arc::clone(&h.stack.svc)))
        .layer(axum::Extension(Arc::clone(&h.stack.msvc)))
}

fn presign_router(h: &Harness) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/versions"),
            post(handlers::presign_version),
        )
        .layer(axum::Extension(h.subject.clone()))
        .layer(axum::Extension(Arc::clone(&h.stack.svc)))
}

fn initiate_router(h: &Harness) -> Router {
    Router::new()
        .route(
            &format!("{BASE}/files/{{id}}/multipart"),
            post(handlers::initiate_multipart),
        )
        .layer(axum::Extension(h.subject.clone()))
        .layer(axum::Extension(Arc::clone(&h.stack.msvc)))
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

async fn assert_no_files(h: &Harness, owner_id: Uuid, msg: &str) {
    let files = h
        .stack
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

#[tokio::test]
async fn create_file_multipart_intent_bind_auto_plans_multipart_response() {
    let h = build_harness().await;
    let resp = post_create(
        &h,
        Uuid::now_v7(),
        &json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE }, "bind": "auto" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let location = resp
        .headers()
        .get(header::LOCATION)
        .expect("Location header on 201");
    let location = location.to_str().expect("valid header").to_owned();

    let body = body_json(resp).await;
    let (file_id, _, _) = created_ids(&body);
    assert!(
        location.ends_with(&file_id.to_string()),
        "Location: {location}"
    );
    assert!(
        body.get("upload_url").is_none(),
        "no upload_url on a multipart-plan response: {body}"
    );
    let plan = &body["multipart"];
    assert_eq!(plan["parts"].as_array().expect("parts array").len(), 3);
    assert_eq!(body["version_id"], plan["version_id"]);

    let (file, _meta) = h
        .stack
        .svc
        .get_file_with_metadata(&h.subject, file_id)
        .await
        .unwrap();
    assert_eq!(
        file.content_id, None,
        "a composite create must not pre-bind any content"
    );
}

/// Whether the `fs-token` in `upload_url` carries the `bind_on_finalize` claim.
fn bind_claim(h: &Harness, upload_url: &str) -> bool {
    h.stack
        .svc
        .verifier()
        .verify(token_of(upload_url), time::OffsetDateTime::now_utc())
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
        let body = body_json(resp).await;
        let upload_url = body["upload_url"].as_str().expect("upload_url");
        assert_eq!(bind_claim(&h, upload_url), expected, "bind={bind:?}: claim");

        extra["multipart"] = json!({ "declared_size": MULTI_PART_DECLARED_SIZE });
        let resp = post_create(&h, Uuid::now_v7(), &extra).await;
        assert_eq!(resp.status(), StatusCode::CREATED, "bind={bind:?}");
        let (_, upload_id, _) = created_ids(&body_json(resp).await);
        let session = h
            .stack
            .multipart_store
            .get_multipart_upload(upload_id)
            .await;
        assert_eq!(
            session.unwrap().unwrap().auto_bind,
            expected,
            "bind={bind:?}: auto_bind"
        );
    }
}

/// `bind` is a closed enum: an unknown value is rejected at deserialization with 422 (with or
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
            "out-of-range part size",
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
        let body = body_json(resp).await;
        assert_eq!(
            body["context"]["field_violations"][0]["field"], field,
            "{row}"
        );

        assert_no_files(&h, owner_id, row).await;
        for table in ["files", "file_versions", "events_outbox", "audit_outbox"] {
            assert_eq!(
                common::raw_count(&h.stack.dsn, table).await,
                0,
                "{row}: wrote {table}"
            );
        }
        assert!(h.usage.reports(0).await.is_empty(), "{row}: reported usage");
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
    let events = h
        .stack
        .store
        .list_file_events(file_id)
        .await
        .expect("events");
    events.into_iter().map(|e| e.event_type).collect()
}

async fn file_count_deltas(h: &Harness) -> Vec<i64> {
    h.usage
        .reports(2)
        .await
        .iter()
        .map(|d| d.file_count_delta)
        .collect()
}

/// A composite create creates the file and its pending version together, so aborting the session
/// leaves nothing behind: the file is deleted (`file.deleted`, usage `-1`).
#[tokio::test]
async fn composite_create_then_abort_deletes_the_file() {
    let h = build_harness().await;
    let extra = json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } });
    let resp = post_create(&h, Uuid::now_v7(), &extra).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let (file_id, upload_id, version_id) = created_ids(&body_json(resp).await);
    assert!(
        h.stack
            .store
            .get_version(file_id, version_id)
            .await
            .unwrap()
            .is_some()
    );

    h.stack
        .msvc
        .abort_multipart_upload(&h.subject, file_id, upload_id)
        .await
        .expect("abort");

    let err = h.stack.svc.get_file(&h.subject, file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::FileNotFound { .. }),
        "got {err:?}"
    );
    assert_eq!(
        event_types(&h, file_id).await,
        ["file.created", "file.deleted"]
    );
    assert_eq!(file_count_deltas(&h).await, [1, -1]);
}

/// A separate `POST /files/{id}/multipart` on a file that already has an available bound version:
/// aborting the session removes only the session's version; the file and its version stay.
#[tokio::test]
async fn standalone_multipart_abort_keeps_file_with_other_versions() {
    let h = build_harness().await;
    let Stack {
        svc,
        msvc,
        store,
        dp,
        ..
    } = &h.stack;
    let bound = common::bound_file(svc, dp, &h.subject, b"AAAAA").await;
    let second = common::initiate(msvc, &h.subject, bound.file_id, 5).await;

    msvc.abort_multipart_upload(&h.subject, bound.file_id, second.upload_id)
        .await
        .expect("abort");

    svc.get_file(&h.subject, bound.file_id)
        .await
        .expect("the file must survive the abort");
    assert!(
        store
            .get_version(bound.file_id, bound.version_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_version(bound.file_id, second.version_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !event_types(&h, bound.file_id)
            .await
            .contains(&"file.deleted".to_owned())
    );
}

/// A failure after the create transaction (here: the session row cannot be written) removes the
/// pending version and the file again.
#[tokio::test]
async fn composite_create_session_failure_leaves_no_file_and_no_version() {
    let h = build_harness().await;
    common::raw_exec(
        &h.stack.dsn,
        "ALTER TABLE multipart_uploads RENAME TO multipart_uploads_gone",
    )
    .await;

    let owner_id = Uuid::now_v7();
    let extra = json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } });
    let resp = post_create(&h, owner_id, &extra).await;
    assert!(resp.status().is_server_error(), "got {}", resp.status());
    assert_no_files(&h, owner_id, "a failed composite create left a file").await;
    assert_eq!(common::raw_count(&h.stack.dsn, "file_versions").await, 0);
    assert_eq!(file_count_deltas(&h).await, [1, -1]);
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
    let extra = json!({ "multipart": { "declared_size": MULTI_PART_DECLARED_SIZE } });
    let resp = post_create(&h, Uuid::now_v7(), &extra).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        *quota.requested.lock().expect("lock"),
        vec![MULTI_PART_DECLARED_SIZE]
    );
}

/// A tiny `declared_size` collapses to one part at the default min part size, so the create takes
/// the documented single-part fallback: a signed `upload_url` and no multipart plan.
#[tokio::test]
async fn create_file_one_part_multipart_intent_falls_back_to_upload_url() {
    let h = build_harness().await;
    let resp = post_create(
        &h,
        Uuid::now_v7(),
        &json!({ "multipart": { "declared_size": 100_u64 } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert!(
        body.get("multipart").is_none(),
        "must carry no multipart plan: {body}"
    );
    assert!(
        body["upload_url"].as_str().is_some_and(|s| !s.is_empty()),
        "signed upload_url"
    );
}

/// `presign_version` on an auto-bind file mints a new version whose token never auto-binds.
#[tokio::test]
async fn presign_version_mints_new_version_without_bind_claim() {
    let h = build_harness().await;
    let ticket = h
        .stack
        .svc
        .create_file(&h.subject, new_file("doc.bin"), None, true)
        .await
        .unwrap();
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

    let body = body_json(resp).await;
    assert_eq!(body["file_id"], ticket.file_id.to_string());
    assert_ne!(
        body["version_id"],
        ticket.version_id.to_string(),
        "must mint a NEW version"
    );
    let upload_url = body["upload_url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("upload_url");
    assert!(
        !bind_claim(&h, upload_url),
        "presign_version must never auto-bind"
    );
}

/// A standalone `POST /files/{id}/multipart` never records `auto_bind`.
#[tokio::test]
async fn initiate_multipart_standalone_never_sets_auto_bind() {
    let h = build_harness().await;
    let file_id = common::bare_file(&h.stack.svc, &h.subject).await;
    let uri = format!("{BASE}/files/{file_id}/multipart");
    let body = json!({
        "declared_mime": "application/octet-stream",
        "declared_size": MULTI_PART_DECLARED_SIZE,
    });
    let resp = initiate_router(&h)
        .oneshot(post_req(uri, &body))
        .await
        .expect("dispatch");
    assert_eq!(resp.status(), StatusCode::OK);

    let upload_id: Uuid = body_json(resp).await["upload_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let session = h
        .stack
        .multipart_store
        .get_multipart_upload(upload_id)
        .await;
    assert!(!session.unwrap().unwrap().auto_bind);
}

/// Drives the real `finalize_version` handler with a signed token (carrying `bind_on_finalize` iff
/// `auto_bind`) reporting `bytes`, which it first stores at the version's path.
async fn finalize_via_handler(
    s: &Stack,
    file_id: Uuid,
    version_id: Uuid,
    bytes: &'static [u8],
    auto_bind: bool,
) -> axum::response::Response {
    let path = backend_path(file_id, version_id);
    s.backend
        .put(&path, Bytes::from_static(bytes))
        .await
        .unwrap();
    let claims = put_claims(file_id, version_id, path, auto_bind);
    let token = s
        .issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");
    let mut headers = HeaderMap::new();
    headers.insert("x-fs-token", token.parse().expect("valid header value"));
    headers.insert(
        "x-fs-internal-token",
        "test-internal-secret".parse().unwrap(),
    );
    let req = FinalizeUploadReq {
        size: i64::try_from(bytes.len()).unwrap(),
        hash_hex: hex::encode(hash::sha256(bytes)),
    };
    handlers::finalize_version(
        Extension(Arc::clone(&s.svc)),
        Extension(Arc::new(s.svc.verifier())),
        Extension(Arc::new(FinalizeAuth::new(
            "test-internal-secret".to_owned(),
            time::Duration::ZERO,
        ))),
        Path((file_id, version_id)),
        headers,
        Json(req),
    )
    .await
    .expect("the finalize handler itself succeeds (only the embedded bind CAS may be lost)")
    .into_response()
}

fn header_str<'a>(resp: &'a axum::response::Response, name: &str) -> Option<&'a str> {
    resp.headers().get(name).map(|v| v.to_str().unwrap())
}

/// A `bind_on_finalize` finalize sets `x-fs-bound: bound` and the new `ETag`; a second pending
/// version of the same file loses the `content_id IS NULL` CAS, still finalizes, and answers
/// `x-fs-bound: conflict` + `x-fs-current-etag` of the winner (no `ETag`); a manual finalize
/// answers `x-fs-bound: manual` with neither.
#[tokio::test]
async fn finalize_version_reports_bound_conflict_and_manual_headers() {
    let s = StackBuilder::new("complete-bind").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = s
        .svc
        .create_file(&ctx, new_file("bind.bin"), None, true)
        .await
        .unwrap();

    let resp = finalize_via_handler(&s, t.file_id, t.version_id, b"auto-bind me", true).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(header_str(&resp, "x-fs-bound"), Some("bound"));
    let file = common::content_id_of(&s.store, t.file_id).await;
    assert_eq!(file, Some(t.version_id));
    let winner_etag = file_storage::domain::etag::content_etag(t.file_id, t.version_id);
    assert_eq!(header_str(&resp, "etag"), Some(winner_etag.as_str()));

    let t2 = s.svc.presign_version(&ctx, t.file_id).await.unwrap();
    let resp = finalize_via_handler(&s, t.file_id, t2.version_id, b"loser!", true).await;
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "a lost bind does not reject the finalize"
    );
    assert_eq!(header_str(&resp, "x-fs-bound"), Some("conflict"));
    assert_eq!(
        header_str(&resp, "x-fs-current-etag"),
        Some(winner_etag.as_str())
    );
    assert_eq!(header_str(&resp, "etag"), None);

    let t3 = s.svc.presign_version(&ctx, t.file_id).await.unwrap();
    let resp = finalize_via_handler(&s, t.file_id, t3.version_id, b"manual", false).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(header_str(&resp, "x-fs-bound"), Some("manual"));
    assert_eq!(header_str(&resp, "etag"), None);
    assert_eq!(header_str(&resp, "x-fs-current-etag"), None);
}

/// A 2-part auto-bind upload completed through the handler; each `MultipartCompleteDto` field is
/// cross-checked against the persisted rows rather than hardcoded hash bytes.
#[tokio::test]
async fn complete_multipart_completed_returns_200_with_full_dto() {
    let Stack {
        svc,
        msvc,
        multipart_store,
        backend,
        store,
        ..
    } = StackBuilder::new("complete-bind").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = common::bare_file(&svc, &ctx).await;
    let part_size = DEFAULT_MIN_PART_SIZE;
    let declared_size = part_size + 5;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            Some(part_size),
            true,
        )
        .await
        .unwrap();
    let first = Bytes::from(vec![0u8; usize::try_from(part_size).unwrap()]);
    for (n, data) in [(1, first), (2, Bytes::from_static(b"AAAAA"))] {
        put_part(&multipart_store, &backend, &plan, file_id, n, data).await;
    }

    let resp = handlers::complete_multipart(
        Extension(ctx.clone()),
        Extension(Arc::clone(&msvc)),
        Path((file_id, plan.upload_id)),
        HeaderMap::new(),
    )
    .await
    .expect("complete_multipart must succeed");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    let manifest = store.get_version_manifest(plan.version_id).await.unwrap();
    assert_eq!(body["version_id"], plan.version_id.to_string());
    assert_eq!(body["size"], i64::try_from(declared_size).unwrap());
    assert_eq!(body["hash_algorithm"], "SHA-256");
    let content_hash = hex::decode(body["content_hash"].as_str().unwrap()).unwrap();
    assert_eq!(
        content_hash, version.hash_value,
        "hex of the persisted hash_value"
    );
    assert_eq!(body["hash_mode"], "multipart-composite-sha256");
    assert_eq!(body["part_count"], 2);
    assert_eq!(
        body["manifest"],
        manifest.expect("a 2-part completion persists a manifest")
    );
    assert_eq!(body["bind_state"], "bound");
    assert_eq!(
        body["etag"],
        file_storage::domain::etag::content_etag(file_id, plan.version_id)
    );
    assert!(
        body.get("current_etag").is_none(),
        "omitted on a successful bind: {body:?}"
    );
    assert_eq!(
        common::content_id_of(&store, file_id).await,
        Some(plan.version_id)
    );
}

/// `complete` against a LIVE completion lease answers 202 with `retry_after_secs` and a matching
/// `Retry-After` header.
#[tokio::test]
async fn complete_multipart_while_lease_held_returns_202_with_matching_retry_after() {
    let Stack {
        svc,
        msvc,
        multipart_store,
        ..
    } = StackBuilder::new("complete-bind").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = common::bare_file(&svc, &ctx).await;
    let plan = common::initiate(&msvc, &ctx, file_id, 5).await;
    common::plant_lease(&multipart_store, plan.upload_id, "other-completer", 120).await;

    let resp = handlers::complete_multipart(
        Extension(ctx.clone()),
        Extension(Arc::clone(&msvc)),
        Path((file_id, plan.upload_id)),
        HeaderMap::new(),
    )
    .await
    .expect("a live competing lease must answer Completing, not an error");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After on a 202");
    let retry_after = retry_after.to_str().unwrap().to_owned();

    let body = body_json(resp).await;
    assert_eq!(body["state"], "completing");
    let secs = body["retry_after_secs"]
        .as_u64()
        .expect("retry_after_secs is a number");
    assert!(secs > 0);
    assert_eq!(
        retry_after,
        secs.to_string(),
        "Retry-After mirrors the body"
    );
}
