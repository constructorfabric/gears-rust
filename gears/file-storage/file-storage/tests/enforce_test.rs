#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use axum::http::StatusCode;
use bytes::Bytes;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{MetadataLimits, PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::service::FileService;
use file_storage::infra::external_clients::{QuotaClient, QuotaDecision};
use file_storage::infra::storage::Store;
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile};

mod common;
use common::{Stack, StackBuilder, ctx};

struct CappedQuota {
    cap: u64,
    seen: AtomicU64,
}

impl CappedQuota {
    fn new(cap: u64) -> Self {
        Self {
            cap,
            seen: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl QuotaClient for CappedQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        let total = self.seen.fetch_add(additional_bytes, Ordering::SeqCst) + additional_bytes;
        if total > self.cap {
            Ok(QuotaDecision::Denied {
                reason: format!("would use {total} > cap {}", self.cap),
            })
        } else {
            Ok(QuotaDecision::Allowed)
        }
    }
}

struct ErroringQuota;

#[async_trait]
impl QuotaClient for ErroringQuota {
    async fn check_storage_quota(
        &self,
        _tenant_id: Uuid,
        _owner_id: Uuid,
        _additional_bytes: u64,
        _metric_name: &str,
    ) -> Result<QuotaDecision, DomainError> {
        Err(DomainError::InternalError)
    }
}

async fn build_service(
    quota: Option<Arc<dyn QuotaClient>>,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    DataPlaneService,
    Store,
) {
    let mut builder = StackBuilder::new("enforce");
    if let Some(quota) = quota {
        builder = builder.quota(quota);
    }
    let s = builder.build().await;
    (s.svc, s.psvc, s.dp, s.store)
}

fn new_file(owner: Uuid, mime: &str) -> NewFile {
    common::new_file_for(owner, "doc.bin", mime)
}

/// Sets the tenant-scope policy to `body`.
async fn set_tenant_policy(psvc: &PolicyService, ctx: &SecurityContext, body: PolicyBody) {
    psvc.set_policy(ctx, PolicyScope::Tenant, None, body)
        .await
        .unwrap();
}

/// A replay re-mints a live upload URL, so `replay_idempotency_key` re-checks the CURRENT policy
/// (as on `47f9efd31`): once the MIME is forbidden the retry is rejected, and no second file is made.
#[tokio::test]
async fn idempotent_replay_rechecks_current_mime_policy() {
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();
    let key = Some("retry-after-policy".to_owned());

    let first = svc
        .create_file(&ctx, new_file(owner, "image/png"), key.clone(), false)
        .await
        .expect("first create");
    let same = svc
        .create_file(&ctx, new_file(owner, "image/png"), key.clone(), false)
        .await
        .expect("replay under an unchanged policy");
    assert_eq!(
        (same.file_id, same.version_id),
        (first.file_id, first.version_id)
    );

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            allowed_mime_types: vec!["text/*".to_owned()],
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let err = svc
        .create_file(&ctx, new_file(owner, "image/png"), key, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn create_file_with_disallowed_mime_is_rejected() {
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());

    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            allowed_mime_types: vec!["image/*".to_owned()],
            ..PolicyBody::default()
        },
    )
    .await;

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { .. }),
        "got {err:?}"
    );

    svc.create_file(&ctx, new_file(Uuid::now_v7(), "image/png"), None, false)
        .await
        .expect("image/png should be allowed");
}

#[tokio::test]
async fn finalize_oversized_upload_is_rejected() {
    let (svc, psvc, dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, 100, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::PolicySizeExceeded {
                limit_bytes: 10,
                ..
            }
        ),
        "got {err:?}"
    );

    dp.put_content(
        &ctx,
        t.file_id,
        t.version_id,
        "text/plain",
        Bytes::from_static(b"hello"),
    )
    .await
    .expect("5 bytes within 10-byte cap");
}

/// Rejected on entry; otherwise the DB `CHECK (size >= 0)` would surface as a 500.
#[tokio::test]
async fn finalize_negative_size_is_rejected_with_400_not_500() {
    let (svc, _psvc, _dp, store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, -1, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "size"),
        "got {err:?}"
    );

    let version = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Pending);
    assert_eq!(version.size, 0);

    let claims = common::put_claims(
        t.file_id,
        t.version_id,
        format!("/{}/{}", t.file_id, t.version_id),
        false,
    );
    let err = svc
        .finalize_upload_by_token(&claims, -1, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "size"),
        "got {err:?}"
    );
}

/// Drives `handlers::finalize_version` through a real `axum::Router`: the hash length check
/// lives only in the handler.
async fn finalize_via_router_with_hash_len(hash_byte_len: usize) -> (StatusCode, String) {
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;
    use file_storage::infra::signed_url::Verifier;

    let s = StackBuilder::new("enforce").build().await;
    let Stack { issuer, svc, .. } = s;
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap();

    let token_start = ticket
        .upload_url
        .find("fs-token=")
        .expect("fs-token in URL")
        + "fs-token=".len();
    let token = ticket.upload_url[token_start..].to_owned();

    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(
        "test-internal-secret".to_owned(),
        time::Duration::ZERO,
    ));

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{file_id}/versions/{version_id}/finalize",
            post(handlers::finalize_version),
        )
        .layer(axum::Extension(Arc::clone(&verifier)))
        .layer(axum::Extension(finalize_auth))
        .layer(axum::Extension(Arc::clone(&svc)));

    let body = serde_json::json!({
        "size": 5,
        "hash_hex": hex::encode(vec![0u8; hash_byte_len]),
    });
    let uri = format!(
        "/api/file-storage/v1/files/{}/versions/{}/finalize",
        ticket.file_id, ticket.version_id
    );
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-fs-token", token)
        .header("x-fs-internal-token", "test-internal-secret")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = router.oneshot(req).await.expect("router dispatch");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (status, text)
}

#[tokio::test]
async fn finalize_wrong_length_hash_hex_is_rejected() {
    // Truncated (16 bytes) and oversized (48 bytes) SHA-256 digests.
    for hash_byte_len in [16, 48] {
        let (status, body) = finalize_via_router_with_hash_len(hash_byte_len).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{hash_byte_len} bytes: expected 400, not a downstream 500; got {status} body {body}"
        );
        assert!(
            body.contains("hash_hex"),
            "{hash_byte_len} bytes: expected a hash_hex field violation in the body, got: {body}"
        );
    }
}

/// A file and a real `axum::Router` serving `handlers::update_metadata` for it.
async fn update_metadata_router() -> (axum::Router, Uuid) {
    use axum::Router;
    use axum::routing::patch;

    use file_storage::api::rest::handlers;

    let (svc, _psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap();

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{id}",
            patch(handlers::update_metadata),
        )
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(Arc::clone(&svc)));
    (router, ticket.file_id)
}

/// PATCHes `file_id` through `router` with an optional `If-Match-Metadata` header.
async fn update_metadata_via_router(
    router: &axum::Router,
    file_id: Uuid,
    if_match_header: Option<&str>,
) -> (StatusCode, String) {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let body = serde_json::json!({ "custom_metadata": { "k": "v" } });
    let mut req_builder = Request::builder()
        .method("PATCH")
        .uri(format!("/api/file-storage/v1/files/{file_id}"))
        .header("content-type", "application/json");
    if let Some(h) = if_match_header {
        req_builder = req_builder.header("if-match-metadata", h);
    }
    let req = req_builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = router.clone().oneshot(req).await.expect("router dispatch");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// `If-Match-Metadata` handling through the router: a present-but-unparseable header is a
/// validation error (not silently an unconditional patch), a stale well-formed one a precondition
/// failure (also HTTP 400, but a different violation), an absent one applies unconditionally.
#[tokio::test]
async fn patch_metadata_if_match_header_handling() {
    let (router, file_id) = update_metadata_router().await;

    let (status, body) = update_metadata_via_router(&router, file_id, Some("not-a-number")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "malformed: body {body}");
    assert!(
        body.contains("if-match-metadata"),
        "malformed: expected an if-match-metadata field violation in the body, got: {body}"
    );

    let (status, body) = update_metadata_via_router(&router, file_id, Some("999999")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "stale: PreconditionFailed maps to 400; body {body}"
    );
    assert!(
        body.contains("IF_MATCH"),
        "stale: expected a precondition (IF_MATCH) violation in the body, got: {body}"
    );

    let (status, body) = update_metadata_via_router(&router, file_id, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "absent: must apply unconditionally; body {body}"
    );
}

#[tokio::test]
async fn create_file_with_too_many_metadata_pairs_is_rejected() {
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(1),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![
        CustomMetadataEntry {
            key: "a".to_owned(),
            value: "1".to_owned(),
        },
        CustomMetadataEntry {
            key: "b".to_owned(),
            value: "2".to_owned(),
        },
    ];
    let err = svc.create_file(&ctx, nf, None, false).await.unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMetadataExceeded { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn update_metadata_over_limit_is_rejected_on_resulting_total() {
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(2),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "a".to_owned(),
        value: "1".to_owned(),
    }];
    let t = svc.create_file(&ctx, nf, None, false).await.unwrap();

    let patch = CustomMetadataPatch {
        entries: vec![
            ("b".to_owned(), Some("2".to_owned())),
            ("c".to_owned(), Some("3".to_owned())),
        ],
    };
    let err = svc
        .update_metadata(&ctx, t.file_id, patch, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMetadataExceeded { .. }),
        "got {err:?}"
    );

    let patch_ok = CustomMetadataPatch {
        entries: vec![("a".to_owned(), Some("9".to_owned()))],
    };
    svc.update_metadata(&ctx, t.file_id, patch_ok, None)
        .await
        .expect("replacing an existing key stays within the limit");
}

/// Quota cap 10 bytes, policy size cap 100: the create preflight of 100 busts the quota.
#[tokio::test]
async fn quota_exceeded_rejects_create_when_client_present() {
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(10));
    let (svc, psvc, _dp, _store) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(100),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "got {err:?}"
    );
}

/// Quota cap 100, size cap 60: create preflights 60, `presign_version` another 60 (120 > 100).
#[tokio::test]
async fn quota_gates_version_creation_not_just_first_upload() {
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(100));
    let (svc, psvc, _dp, _store) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());
    set_tenant_policy(
        &psvc,
        &ctx,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(60),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await;

    let t = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .expect("first create within quota");

    let err = svc.presign_version(&ctx, t.file_id).await.unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "version creation must also be quota-gated, got {err:?}"
    );
}

#[tokio::test]
async fn quota_client_error_fails_closed() {
    let quota: Arc<dyn QuotaClient> = Arc::new(ErroringQuota);
    let (svc, _psvc, _dp, _store) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InternalError),
        "quota client error must fail closed, got {err:?}"
    );
}

/// `dp.put_content` does the backend `put` and `finalize_upload` in one call.
#[tokio::test]
async fn no_policy_and_no_quota_is_fully_permissive() {
    let (svc, _psvc, dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());

    let mut nf = new_file(Uuid::now_v7(), "application/x-anything");
    nf.custom_metadata = (0..50)
        .map(|i| CustomMetadataEntry {
            key: format!("k{i}"),
            value: "v".repeat(100),
        })
        .collect();
    let t = svc
        .create_file(&ctx, nf, None, false)
        .await
        .expect("permissive create");

    let big = Bytes::from(vec![0u8; 1_000_000]);
    dp.put_content(&ctx, t.file_id, t.version_id, "application/x-anything", big)
        .await
        .expect("no size limit without policy");
}
