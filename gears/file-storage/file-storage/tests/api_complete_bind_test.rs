//! Handler-level tests for `finalize_version`'s auto-bind header mapping. Calls the handler
//! directly, without an `axum::Router`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use bytes::Bytes;
use toolkit::api::rest::extract::Path;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage::api::rest::handlers::{self, FinalizeAuth, FinalizeUploadReq};
use file_storage::domain::etag::etag_for;
use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
use file_storage::infra::content::hash;
use file_storage::infra::signed_url::Issuer;

mod common;
use common::{Stack, backend_path, body_json, ctx, new_file, put_claims, put_part};

async fn stack(issuer: &Arc<Issuer>) -> Stack {
    common::build_stack("complete-bind", Arc::clone(issuer), None).await
}

/// Signed `bind_on_finalize` token + internal-secret headers and the matching finalize body for
/// `bytes` stored at `path`.
fn bind_finalize_inputs(
    issuer: &Issuer,
    file_id: Uuid,
    version_id: Uuid,
    path: &str,
    bytes: &Bytes,
) -> (HeaderMap, FinalizeUploadReq) {
    let claims = put_claims(file_id, version_id, path.to_owned(), true);
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");
    let mut headers = HeaderMap::new();
    headers.insert("x-fs-token", token.parse().expect("valid header value"));
    headers.insert(
        "x-fs-internal-token",
        "test-internal-secret".parse().expect("valid header value"),
    );
    let req = FinalizeUploadReq {
        size: i64::try_from(bytes.len()).unwrap(),
        hash_hex: hex::encode(hash::sha256(bytes)),
    };
    (headers, req)
}

/// Drives the real `finalize_version` handler for `(file_id, version_id)` with a signed
/// `bind_on_finalize` token reporting `bytes` stored at `path`.
async fn finalize_via_handler(
    svc: &Arc<file_storage::domain::service::FileService>,
    issuer: &Issuer,
    file_id: Uuid,
    version_id: Uuid,
    path: &str,
    bytes: &Bytes,
) -> axum::response::Response {
    let (headers, req) = bind_finalize_inputs(issuer, file_id, version_id, path, bytes);
    handlers::finalize_version(
        Extension(Arc::clone(svc)),
        Extension(Arc::new(svc.verifier())),
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

/// A `bind_on_finalize` finalize through the real handler sets `x-fs-bound: true` and the new
/// `ETag`; a second pending version of the same file then loses the `content_id IS NULL` CAS, still
/// finalizes, and the handler answers `x-fs-bound: conflict` + `x-fs-current-etag` of the winner.
#[tokio::test]
async fn finalize_version_bind_claim_reports_bound_headers_then_conflict_headers() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let Stack {
        svc,
        backend,
        store,
        ..
    } = stack(&issuer).await;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file("complete-bind.bin"), None, true)
        .await
        .unwrap();
    let bytes = Bytes::from_static(b"auto-bind me");
    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, bytes.clone()).await.unwrap();

    let resp = finalize_via_handler(
        &svc,
        &issuer,
        ticket.file_id,
        ticket.version_id,
        &path,
        &bytes,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers().get("x-fs-bound").expect("x-fs-bound header"),
        "true"
    );
    let file = store
        .get_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap()
        .expect("file");
    let winner_etag = etag_for(&file).expect("bound file must have a content etag");
    assert_eq!(
        resp.headers()
            .get(header::ETAG)
            .expect("ETag header")
            .to_str()
            .unwrap(),
        winner_etag,
        "ETag header must be the file's new content etag"
    );

    let ticket2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    let loser_bytes = Bytes::from_static(b"loser!");
    let loser_path = backend_path(ticket.file_id, ticket2.version_id);
    backend.put(&loser_path, loser_bytes.clone()).await.unwrap();

    let resp = finalize_via_handler(
        &svc,
        &issuer,
        ticket.file_id,
        ticket2.version_id,
        &loser_path,
        &loser_bytes,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "the upload/finalize is not rejected merely because the bind lost"
    );
    assert_eq!(
        resp.headers().get("x-fs-bound").expect("x-fs-bound header"),
        "conflict"
    );
    assert_eq!(
        resp.headers()
            .get("x-fs-current-etag")
            .expect("x-fs-current-etag header")
            .to_str()
            .unwrap(),
        winner_etag,
        "x-fs-current-etag must carry the CURRENT (winner's) etag for a manual rebind's If-Match"
    );
    assert!(
        resp.headers().get(header::ETAG).is_none(),
        "a conflict response must not also carry an ETag header"
    );
}

/// A 2-part auto-bind upload completed through the handler; each `MultipartCompleteDto` field is
/// cross-checked against the persisted rows rather than hardcoded hash bytes.
#[tokio::test]
async fn complete_multipart_completed_returns_200_with_full_dto() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let Stack {
        svc,
        msvc,
        multipart_store,
        backend,
        store,
        ..
    } = stack(&issuer).await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = svc
        .create_file_bare(&ctx, new_file("complete-bind.bin"))
        .await
        .unwrap();

    // 5 MiB + 5 bytes at a 5 MiB part size -> exactly 2 parts, so composite hash mode/manifest are
    // set.
    let part_size = DEFAULT_MIN_PART_SIZE;
    let declared_size = part_size + 5;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            Some(part_size),
            true, // auto_bind
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 2, "plan must have exactly 2 parts");

    for (n, data) in [
        (
            1,
            Bytes::from(vec![0u8; usize::try_from(part_size).unwrap()]),
        ),
        (2, Bytes::from_static(b"AAAAA")),
    ] {
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
        .expect("version row must exist");
    let file = store
        .get_file(&AccessScope::allow_all(), file_id)
        .await
        .unwrap()
        .expect("file row must exist");
    let manifest = store
        .get_version_manifest(plan.version_id)
        .await
        .unwrap()
        .expect("a 2-part completion must persist a manifest row");

    assert_eq!(body["version_id"], plan.version_id.to_string());
    assert_eq!(body["size"], i64::try_from(declared_size).unwrap());
    assert_eq!(body["hash_algorithm"], "SHA-256");
    assert_eq!(
        hex::decode(
            body["content_hash"]
                .as_str()
                .expect("content_hash is a string")
        )
        .unwrap(),
        version.hash_value,
        "content_hash must be the hex encoding of the persisted hash_value"
    );
    assert_eq!(body["hash_mode"], "multipart-composite-sha256");
    assert_eq!(body["part_count"], 2);
    assert_eq!(body["manifest"], manifest);
    assert_eq!(body["bind_state"], "bound");
    assert_eq!(
        body["etag"],
        etag_for(&file).expect("bound file must have a content etag")
    );
    assert!(
        body.get("current_etag").is_none(),
        "current_etag must be omitted (null) on a successful bind, got {body:?}"
    );

    assert_eq!(file.content_id, Some(plan.version_id));
}

/// `complete` racing a LIVE completion lease answers 202 with `retry_after_secs` and a matching
/// `Retry-After` header. The lease is taken directly via the store, so there is no race or sleep.
#[tokio::test]
async fn complete_multipart_while_lease_held_returns_202_with_matching_retry_after() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let Stack {
        svc,
        msvc,
        multipart_store,
        ..
    } = stack(&issuer).await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = svc
        .create_file_bare(&ctx, new_file("complete-bind.bin"))
        .await
        .unwrap();

    let plan = msvc
        .initiate_multipart_upload(&ctx, file_id, "application/octet-stream", 5, None, false)
        .await
        .unwrap();

    let now = time::OffsetDateTime::now_utc();
    let acquired = multipart_store
        .acquire_multipart_complete_lease(
            plan.upload_id,
            "other-completer",
            now + time::Duration::seconds(120),
            now,
        )
        .await
        .unwrap();
    assert!(
        acquired,
        "test setup: the other completer must win the lease"
    );

    let resp = handlers::complete_multipart(
        Extension(ctx.clone()),
        Extension(Arc::clone(&msvc)),
        Path((file_id, plan.upload_id)),
        HeaderMap::new(),
    )
    .await
    .expect("a live competing lease must answer Completing, not an error");
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let retry_after_header = resp
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After header must be set on a 202")
        .to_str()
        .expect("Retry-After is ASCII")
        .to_owned();

    let body = body_json(resp).await;
    assert_eq!(body["state"], "completing");
    let retry_after_secs = body["retry_after_secs"]
        .as_u64()
        .expect("retry_after_secs is a number");
    assert!(retry_after_secs > 0);
    assert_eq!(
        retry_after_header,
        retry_after_secs.to_string(),
        "Retry-After header must mirror the body's retry_after_secs"
    );
}
