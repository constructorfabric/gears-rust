#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use bytes::Bytes;
use toolkit::api::rest::extract::Path;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::api::rest::handlers::{
    FinalizeAuth, FinalizeUploadReq, ReportPartReq, finalize_version, report_multipart_part,
};
use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::BindState;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::AutoBindOnFinalize;
use file_storage::domain::service::{FileService, FinalizeByTokenOutcome};
use file_storage::infra::backend::StorageBackend;
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{File, FileVersion, NewFile, OwnerKind, VersionStatus};

mod common;
use common::{StackBuilder, backend_path, ctx, put_claims};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Returns the raw backend and `Store` so tests control the object and inspect the version row.
async fn build_service() -> (Arc<FileService>, Arc<dyn StorageBackend>, Store) {
    let s = StackBuilder::new("finalize-test").build().await;
    (s.svc, s.backend, s.store)
}

/// Shares one `Issuer` so handler-level tests can mint tokens the service's verifier accepts.
async fn build_full_service_with_issuer(
    issuer: Arc<Issuer>,
) -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn StorageBackend>,
    Store,
) {
    let s = StackBuilder::new("finalize-test")
        .issuer(issuer)
        .build()
        .await;
    (s.svc, s.msvc, s.backend, s.store)
}

fn headers_with_token(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-fs-token",
        token.parse().expect("token is a valid header value"),
    );
    headers
}

fn new_file() -> NewFile {
    new_file_with_mime("application/octet-stream")
}

fn new_file_with_mime(mime_type: &str) -> NewFile {
    common::new_file_of("finalize.bin", mime_type)
}

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const PDF_MAGIC: &[u8] = b"%PDF-1.4\n";

/// Finalizing before any bytes were put is rejected on both entry points and leaves the row pending.
#[tokio::test]
async fn finalize_without_prior_put_is_rejected() {
    for by_token in [false, true] {
        let (svc, _backend, store) = build_service().await;
        let ctx = ctx(Uuid::now_v7());
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();

        let err = if by_token {
            // Claims mirror what `handlers::finalize_version` builds after verifying a signed token.
            let claims = put_claims(
                ticket.file_id,
                ticket.version_id,
                backend_path(ticket.file_id, ticket.version_id),
                false,
            );
            svc.finalize_upload_by_token(&claims, 100, vec![0u8; 32])
                .await
                .map(|_| ())
                .unwrap_err()
        } else {
            svc.finalize_upload(&ctx, ticket.file_id, ticket.version_id, 100, vec![0u8; 32])
                .await
                .unwrap_err()
        };
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "by_token={by_token}: expected Validation, got {err:?}"
        );

        let version = store
            .get_version(ticket.file_id, ticket.version_id)
            .await
            .unwrap()
            .expect("version row must still exist");
        assert_eq!(
            version.status,
            VersionStatus::Pending,
            "by_token={by_token}"
        );
        assert_eq!(version.size, 0, "by_token={by_token}");
    }
}

#[tokio::test]
async fn finalize_size_mismatch_is_rejected() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    backend
        .put(&path, Bytes::from_static(b"hello"))
        .await
        .unwrap();

    let err = svc
        .finalize_upload(
            &ctx,
            ticket.file_id,
            ticket.version_id,
            999,
            hash::sha256(b"hello"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, VersionStatus::Pending);
}

/// Finalize trusts the sidecar-reported hash and does not recompute it.
#[tokio::test]
async fn finalize_persists_reported_hash() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    backend
        .put(&path, Bytes::from_static(b"hello"))
        .await
        .unwrap();

    let reported_hash = vec![0xabu8; 32];
    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        5,
        reported_hash.clone(),
    )
    .await
    .unwrap();

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(version.size, 5);
    assert_eq!(version.hash_value, reported_hash);
}

/// Replays the same correct size/hash so it reaches the repo-level CAS, which must conflict.
#[tokio::test]
async fn finalize_upload_after_already_available_returns_conflict() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, known_bytes.clone()).await.unwrap();

    let true_size = i64::try_from(known_bytes.len()).unwrap();
    let true_hash = hash::sha256(&known_bytes);

    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        true_size,
        true_hash.clone(),
    )
    .await
    .unwrap();

    let err = svc
        .finalize_upload(
            &ctx,
            ticket.file_id,
            ticket.version_id,
            true_size,
            true_hash.clone(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(version.size, true_size);
    assert_eq!(version.hash_value, true_hash);
}

/// Declared `image/png` but PDF bytes uploaded (policy bypass attempt); version must stay pending.
#[tokio::test]
async fn finalize_rejects_content_not_matching_declared_mime() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file_with_mime("image/png"), None, false)
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    backend
        .put(&path, Bytes::from_static(PDF_MAGIC))
        .await
        .unwrap();

    let true_size = i64::try_from(PDF_MAGIC.len()).unwrap();
    let true_hash = hash::sha256(PDF_MAGIC);

    let err = svc
        .finalize_upload(
            &ctx,
            ticket.file_id,
            ticket.version_id,
            true_size,
            true_hash,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MimeMismatch { .. }),
        "expected MimeMismatch, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, VersionStatus::Pending);
    assert_eq!(
        version.mime_type, "image/png",
        "declared mime is untouched by a rejected finalize"
    );
}

/// Declared with a `charset` parameter so the stored value proves the sniffed type was persisted.
#[tokio::test]
async fn finalize_persists_validated_mime() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(
            &ctx,
            new_file_with_mime("image/png; charset=binary"),
            None,
            false,
        )
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    backend
        .put(&path, Bytes::from_static(PNG_MAGIC))
        .await
        .unwrap();

    let true_size = i64::try_from(PNG_MAGIC.len()).unwrap();
    let true_hash = hash::sha256(PNG_MAGIC);

    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        true_size,
        true_hash,
    )
    .await
    .unwrap();

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(
        version.mime_type, "image/png",
        "stored mime_type must be the sniffed canonical type, not the declared string verbatim"
    );
}

// `finalize_token_grace_secs`: the sidecar checks the PUT token once at start, so a slow-but-live
// upload can reach finalize after the token's `exp`.

const TEST_INTERNAL_SECRET: &str = "interim-shared-secret";

fn headers_with_internal_token(token: &str) -> HeaderMap {
    let mut headers = headers_with_token(token);
    headers.insert(
        "x-fs-internal-token",
        TEST_INTERNAL_SECRET
            .parse()
            .expect("secret is a valid header value"),
    );
    headers
}

/// Which handler an expired-token row drives.
#[derive(Clone, Copy)]
enum GraceEndpoint {
    Finalize,
    ReportPart,
}

/// Issues a token that expired `expired_secs_ago` (negative: still valid) and drives `endpoint` with
/// `grace_secs` of grace, with or without the `x-fs-internal-token` header.
/// `Ok(status)` is the handler's own response; `Err(code)` its error status. For `Finalize` the
/// true bytes are in the backend, so an accepted token really finalizes (returned as the row's
/// version status).
async fn call_with_expired_token(
    endpoint: GraceEndpoint,
    expired_secs_ago: i64,
    grace_secs: i64,
    send_internal_header: bool,
) -> (Result<StatusCode, u16>, VersionStatus) {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, backend, store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, known_bytes.clone()).await.unwrap();

    let upload_id = Uuid::now_v7();
    let part_number = 1u32;
    let base = put_claims(ticket.file_id, ticket.version_id, path, false);
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() - expired_secs_ago;
    let claims = match endpoint {
        GraceEndpoint::Finalize => Claims { exp, ..base },
        GraceEndpoint::ReportPart => Claims {
            op: Op::MultipartPart,
            exp,
            multipart: MultipartClaims {
                upload_id,
                part_number,
                offset: 0,
                size: 5,
                backend_handle: String::new(),
            },
            ..base
        },
    };
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");
    let verifier = Arc::new(svc.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(
        TEST_INTERNAL_SECRET.to_owned(),
        time::Duration::seconds(grace_secs),
    ));
    let headers = if send_internal_header {
        headers_with_internal_token(&token)
    } else {
        headers_with_token(&token)
    };

    let result = match endpoint {
        GraceEndpoint::Finalize => finalize_version(
            Extension(Arc::clone(&svc)),
            Extension(verifier),
            Extension(finalize_auth),
            Path((ticket.file_id, ticket.version_id)),
            headers,
            Json(FinalizeUploadReq {
                size: i64::try_from(known_bytes.len()).unwrap(),
                hash_hex: hex::encode(hash::sha256(&known_bytes)),
            }),
        )
        .await
        .map(|r| r.into_response().status()),
        GraceEndpoint::ReportPart => report_multipart_part(
            Extension(msvc),
            Extension(verifier),
            Extension(finalize_auth),
            Path((ticket.file_id, ticket.version_id, upload_id, part_number)),
            headers,
            Json(ReportPartReq {
                backend_etag: "etag-1".to_owned(),
                hash_hex: hex::encode(hash::sha256(b"hello")),
                size: 5,
            }),
        )
        .await
        .map(|r| r.into_response().status()),
    };
    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    (result.map_err(|e| e.status_code()), version.status)
}

/// `finalize_token_grace_secs`: an expired token is accepted within the grace window and rejected
/// (403, like any invalid token) beyond it, on both token-authenticated sidecar callbacks.
#[tokio::test]
async fn expired_token_grace_window_applies_to_finalize_and_report_part() {
    // Within grace: 2 minutes past exp, 1 hour of grace. Beyond: 2 hours past exp, 1 minute grace.
    let (within, beyond) = ((120, 3600), (7200, 60));
    {
        let (result, status) =
            call_with_expired_token(GraceEndpoint::Finalize, within.0, within.1, true).await;
        assert_eq!(
            result,
            Ok(StatusCode::NO_CONTENT),
            "finalize/within grace: an expired-but-within-grace token must be accepted"
        );
        assert_eq!(
            status,
            VersionStatus::Available,
            "finalize/within grace: finalize must have actually gone through"
        );
    }
    {
        let (result, _) =
            call_with_expired_token(GraceEndpoint::ReportPart, within.0, within.1, true).await;
        // No multipart session exists, so it may fail -- but never with 403 (token rejection).
        assert_ne!(
            result,
            Err(403),
            "report_part/within grace: an expired-but-within-grace token must pass verification"
        );
    }
    for (row, endpoint) in [
        ("finalize/beyond grace", GraceEndpoint::Finalize),
        ("report_part/beyond grace", GraceEndpoint::ReportPart),
    ] {
        let (result, _) = call_with_expired_token(endpoint, beyond.0, beyond.1, true).await;
        assert_eq!(
            result,
            Err(403),
            "{row}: a token expired beyond the grace must map to 403, same as any invalid token"
        );
    }
}

/// With an internal secret configured, a valid token without the `x-fs-internal-token` header is
/// rejected (403) on both sidecar callbacks, and finalize leaves the row pending.
#[tokio::test]
async fn internal_secret_required_rejects_missing_header() {
    for endpoint in [GraceEndpoint::Finalize, GraceEndpoint::ReportPart] {
        let (result, status) = call_with_expired_token(endpoint, -3600, 0, false).await;
        assert_eq!(
            result,
            Err(403),
            "missing internal credential must map to 403"
        );
        assert_eq!(status, VersionStatus::Pending);
    }
}

// --- Auto-bind on finalize ---

/// Puts `bytes` at the version's backend path and returns that path.
async fn put_object(
    backend: &Arc<dyn StorageBackend>,
    file_id: Uuid,
    version_id: Uuid,
    bytes: &Bytes,
) -> String {
    let path = backend_path(file_id, version_id);
    backend.put(&path, bytes.clone()).await.unwrap();
    path
}

/// Token-authenticated finalize reporting `bytes`' true size and hash.
async fn finalize_by_token(
    svc: &FileService,
    claims: &Claims,
    bytes: &Bytes,
) -> Result<FinalizeByTokenOutcome, DomainError> {
    svc.finalize_upload_by_token(
        claims,
        i64::try_from(bytes.len()).unwrap(),
        hash::sha256(bytes),
    )
    .await
}

async fn content_id_of(store: &Store, file_id: Uuid) -> Option<Uuid> {
    store
        .get_file(&AccessScope::allow_all(), file_id)
        .await
        .unwrap()
        .expect("file")
        .content_id
}

/// A file whose first content (`winner`) is finalized and bound, plus a second pending version
/// whose `content_id IS NULL` auto-bind CAS can no longer win: `(file_id, loser_version_id,
/// loser token claims, loser bytes)`.
async fn file_with_lost_cas_loser(
    svc: &FileService,
    backend: &Arc<dyn StorageBackend>,
    ctx: &SecurityContext,
) -> (Uuid, Uuid, Claims, Bytes) {
    let ticket = svc.create_file(ctx, new_file(), None, true).await.unwrap();
    let winner_bytes = Bytes::from_static(b"winner");
    put_object(backend, ticket.file_id, ticket.version_id, &winner_bytes).await;
    svc.finalize_upload(
        ctx,
        ticket.file_id,
        ticket.version_id,
        i64::try_from(winner_bytes.len()).unwrap(),
        hash::sha256(&winner_bytes),
    )
    .await
    .unwrap();
    svc.bind(ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    let ticket2 = svc.presign_version(ctx, ticket.file_id).await.unwrap();
    let loser_bytes = Bytes::from_static(b"loser!");
    let path = put_object(backend, ticket.file_id, ticket2.version_id, &loser_bytes).await;
    let claims = put_claims(ticket.file_id, ticket2.version_id, path, true);
    (ticket.file_id, ticket2.version_id, claims, loser_bytes)
}

/// A `bind_on_finalize` token makes finalize itself bind the first content (`content_id IS NULL`
/// CAS).
#[tokio::test]
async fn finalize_with_bind_claim_binds_first_content() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    let bytes = Bytes::from_static(b"auto-bind me");
    let path = put_object(&backend, ticket.file_id, ticket.version_id, &bytes).await;

    let claims = put_claims(ticket.file_id, ticket.version_id, path, true);
    let outcome = finalize_by_token(&svc, &claims, &bytes).await.unwrap();
    assert_eq!(outcome.bind_state, Some(BindState::Bound));
    assert!(
        outcome.etag.is_some(),
        "bound finalize must return the new ETag"
    );
    assert_eq!(
        content_id_of(&store, ticket.file_id).await,
        Some(ticket.version_id),
        "finalize with the bind claim must have bound the first content"
    );
}

/// A retry replays the ORIGINAL `Bound` decision even if an unrelated rebind has since moved
/// the file's content.
#[tokio::test]
async fn finalize_by_token_retry_replays_bound_decision_despite_later_rebind() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    let bytes_a = Bytes::from_static(b"version A content");
    let path_a = put_object(&backend, ticket.file_id, ticket.version_id, &bytes_a).await;

    let claims_a = put_claims(ticket.file_id, ticket.version_id, path_a, true);
    let original = finalize_by_token(&svc, &claims_a, &bytes_a).await.unwrap();
    assert_eq!(original.bind_state, Some(BindState::Bound));
    assert!(original.etag.is_some());

    let ticket_b = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    let bytes_b = Bytes::from_static(b"version B content");
    put_object(&backend, ticket.file_id, ticket_b.version_id, &bytes_b).await;
    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket_b.version_id,
        i64::try_from(bytes_b.len()).unwrap(),
        hash::sha256(&bytes_b),
    )
    .await
    .unwrap();
    // File is bound to A, so rebinding to B needs A's ETag as `If-Match`.
    svc.bind(
        &ctx,
        ticket.file_id,
        ticket_b.version_id,
        original.etag.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(
        content_id_of(&store, ticket.file_id).await,
        Some(ticket_b.version_id),
        "the file must now legitimately point at version B"
    );

    // Retry of A's token must still see `Bound` + A's ETag, not `Conflict` against B.
    let retry = finalize_by_token(&svc, &claims_a, &bytes_a)
        .await
        .expect("retry of an already-decided Bound finalize must not fail");
    assert_eq!(
        retry.bind_state,
        Some(BindState::Bound),
        "must replay the ORIGINAL Bound decision, not the file's current pointer"
    );
    assert_eq!(
        retry.etag, original.etag,
        "must report version A's own ETag, not a Conflict against version B"
    );
    assert_eq!(retry.current_etag, None);
}

/// Two create-tokens racing for one new file: the loser reports `conflict` + the current ETag and its
/// upload still succeeds. A lost auto-bind CAS leaves `bound_on_finalize` unset, so a retry
/// re-derives `Conflict` with the same ETag; the reported ETag works as `If-Match` for a manual rebind.
#[tokio::test]
async fn finalize_bind_claim_lost_cas_reports_conflict() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, loser_version_id, claims, loser_bytes) =
        file_with_lost_cas_loser(&svc, &backend, &ctx).await;

    let outcome = finalize_by_token(&svc, &claims, &loser_bytes)
        .await
        .expect("finalize itself succeeds \u{2014} only the bind CAS is lost");
    assert_eq!(outcome.bind_state, Some(BindState::Conflict));
    assert!(
        outcome.current_etag.is_some(),
        "conflict must carry the CURRENT ETag for a manual rebind's If-Match"
    );
    assert_eq!(outcome.etag, None);

    let retry = finalize_by_token(&svc, &claims, &loser_bytes)
        .await
        .expect("honest retry of a lost-CAS finalize must still converge");
    assert_eq!(retry.bind_state, Some(BindState::Conflict));
    assert_eq!(
        retry.current_etag, outcome.current_etag,
        "must report the SAME live current ETag as the original lost-CAS response"
    );
    assert_eq!(retry.etag, None);

    let version = store
        .get_version(file_id, loser_version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, VersionStatus::Available);
    assert!(
        !version.bound_on_finalize,
        "a lost CAS must never set the persisted bind flag"
    );
    svc.bind(
        &ctx,
        file_id,
        loser_version_id,
        outcome.current_etag.as_deref(),
    )
    .await
    .expect("manual rebind with the conflict-reported ETag");
}

// Manual-mode retry convergence: every single-part upload goes through the replay-safe
// `publish_exclusive`.

/// A manual-token finalize replayed after the version is `Available` converges to the same success,
/// not 409; a replay with mismatched size/hash (forged) stays rejected; a retry after a manual bind
/// still reports `Manual`.
#[tokio::test]
async fn finalize_manual_token_converges_on_retry() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let bytes = Bytes::from_static(b"manual retry");
    let path = put_object(&backend, ticket.file_id, ticket.version_id, &bytes).await;
    let claims = put_claims(ticket.file_id, ticket.version_id, path, false);

    let outcome = finalize_by_token(&svc, &claims, &bytes).await.unwrap();
    assert_eq!(outcome.bind_state, None, "manual mode requests no bind");

    let retry = finalize_by_token(&svc, &claims, &bytes)
        .await
        .expect("honest manual-mode PUT retry must converge to success, not 409");
    assert_eq!(retry.bind_state, Some(BindState::Manual));
    assert_eq!(retry.etag, None);
    assert_eq!(retry.current_etag, None);

    let versions = store.list_versions(ticket.file_id).await.unwrap();
    assert_eq!(
        versions.len(),
        1,
        "the retry must not create or touch a second version row"
    );
    assert_eq!(
        content_id_of(&store, ticket.file_id).await,
        None,
        "manual mode must never auto-bind, even on retry"
    );

    let mismatched = Bytes::from_static(b"forged replay!!!");
    let err = finalize_by_token(&svc, &claims, &mismatched)
        .await
        .expect_err("a mismatched replay must not converge to success");
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "expected a hash-mismatch rejection, got {err:?}"
    );

    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("manual bind after finalize");
    let retry = finalize_by_token(&svc, &claims, &bytes)
        .await
        .expect("retry after manual bind must converge to success");
    assert_eq!(
        retry.bind_state,
        Some(BindState::Manual),
        "a manual upload never reports a bind outcome, even once bound by hand"
    );
    assert_eq!(retry.etag, None);
    assert_eq!(retry.current_etag, None);
}

// --- Store::finalize_version (auto-bind CAS, lost CAS) ---

fn store_file(file_id: Uuid, tenant_id: Uuid, content_id: Option<Uuid>) -> File {
    let now = time::OffsetDateTime::now_utc();
    File {
        file_id,
        tenant_id,
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "doc.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        content_id,
        meta_version: 0,
        created_at: now,
        last_modified_at: now,
    }
}

fn pending_version(file_id: Uuid, version_id: Uuid) -> FileVersion {
    FileVersion {
        file_id,
        version_id,
        mime_type: "application/octet-stream".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: HashMode::WholeSha256.as_str().to_owned(),
        part_count: None,
        status: VersionStatus::Pending,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(file_id, version_id),
        created_at: time::OffsetDateTime::now_utc(),
        bound_on_finalize: false,
    }
}

fn audit_entry(tenant_id: Uuid, file_id: Uuid, op: AuditOperation) -> AuditEntry {
    AuditEntry::success(
        tenant_id,
        "user",
        Uuid::now_v7(),
        Some(file_id),
        op,
        serde_json::json!({}),
    )
}

fn file_event(tenant_id: Uuid, owner_id: Uuid, file_id: Uuid, event_type: &str) -> FileEvent {
    FileEvent {
        tenant_id,
        owner_id,
        file_id,
        event_type: event_type.to_owned(),
        payload: serde_json::json!({}),
    }
}

async fn build_store() -> (Store, Arc<DBProvider<DbError>>) {
    let s = StackBuilder::new("finalize-test").build().await;
    (s.store, s.db)
}

/// A file (already bound to `content_id`, if given) with one pending version; returns
/// `(tenant_id, file_id, version_id)`.
async fn seed_pending_version<C: toolkit_db::secure::DBRunner>(
    conn: &C,
    content_id: Option<Uuid>,
) -> (Uuid, Uuid, Uuid) {
    let scope = AccessScope::allow_all();
    let (tenant_id, file_id, version_id) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    FileRepo::new()
        .create(conn, &scope, &store_file(file_id, tenant_id, content_id))
        .await
        .expect("create file");
    VersionRepo::new()
        .insert(conn, &scope, &pending_version(file_id, version_id))
        .await
        .expect("insert pending version");
    (tenant_id, file_id, version_id)
}

#[tokio::test]
async fn finalize_version_with_auto_bind_marks_available_and_binds_content() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let owner_id = Uuid::now_v7();
    let (tenant_id, file_id, v1) = seed_pending_version(&conn, None).await;

    let auto_bind = AutoBindOnFinalize {
        expected_content_id: None,
        audit: audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        event: Some(file_event(
            tenant_id,
            owner_id,
            file_id,
            "file.content_updated",
        )),
    };

    let outcome = store
        .finalize_version(
            file_id,
            v1,
            456,
            vec![9u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            None,
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            Some(auto_bind),
        )
        .await
        .expect("finalize with auto_bind must not error");
    assert!(outcome.updated);
    assert!(
        outcome.bound,
        "the CAS must win on a first bind (expected None)"
    );

    let v_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .expect("version exists");
    assert_eq!(v_row.status, VersionStatus::Available);
    assert!(v_row.is_current, "auto_bind must mark the version current");
    assert!(
        v_row.bound_on_finalize,
        "a won auto-bind CAS must persist the bound_on_finalize flag \
         (read back by FileService::finalize_upload_by_token's retry fast path)"
    );

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(file.content_id, Some(v1), "auto_bind must set content_id");

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 2, "finalize row + bind row");
    let events = store.list_file_events(file_id).await.unwrap();
    assert_eq!(
        events.len(),
        1,
        "the auto_bind's event must be enqueued once"
    );
}

#[tokio::test]
async fn finalize_version_lost_cas_when_not_pending_leaves_row_untouched() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let versions = VersionRepo::new();

    let files = FileRepo::new();
    let bound_elsewhere = Uuid::now_v7(); // stands in for an unrelated current version
    let (tenant_id, file_id, v1) = seed_pending_version(&conn, Some(bound_elsewhere)).await;

    // Finalize it once, directly through the repo, bypassing `Store` (no
    // audit row) -- this is the "already available" state a second,
    // racing finalize call would observe.
    let first_hash = vec![1u8; 32];
    let first_updated = versions
        .finalize(
            &conn,
            &scope,
            file_id,
            v1,
            42,
            first_hash.clone(),
            "whole-sha256",
            None,
            Some("text/plain".to_owned()),
        )
        .await
        .expect("direct finalize must succeed");
    assert!(first_updated, "the version was pending, so this must win");

    let outcome = store
        .finalize_version(
            file_id,
            v1,
            999,
            vec![2u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            Some("application/json".to_owned()),
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            None,
        )
        .await
        .expect("a lost CAS is reported, not an error");
    assert!(!outcome.updated, "the version is no longer pending");
    assert!(!outcome.bound, "no bind can happen when updated is false");

    let v_row = versions
        .get(&conn, &scope, file_id, v1)
        .await
        .unwrap()
        .expect("version still exists");
    assert_eq!(v_row.status, VersionStatus::Available);
    assert_eq!(v_row.size, 42, "the lost CAS must not overwrite size");
    assert_eq!(
        v_row.hash_value, first_hash,
        "the lost CAS must not overwrite hash_value"
    );
    assert_eq!(
        v_row.mime_type, "text/plain",
        "the lost CAS must not overwrite mime_type"
    );

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(
        file.content_id,
        Some(bound_elsewhere),
        "the lost CAS must not touch the file's content_id"
    );

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(
        audit_rows.is_empty(),
        "a lost CAS (updated == false) must never write an audit row"
    );
}
