//! Finalize of single-part uploads (by caller and by signed token), its bind outcome on retries,
//! the token/secret checks of the `finalize_version` and `report_multipart_part` handlers.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use bytes::Bytes;
use toolkit::api::rest::extract::Path;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::api::rest::handlers::{
    FinalizeAuth, FinalizeUploadReq, ReportPartReq, finalize_version, report_multipart_part,
};
use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::error::DomainError;
use file_storage::domain::etag::content_etag;
use file_storage::domain::multipart::BindState;
use file_storage::domain::ports::AutoBindOnFinalize;
use file_storage::domain::service::FinalizeByTokenOutcome;
use file_storage::infra::backend::StorageBackend;
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op};
use file_storage_sdk::VersionStatus;

mod common;
use common::{
    MULTI_PART_DECLARED_SIZE, Stack, StackBuilder, backend_path, bare_file, ctx, initiate,
    put_claims,
};

fn headers_with_token(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-fs-token",
        token.parse().expect("token is a valid header value"),
    );
    headers
}

fn new_file() -> file_storage_sdk::NewFile {
    common::new_file_of("finalize.bin", "application/octet-stream")
}

/// Puts `bytes` at the version's backend path and returns that path.
async fn put_object(
    backend: &Arc<dyn StorageBackend>,
    file_id: Uuid,
    version_id: Uuid,
    bytes: &[u8],
) -> String {
    let path = backend_path(file_id, version_id);
    backend
        .put(&path, Bytes::copy_from_slice(bytes))
        .await
        .unwrap();
    path
}

/// Finalize is rejected on both entry points when the object is missing or its size differs from
/// the reported one, and the version stays pending.
#[tokio::test]
async fn finalize_rejects_missing_object_and_size_mismatch() {
    // (row, by_token, bytes put before finalize, reported size)
    let rows = [
        ("no object, by id", false, None, 100),
        ("no object, by token", true, None, 100),
        ("size mismatch, by id", false, Some(&b"hello"[..]), 999),
        ("size mismatch, by token", true, Some(&b"hello"[..]), 999),
    ];
    for (row, by_token, stored, size) in rows {
        let Stack {
            svc,
            backend,
            store,
            ..
        } = StackBuilder::new("finalize-test").build().await;
        let ctx = ctx(Uuid::now_v7());
        let t = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();
        let path = backend_path(t.file_id, t.version_id);
        if let Some(bytes) = stored {
            put_object(&backend, t.file_id, t.version_id, bytes).await;
        }
        let digest = hash::sha256(b"hello");

        let err = if by_token {
            let claims = put_claims(t.file_id, t.version_id, path, false);
            svc.finalize_upload_by_token(&claims, size, digest)
                .await
                .map(|_| ())
                .unwrap_err()
        } else {
            svc.finalize_upload(&ctx, t.file_id, t.version_id, size, digest)
                .await
                .unwrap_err()
        };
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{row}: got {err:?}"
        );

        let version = store
            .get_version(t.file_id, t.version_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(version.status, VersionStatus::Pending, "{row}");
        assert_eq!(version.size, 0, "{row}");
    }
}

/// Finalize trusts the reported hash without recomputing it; a second finalize of the now
/// available version conflicts and leaves the row as the first one wrote it.
#[tokio::test]
async fn finalize_persists_reported_hash_and_second_finalize_conflicts() {
    let Stack {
        svc,
        backend,
        store,
        ..
    } = StackBuilder::new("finalize-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    put_object(&backend, t.file_id, t.version_id, b"hello, world!").await;

    let reported_hash = vec![0xabu8; 32];
    svc.finalize_upload(&ctx, t.file_id, t.version_id, 13, reported_hash.clone())
        .await
        .unwrap();
    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, 13, reported_hash.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Conflict { .. }), "got {err:?}");

    let version = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(version.size, 13);
    assert_eq!(version.hash_value, reported_hash);
}

/// The declared MIME is checked against the sniffed bytes: a mismatch (PDF bytes declared as
/// `image/png`) leaves the version pending and untouched; a match persists the sniffed canonical
/// type, not the declared string (declared with a `charset` parameter to prove it).
#[tokio::test]
async fn finalize_validates_declared_mime_against_content() {
    const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    const PDF_MAGIC: &[u8] = b"%PDF-1.4\n";
    // (declared, bytes, expected stored mime; None = rejected with MimeMismatch)
    let rows = [
        ("image/png", PDF_MAGIC, None),
        ("image/png; charset=binary", PNG_MAGIC, Some("image/png")),
    ];
    for (declared, bytes, accepted_as) in rows {
        let Stack {
            svc,
            backend,
            store,
            ..
        } = StackBuilder::new("finalize-test").build().await;
        let ctx = ctx(Uuid::now_v7());
        let file = common::new_file_of("finalize.bin", declared);
        let t = svc.create_file(&ctx, file, None, false).await.unwrap();
        put_object(&backend, t.file_id, t.version_id, bytes).await;

        let result = svc
            .finalize_upload(
                &ctx,
                t.file_id,
                t.version_id,
                i64::try_from(bytes.len()).unwrap(),
                hash::sha256(bytes),
            )
            .await;
        let version = store
            .get_version(t.file_id, t.version_id)
            .await
            .unwrap()
            .unwrap();
        match accepted_as {
            None => {
                let err = result.unwrap_err();
                assert!(
                    matches!(err, DomainError::MimeMismatch { .. }),
                    "got {err:?}"
                );
                assert_eq!(version.status, VersionStatus::Pending);
                assert_eq!(version.mime_type, declared);
            }
            Some(stored) => {
                result.unwrap();
                assert_eq!(version.status, VersionStatus::Available);
                assert_eq!(version.mime_type, stored);
            }
        }
    }
}

// `finalize_token_grace_secs`: the sidecar checks the PUT token once at start, so a slow-but-live
// upload can reach finalize after the token's `exp`.

const TEST_INTERNAL_SECRET: &str = "interim-shared-secret";

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
    let Stack {
        svc,
        msvc,
        backend,
        store,
        ..
    } = StackBuilder::new("finalize-test")
        .issuer(Arc::clone(&issuer))
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let known_bytes = b"hello, world!";
    let path = put_object(&backend, t.file_id, t.version_id, known_bytes).await;

    let upload_id = Uuid::now_v7();
    let base = put_claims(t.file_id, t.version_id, path, false);
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() - expired_secs_ago;
    let claims = match endpoint {
        GraceEndpoint::Finalize => Claims { exp, ..base },
        GraceEndpoint::ReportPart => Claims {
            op: Op::MultipartPart,
            exp,
            multipart: MultipartClaims {
                upload_id,
                part_number: 1,
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
    let mut headers = headers_with_token(&token);
    if send_internal_header {
        headers.insert("x-fs-internal-token", TEST_INTERNAL_SECRET.parse().unwrap());
    }

    let result = match endpoint {
        GraceEndpoint::Finalize => finalize_version(
            Extension(Arc::clone(&svc)),
            Extension(verifier),
            Extension(finalize_auth),
            Path((t.file_id, t.version_id)),
            headers,
            Json(FinalizeUploadReq {
                size: i64::try_from(known_bytes.len()).unwrap(),
                hash_hex: hex::encode(hash::sha256(known_bytes)),
            }),
        )
        .await
        .map(|r| r.into_response().status()),
        GraceEndpoint::ReportPart => report_multipart_part(
            Extension(msvc),
            Extension(verifier),
            Extension(finalize_auth),
            Path((t.file_id, t.version_id, upload_id, 1)),
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
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .unwrap();
    (result.map_err(|e| e.status_code()), version.status)
}

/// `finalize_token_grace_secs`: an expired token is accepted within the grace window and rejected
/// (403, like any invalid token) beyond it, on both token-authenticated sidecar callbacks; with an
/// internal secret configured, a valid token without the `x-fs-internal-token` header is 403 too.
#[tokio::test]
async fn expired_token_grace_window_and_internal_secret_apply_to_finalize_and_report_part() {
    // (row, endpoint, (secs past exp, grace secs), internal header sent, handler result, version status)
    // The report-part rows have no multipart session, so passing verification ends in a 404.
    use GraceEndpoint::{Finalize, ReportPart};
    let rows = [
        (
            "finalize within grace",
            Finalize,
            (120, 3600),
            true,
            Ok(StatusCode::NO_CONTENT),
            VersionStatus::Available,
        ),
        (
            "report_part within grace",
            ReportPart,
            (120, 3600),
            true,
            Err(404),
            VersionStatus::Pending,
        ),
        (
            "finalize beyond grace",
            Finalize,
            (7200, 60),
            true,
            Err(403),
            VersionStatus::Pending,
        ),
        (
            "report_part beyond grace",
            ReportPart,
            (7200, 60),
            true,
            Err(403),
            VersionStatus::Pending,
        ),
        (
            "finalize without secret",
            Finalize,
            (-3600, 0),
            false,
            Err(403),
            VersionStatus::Pending,
        ),
        (
            "report_part without secret",
            ReportPart,
            (-3600, 0),
            false,
            Err(403),
            VersionStatus::Pending,
        ),
    ];
    for (row, endpoint, (expired_ago, grace), internal, expected, expected_status) in rows {
        let (result, status) =
            call_with_expired_token(endpoint, expired_ago, grace, internal).await;
        assert_eq!(result, expected, "{row}");
        assert_eq!(status, expected_status, "{row}");
    }
}

/// Token-authenticated finalize reporting `bytes`' true size and hash.
async fn finalize_by_token(
    svc: &file_storage::domain::service::FileService,
    claims: &Claims,
    bytes: &[u8],
) -> Result<FinalizeByTokenOutcome, DomainError> {
    svc.finalize_upload_by_token(
        claims,
        i64::try_from(bytes.len()).unwrap(),
        hash::sha256(bytes),
    )
    .await
}

/// A manual-token finalize replayed after the version is `Available` converges to the same
/// success (not 409) and never reports a bind outcome, even once the version was bound by hand; a
/// replay with a mismatched hash stays rejected.
#[tokio::test]
async fn finalize_by_token_manual_retry_converges_and_forged_replay_is_rejected() {
    let Stack {
        svc,
        backend,
        store,
        ..
    } = StackBuilder::new("finalize-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let bytes = b"manual retry";
    let path = put_object(&backend, t.file_id, t.version_id, bytes).await;
    let claims = put_claims(t.file_id, t.version_id, path, false);

    let outcome = finalize_by_token(&svc, &claims, bytes).await.unwrap();
    assert_eq!(outcome.bind_state, None, "manual mode requests no bind");

    for bound_by_hand in [false, true] {
        if bound_by_hand {
            svc.bind(&ctx, t.file_id, t.version_id, None).await.unwrap();
        }
        let retry = finalize_by_token(&svc, &claims, bytes)
            .await
            .expect("honest manual-mode PUT retry must converge to success, not 409");
        assert_eq!(
            retry.bind_state,
            Some(BindState::Manual),
            "bound_by_hand={bound_by_hand}"
        );
        assert_eq!((retry.etag, retry.current_etag), (None, None));
    }
    assert_eq!(store.list_versions(t.file_id).await.unwrap().len(), 1);

    let err = finalize_by_token(&svc, &claims, b"forged replay!!!")
        .await
        .expect_err("a mismatched replay must not converge to success");
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "got {err:?}"
    );
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

/// `Store::finalize_version` directly, with an optional auto-bind over an unbound file.
async fn store_finalize(
    store: &file_storage::infra::storage::Store,
    tenant_id: Uuid,
    file_id: Uuid,
    version_id: Uuid,
    size: i64,
    auto_bind: bool,
) -> file_storage::domain::ports::FinalizeVersionOutcome {
    let auto_bind = auto_bind.then(|| AutoBindOnFinalize {
        expected_content_id: None,
        audit: audit_entry(tenant_id, file_id, AuditOperation::PatchContent),
        event: Some(FileEvent {
            tenant_id,
            owner_id: Uuid::now_v7(),
            file_id,
            event_type: "file.content_updated".to_owned(),
            payload: serde_json::json!({}),
        }),
    });
    store
        .finalize_version(
            file_id,
            version_id,
            size,
            vec![u8::try_from(size).unwrap(); 32],
            HashMode::WholeSha256,
            None,
            None,
            None,
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            auto_bind,
        )
        .await
        .expect("finalize_version")
}

/// `Store::finalize_version` with an auto-bind: the version becomes available and current, the
/// file's content is bound, and one audit row for each of finalize and bind plus one event are
/// written. A second finalize of the same version loses the CAS and changes nothing, auto-bind
/// included.
#[tokio::test]
async fn finalize_version_auto_bind_wins_then_lost_cas_leaves_row_untouched() {
    let Stack { svc, store, .. } = StackBuilder::new("finalize-test").build().await;
    let tenant = Uuid::now_v7();
    let ctx = ctx(tenant);
    let t = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let audit_before = store.list_audit(t.file_id).await.unwrap().len();

    let outcome = store_finalize(&store, tenant, t.file_id, t.version_id, 7, true).await;
    assert!(
        outcome.updated && outcome.bound,
        "the CAS must win on a first bind"
    );
    let version = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Available);
    assert!(version.is_current);
    assert!(
        version.bound_on_finalize,
        "the retry path reads this flag back"
    );
    assert_eq!(
        common::content_id_of(&store, t.file_id).await,
        Some(t.version_id)
    );
    assert_eq!(
        store.list_audit(t.file_id).await.unwrap().len(),
        audit_before + 2
    );
    assert_eq!(
        store.list_file_events(t.file_id).await.unwrap().len(),
        2 /* created + bind */
    );

    let lost = store_finalize(&store, tenant, t.file_id, t.version_id, 9, true).await;
    assert!(
        !lost.updated && !lost.bound,
        "the version is no longer pending"
    );
    let after = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (after.size, after.hash_value),
        (version.size, version.hash_value)
    );
    assert_eq!(
        store.list_audit(t.file_id).await.unwrap().len(),
        audit_before + 2
    );
}

/// Parts reported through the real `report_multipart_part` handler are persisted one row each and
/// `complete` sums them into the version size.
#[tokio::test]
async fn report_part_handler_rows_feed_complete() {
    let Stack {
        backend,
        issuer,
        msvc,
        store,
        svc,
        ..
    } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, MULTI_PART_DECLARED_SIZE).await;
    assert_eq!(plan.parts.len(), 3);
    let router = common::report_part_router(Arc::new(issuer.verifier()), Arc::clone(&msvc));
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    let path = backend_path(file_id, plan.version_id);

    for part in &plan.parts {
        let size = usize::try_from(part.size).unwrap();
        backend
            .upload_part(
                &path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                Bytes::from(vec![b'x'; size]),
            )
            .await
            .expect("backend upload_part");
        let body = serde_json::json!({
            "backend_etag": format!("etag-{}", part.part_number),
            "hash_hex": hex::encode([u8::try_from(part.part_number).unwrap(); 32]),
            "size": part.size,
        });
        let req = common::report_part_request(
            file_id,
            &plan,
            part.part_number,
            common::token_of(&part.upload_url),
            &body,
        );
        let resp = router.clone().oneshot(req).await.expect("router dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "part {}",
            part.part_number
        );
    }

    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    assert_eq!(status.received.len(), 3, "one row per reported part");
    msvc.complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();
    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        version.size,
        i64::try_from(MULTI_PART_DECLARED_SIZE).unwrap()
    );
}

/// The callback is token-authenticated, so the reported `size` must match `claims.multipart.size`;
/// no part row may be persisted for a forged size.
#[tokio::test]
async fn report_part_rejects_forged_size() {
    let Stack {
        issuer, msvc, svc, ..
    } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, 100).await;
    let part = &plan.parts[0];
    let router = common::report_part_router(Arc::new(issuer.verifier()), Arc::clone(&msvc));
    let body = serde_json::json!({
        "backend_etag": "forged-etag",
        "hash_hex": hex::encode([7u8; 32]),
        "size": part.size + 1,
    });
    let req = common::report_part_request(
        file_id,
        &plan,
        part.part_number,
        common::token_of(&part.upload_url),
        &body,
    );

    let resp = router.oneshot(req).await.expect("router dispatch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    assert!(
        status.received.is_empty(),
        "a forged-size report must not persist a part row"
    );
}

/// One upload of `data` to `(file_id, version_id)` through the token path, as the sidecar does it.
struct Upload {
    file_id: Uuid,
    version_id: Uuid,
    auto_bind: bool,
    data: &'static [u8],
}

impl Upload {
    async fn finalize(&self, s: &Stack) -> FinalizeByTokenOutcome {
        let path = backend_path(self.file_id, self.version_id);
        // A retry finds the object already stored (create-exclusive publish).
        if !s.backend.exists(&path).await.unwrap() {
            put_object(&s.backend, self.file_id, self.version_id, self.data).await;
        }
        let claims = put_claims(self.file_id, self.version_id, path, self.auto_bind);
        finalize_by_token(&s.svc, &claims, self.data).await.unwrap()
    }
}

fn assert_outcome(
    out: &FinalizeByTokenOutcome,
    state: BindState,
    etag: Option<&str>,
    current_etag: Option<&str>,
    row: &str,
) {
    assert_eq!(out.bind_state, Some(state), "{row}");
    assert_eq!(out.etag.as_deref(), etag, "{row}: etag");
    assert_eq!(
        out.current_etag.as_deref(),
        current_etag,
        "{row}: current_etag"
    );
}

/// A bind won at finalize replays as `Bound` after the file moved on to another version.
#[tokio::test]
async fn won_bind_replays_as_bound_after_the_file_is_rebound() {
    let s = StackBuilder::new("finalize-replay-bind").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = s
        .svc
        .create_file(&ctx, common::new_file("w.bin"), None, true)
        .await
        .unwrap();
    let up = Upload {
        file_id: t.file_id,
        version_id: t.version_id,
        auto_bind: true,
        data: b"winner",
    };
    let own_etag = content_etag(t.file_id, t.version_id);
    assert_outcome(
        &up.finalize(&s).await,
        BindState::Bound,
        Some(&own_etag),
        None,
        "first",
    );
    assert_eq!(
        common::content_id_of(&s.store, t.file_id).await,
        Some(t.version_id),
        "the finalize itself binds the first content"
    );

    let t2 = s.svc.presign_version(&ctx, t.file_id).await.unwrap();
    let other = Upload {
        file_id: t.file_id,
        version_id: t2.version_id,
        auto_bind: false,
        data: b"other",
    };
    other.finalize(&s).await;
    s.svc
        .bind(&ctx, t.file_id, t2.version_id, Some(&own_etag))
        .await
        .unwrap();
    assert_eq!(
        common::content_id_of(&s.store, t.file_id).await,
        Some(t2.version_id),
        "the file now legitimately points at the second version"
    );

    let replay = up.finalize(&s).await;
    assert_outcome(
        &replay,
        BindState::Bound,
        Some(&own_etag),
        None,
        "replay after rebind",
    );
}

/// A lost CAS replays against the live pointer: `Conflict` with the current `ETag` (the upload
/// itself still succeeds and the persisted bind flag stays unset), then `Bound` once this very
/// version has been bound by hand with that `ETag` as `If-Match`.
#[tokio::test]
async fn lost_cas_replay_reads_the_live_pointer() {
    let s = StackBuilder::new("finalize-replay-bind").build().await;
    let ctx = ctx(Uuid::now_v7());
    let t = s
        .svc
        .create_file(&ctx, common::new_file("l.bin"), None, true)
        .await
        .unwrap();
    let winner = Upload {
        file_id: t.file_id,
        version_id: t.version_id,
        auto_bind: true,
        data: b"winner",
    };
    winner.finalize(&s).await;
    let winner_etag = content_etag(t.file_id, t.version_id);

    let t2 = s.svc.presign_version(&ctx, t.file_id).await.unwrap();
    let loser = Upload {
        file_id: t.file_id,
        version_id: t2.version_id,
        auto_bind: true,
        data: b"loser!",
    };
    let first = loser.finalize(&s).await;
    assert_outcome(
        &first,
        BindState::Conflict,
        None,
        Some(&winner_etag),
        "first",
    );
    let replay = loser.finalize(&s).await;
    assert_outcome(
        &replay,
        BindState::Conflict,
        None,
        Some(&winner_etag),
        "replay",
    );
    let version = s
        .store
        .get_version(t.file_id, t2.version_id)
        .await
        .unwrap()
        .expect("loser version");
    assert_eq!(version.status, VersionStatus::Available);
    assert!(
        !version.bound_on_finalize,
        "a lost CAS must never set the persisted bind flag"
    );

    s.svc
        .bind(&ctx, t.file_id, t2.version_id, Some(&winner_etag))
        .await
        .unwrap();
    let after_bind = loser.finalize(&s).await;
    assert_outcome(
        &after_bind,
        BindState::Bound,
        Some(&content_etag(t.file_id, t2.version_id)),
        None,
        "replay after manual bind",
    );
}
