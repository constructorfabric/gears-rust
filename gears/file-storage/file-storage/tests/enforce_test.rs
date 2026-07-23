//! End-to-end write-path enforcement tests (P2-M2) against a real temp-file
//! `SQLite` DB, the in-memory backend, the tenant-only authorizer, and (where
//! relevant) a mock `QuotaClient`. These prove the effective policy + quota
//! gates actually bite on the control-plane write path:
//!
//! - disallowed declared mime → reject
//! - oversized finalize → reject
//! - metadata over-limit → reject (create + update)
//! - quota exceeded → reject (create + version creation) when a client is wired
//! - permissive when no policy configured (P1 behaviour preserved)

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use axum::http::StatusCode;
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::policy::{MetadataLimits, PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::policy_service::PolicyService;
use file_storage::domain::ports::{DataPlanePort, PolicyStore};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::external_clients::{QuotaClient, QuotaDecision};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{CustomMetadataEntry, CustomMetadataPatch, NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A mock quota client that denies once the cumulative requested bytes exceed a
/// cap. Each `check_storage_quota` call counts as a request of `additional_bytes`.
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

/// A quota client that always fails (to verify fail-closed behaviour).
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

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-fs-enforce-{}.db", Uuid::now_v7().simple()));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
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

async fn build_service(
    quota: Option<Arc<dyn QuotaClient>>,
) -> (
    Arc<FileService>,
    Arc<PolicyService>,
    DataPlaneService,
    Store,
) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let policy_store: Arc<dyn PolicyStore> = Arc::new(store.clone());
    let store_handle = store.clone();
    let svc = Arc::new(FileService::new(
        store,
        backends,
        issuer,
        Arc::clone(&authorizer),
        cfg,
        quota,
        None,
    ));
    let dp = DataPlaneService::new(Arc::clone(&svc) as Arc<dyn DataPlanePort>);
    let psvc = Arc::new(PolicyService::new(policy_store, authorizer));
    (svc, psvc, dp, store_handle)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
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

// ── allowed-types-policy ────────────────────────────────────────────────────

#[tokio::test]
async fn create_file_with_disallowed_mime_is_rejected() {
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());

    // Tenant policy allows only image/*.
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            allowed_mime_types: vec!["image/*".to_owned()],
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    // text/plain is not allowed → reject.
    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PolicyMimeNotAllowed { .. }),
        "got {err:?}"
    );

    // image/png matches image/* → allowed.
    svc.create_file(&ctx, new_file(Uuid::now_v7(), "image/png"), None, false)
        .await
        .expect("image/png should be allowed");
}

// ── size-limits-policy ──────────────────────────────────────────────────────

#[tokio::test]
async fn finalize_oversized_upload_is_rejected() {
    let (svc, psvc, dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    // Tenant policy: global 10-byte cap.
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(10),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    // Finalize a 100-byte upload → exceeds the 10-byte policy ceiling.
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

    // A 5-byte finalize is within the ceiling. `finalize_upload` now
    // re-verifies the claimed size/hash against the real backend blob, so the
    // 5 bytes must actually be written first (`dp.put_content` does the
    // backend `put` + `finalize_upload` in one call, matching production).
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

// ── negative-size / malformed-hash-hex (P2 2.6) ─────────────────────────────
//
// Both were previously backstopped only by DB `CHECK` constraints
// (`file_versions.size >= 0`), so an obviously-invalid claim surfaced as a raw
// `DomainError::Database` -> 500 instead of a 400. `finalize_upload` /
// `finalize_upload_by_token` now reject `size < 0` on entry, before any
// policy/backend lookup or the P2 0.1 read-back; `handlers::finalize_version`
// now rejects a `hash_hex` that doesn't decode to exactly 32 bytes (SHA-256).

#[tokio::test]
async fn finalize_negative_size_is_rejected_with_400_not_500() {
    let (svc, _psvc, _dp, store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    let t = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    // `size: -1` must be rejected by the new entry guard, not by the
    // `file_versions` `CHECK (size >= 0)` constraint several steps later
    // (which would surface as `DomainError::Database` -> 500).
    let err = svc
        .finalize_upload(&ctx, t.file_id, t.version_id, -1, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { ref field, .. } if field == "size"),
        "got {err:?}"
    );

    // Secondary artifact: the version row must be untouched (still pending,
    // size/hash never written) -- the guard fires before any store call.
    let version = store
        .get_version(t.file_id, t.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Pending);
    assert_eq!(version.size, 0);

    // Identical guard on the token-authenticated sibling.
    let claims = file_storage::infra::signed_url::Claims {
        op: file_storage::infra::signed_url::Op::Put,
        file_id: t.file_id,
        version_id: t.version_id,
        backend_id: "mem".to_owned(),
        backend_path: format!("/{}/{}", t.file_id, t.version_id),
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: file_storage::infra::signed_url::UploadConstraints::default(),
        multipart: file_storage::infra::signed_url::MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let err = svc
        .finalize_upload_by_token(&claims, -1, vec![0u8; 32])
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
}

/// Drive `handlers::finalize_version` through a minimal real `axum::Router`
/// (mirrors the pattern used in `multipart_test.rs`'s
/// `multipart_complete_uses_reported_parts_not_empty_list`) with a `hash_hex`
/// of the given decoded byte length, returning the response status and body
/// text.
///
/// The length check lives only in the handler (per the plan's step 3: the
/// service methods' only real callers always compute a genuine 32-byte
/// SHA-256 digest), so exercising it means going through the HTTP boundary
/// rather than calling `finalize_upload`/`finalize_upload_by_token` directly.
async fn finalize_via_router_with_hash_len(hash_byte_len: usize) -> (StatusCode, String) {
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;
    use file_storage::infra::signed_url::Verifier;

    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![backend], "mem").expect("registry");
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    let store = Store::new(Arc::clone(&db));
    let svc = Arc::new(FileService::new(
        store,
        backends,
        Arc::clone(&issuer),
        authorizer,
        cfg,
        None,
        None,
    ));

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

    // P2 0.1 remaining: `finalize_version` now also requires a `FinalizeAuth`
    // extension. `None` reproduces this test's pre-existing behavior (no
    // internal-secret gate configured, token-only trust model).
    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(None));

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
async fn finalize_truncated_hash_hex_is_rejected() {
    // 16 bytes: valid hex, but not the 32 bytes a SHA-256 digest decodes to.
    let (status, body) = finalize_via_router_with_hash_len(16).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected 400, not a downstream 500; got {status} body {body}"
    );
    assert!(
        body.contains("hash_hex"),
        "expected a hash_hex field violation in the body, got: {body}"
    );
}

#[tokio::test]
async fn finalize_oversized_hash_hex_is_rejected() {
    // 48 bytes: valid hex, too long for a 32-byte SHA-256 digest.
    let (status, body) = finalize_via_router_with_hash_len(48).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected 400, not a downstream 500; got {status} body {body}"
    );
    assert!(
        body.contains("hash_hex"),
        "expected a hash_hex field violation in the body, got: {body}"
    );
}

// ── malformed If-Match-Metadata (P2 2.10) ───────────────────────────────────
//
// `handlers::update_metadata` used to collapse an unparseable
// `If-Match-Metadata` header to `None` via `.and_then(..).ok()`, which made
// the patch apply unconditionally -- exactly the clients that tried to use
// optimistic concurrency got silently downgraded to "no CAS at all" instead
// of a `400`. The handler now parses the header only when present, and a
// parse failure is a validation error.

/// Drive `handlers::update_metadata` through a minimal real `axum::Router`
/// (same pattern as `finalize_via_router_with_hash_len`), optionally setting
/// an `If-Match-Metadata` header, and return the response status and body
/// text.
async fn update_metadata_via_router(if_match_header: Option<&str>) -> (StatusCode, String) {
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::patch;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;

    let (svc, _psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    let owner = Uuid::now_v7();

    let ticket = svc
        .create_file(&ctx, new_file(owner, "text/plain"), None, false)
        .await
        .unwrap();

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{id}",
            patch(handlers::update_metadata),
        )
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(Arc::clone(&svc)));

    let body = serde_json::json!({ "custom_metadata": { "k": "v" } });
    let mut req_builder = Request::builder()
        .method("PATCH")
        .uri(format!("/api/file-storage/v1/files/{}", ticket.file_id))
        .header("content-type", "application/json");
    if let Some(h) = if_match_header {
        req_builder = req_builder.header("if-match-metadata", h);
    }
    let req = req_builder
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
async fn patch_metadata_malformed_if_match_returns_400() {
    let (status, body) = update_metadata_via_router(Some("not-a-number")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unparseable If-Match-Metadata must be a 400, not a silent unconditional patch; got {status} body {body}"
    );
    assert!(
        body.contains("if-match-metadata"),
        "expected an if-match-metadata field violation in the body, got: {body}"
    );
}

#[tokio::test]
async fn patch_metadata_absent_if_match_applies_unconditionally() {
    // Positive control: no header at all must still succeed (unconditional
    // patch remains valid -- only a *present-but-unparseable* header is
    // rejected).
    let (status, body) = update_metadata_via_router(None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an absent If-Match-Metadata must apply unconditionally; got {status} body {body}"
    );
}

#[tokio::test]
async fn patch_metadata_stale_if_match_returns_conflict() {
    // Existing CAS behaviour lock-in: a well-formed but stale version must
    // still be rejected via `DomainError::PreconditionFailed`, distinct from
    // the malformed-header `DomainError::Validation` case above. Per the 2.5
    // canonical-error-mapping guardrail (`error_mapping_test.rs`), both
    // variants happen to resolve to HTTP 400 -- there is no built-in 412 in
    // this taxonomy -- so the two are told apart by the response body's
    // violation payload (`IF_MATCH`/"revision changed" vs. the
    // `if-match-metadata` field violation used above), not by status code.
    let (status, body) = update_metadata_via_router(Some("999999")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a stale (but well-formed) If-Match-Metadata resolves to 400 per the PreconditionFailed mapping; got {status} body {body}"
    );
    assert!(
        body.contains("IF_MATCH") && body.contains("revision changed concurrently"),
        "expected a precondition (IF_MATCH) violation in the body, got: {body}"
    );
    assert!(
        !body.contains("if-match-metadata") && !body.contains("must be an integer version"),
        "a stale-version conflict must not be reported as a malformed-header validation error, got: {body}"
    );
}

#[tokio::test]
async fn create_file_bakes_max_size_into_upload_url() {
    // When a policy caps size, the signed URL carries the constraint so the
    // sidecar enforces mid-stream. We can't decode the opaque token here, but
    // the URL must still be issued (the gate did not reject create).
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(1_000_000),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();
    let t = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap();
    assert!(t.upload_url.contains("fs-token="));
}

// ── metadata-limits ─────────────────────────────────────────────────────────

#[tokio::test]
async fn create_file_with_too_many_metadata_pairs_is_rejected() {
    let (svc, psvc, _dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(1),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

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
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            metadata_limits: MetadataLimits {
                max_pairs: Some(2),
                ..MetadataLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    // Create with one entry (within the limit).
    let mut nf = new_file(Uuid::now_v7(), "text/plain");
    nf.custom_metadata = vec![CustomMetadataEntry {
        key: "a".to_owned(),
        value: "1".to_owned(),
    }];
    let t = svc.create_file(&ctx, nf, None, false).await.unwrap();

    // Patch adds two more keys → resulting total of 3 pairs > 2 → reject.
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

    // Patch that replaces the existing key keeps the total at 1 → allowed.
    let patch_ok = CustomMetadataPatch {
        entries: vec![("a".to_owned(), Some("9".to_owned()))],
    };
    svc.update_metadata(&ctx, t.file_id, patch_ok, None)
        .await
        .expect("replacing an existing key stays within the limit");
}

// ── storage-quota ───────────────────────────────────────────────────────────

#[tokio::test]
async fn quota_exceeded_rejects_create_when_client_present() {
    // Cap of 10 bytes; the policy caps size at 100, so each create preflights
    // 100 bytes → the first create busts the 10-byte quota.
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(10));
    let (svc, psvc, _dp, _store) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(100),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::QuotaExceeded { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn quota_gates_version_creation_not_just_first_upload() {
    // Cap of 100 bytes; policy caps size at 60. First create preflights 60
    // (allowed, total 60). presign_version preflights another 60 (total 120 >
    // 100) → version creation is denied, proving quota covers overwrites too.
    let quota: Arc<dyn QuotaClient> = Arc::new(CappedQuota::new(100));
    let (svc, psvc, _dp, _store) = build_service(Some(quota)).await;
    let ctx = ctx(Uuid::now_v7());
    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(60),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

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

    // No policy configured, but the quota client errors → fail closed (deny).
    let err = svc
        .create_file(&ctx, new_file(Uuid::now_v7(), "text/plain"), None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InternalError),
        "quota client error must fail closed, got {err:?}"
    );
}

// ── permissive when no policy ───────────────────────────────────────────────

#[tokio::test]
async fn no_policy_and_no_quota_is_fully_permissive() {
    let (svc, _psvc, dp, _store) = build_service(None).await;
    let ctx = ctx(Uuid::now_v7());

    // Any mime, any size finalize, any metadata — all accepted.
    let mut nf = new_file(Uuid::now_v7(), "application/x-anything");
    nf.custom_metadata = (0..50)
        .map(|i| CustomMetadataEntry {
            key: format!("k{i}"),
            value: "v".repeat(1000),
        })
        .collect();
    let t = svc
        .create_file(&ctx, nf, None, false)
        .await
        .expect("permissive create");

    // `finalize_upload` now re-verifies the claimed size/hash against the
    // real backend blob, so the large upload must actually be written first
    // (`dp.put_content` does the backend `put` + `finalize_upload` in one
    // call, matching production) — the policy-permissiveness assertion is
    // that no size cap rejects a 10MB upload absent a configured policy.
    let big = Bytes::from(vec![0u8; 10_000_000]);
    dp.put_content(&ctx, t.file_id, t.version_id, "application/x-anything", big)
        .await
        .expect("no size limit without policy");
}
