//! Tests for finalize-time server-side re-verification of size/hash (P2 0.1).
//!
//! Both finalize entry points — the user-context `finalize_upload` and the
//! token-authenticated `finalize_upload_by_token` — must never persist a
//! `size`/`hash_value` that was not independently derived from the bytes
//! actually present at the version's backend path. A finalize call for a
//! version with no prior successful `PUT`, or with a claimed size/hash that
//! doesn't match the real blob, must be rejected and must leave the version
//! row `pending`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::api::rest::handlers::{
    FinalizeAuth, FinalizeUploadReq, ReportPartReq, finalize_version, report_multipart_part,
};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::VersionRepo;
use file_storage_sdk::{FileVersion, NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-finalize-test-{}.db",
        Uuid::now_v7().simple()
    ));
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

/// Build `FileService` plus the raw `InMemoryBackend` handle (so tests can
/// directly control `backend.get`/`put`) and the `Store` (for direct DB
/// assertions on the version row).
async fn build_service() -> (Arc<FileService>, Arc<dyn StorageBackend>, Store) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
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
    let svc = Arc::new(FileService::new(
        store.clone(),
        backends,
        issuer,
        authorizer,
        cfg,
        None,
        None,
    ));
    (svc, backend, store)
}

/// Build `FileService` + `MultipartService` sharing one store/backend, using
/// a caller-supplied `Issuer` (P2 0.1 remaining handler-level tests below
/// need a *real* signed token — unlike the service-layer tests above, which
/// hand-build `Claims` and call `finalize_upload_by_token` directly,
/// bypassing token verification). `svc.verifier()` (mirroring
/// `handlers::finalize_version`'s own wiring in `routes.rs`) derives from
/// this same issuer, so a token it mints verifies correctly.
async fn build_full_service_with_issuer(
    issuer: Arc<Issuer>,
) -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn StorageBackend>,
    Store,
) {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
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
        store.clone(),
        backends.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::new(store.clone()) as Arc<dyn MultipartStore>,
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    (svc, msvc, backend, store)
}

/// Build an `x-fs-token`-only `HeaderMap` (no `x-fs-internal-token`).
fn headers_with_token(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-fs-token",
        token.parse().expect("token is a valid header value"),
    );
    headers
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file() -> NewFile {
    new_file_with_mime("application/octet-stream")
}

fn new_file_with_mime(mime_type: &str) -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "finalize.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: mime_type.to_owned(),
        custom_metadata: vec![],
    }
}

// Minimal PNG signature (8-byte magic) — recognized by `infer` as `image/png`.
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
// `%PDF-1.4` header — recognized by `infer` as `application/pdf`.
const PDF_MAGIC: &[u8] = b"%PDF-1.4\n";

/// The canonical backend path a pending version is created at
/// (mirrors `FileService::backend_path`, `pub(super)` so not directly
/// reachable from an external test crate).
fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

// -- 1. finalize_upload: no prior PUT is rejected ----------------------------

#[tokio::test]
async fn finalize_without_prior_put_is_rejected() {
    let (svc, _backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    // Nothing was ever `put` to the backend for this version.
    let err = svc
        .finalize_upload(&ctx, ticket.file_id, ticket.version_id, 100, vec![0u8; 32])
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
    assert_eq!(version.size, 0);
}

// -- 2. finalize_upload: size mismatch is rejected ---------------------------

#[tokio::test]
async fn finalize_size_mismatch_is_rejected() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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

// -- 3. finalize_upload: hash mismatch is rejected ---------------------------

#[tokio::test]
async fn finalize_hash_mismatch_is_rejected() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    backend
        .put(&path, Bytes::from_static(b"hello"))
        .await
        .unwrap();

    let err = svc
        .finalize_upload(&ctx, ticket.file_id, ticket.version_id, 5, vec![0u8; 32])
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "expected HashMismatch, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, VersionStatus::Pending);
}

// -- 4. finalize_upload: matching size+hash succeeds, persists read-back ----

#[tokio::test]
async fn finalize_matching_size_and_hash_succeeds() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

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
    // `finalize_upload` persists the caller's `hash_value` only after
    // `Store::verify_content_hash` has proven it byte-for-byte equal to
    // `sha256` of the read-back blob, so this independently recomputed hash
    // must match the persisted value regardless of which of the two
    // (guaranteed-identical) values the implementation happens to persist.
    let independently_recomputed = hash::sha256(&known_bytes);
    assert_eq!(version.size, true_size);
    assert_eq!(version.hash_value, independently_recomputed);
}

// -- 5. finalize_upload_by_token: no prior PUT is rejected -------------------

#[tokio::test]
async fn finalize_by_token_without_prior_put_is_rejected() {
    let (svc, _backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    // Hand-build claims mirroring how `handlers::finalize_version` constructs
    // them after verifying the signed token (op == Put, file/version match).
    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(ticket.file_id, ticket.version_id),
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };

    let err = svc
        .finalize_upload_by_token(&claims, 100, vec![0u8; 32])
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

// -- 6. VersionRepo::finalize: second call on an Available row is a no-op ---
// (P2 0.4 — status-guard CAS)

#[tokio::test]
async fn version_repo_finalize_twice_second_call_returns_false() {
    // `file_versions.file_id` carries a `REFERENCES files (file_id)` FK, so a
    // repo-level version row still needs a real parent file row. Build the db
    // directly (rather than via `build_service()`) so this test keeps its own
    // `DBProvider` handle for `.conn()`, and go through a `FileService` on the
    // same db just once to create that parent file row.
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
    let svc = FileService::new(store, backends, issuer, authorizer, cfg, None, None);

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();
    let file_id = ticket.file_id;

    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let repo = VersionRepo::new();

    let version_id = Uuid::now_v7();
    let pending = FileVersion {
        file_id,
        version_id,
        mime_type: "application/octet-stream".to_owned(),
        size: 0,
        hash_algorithm: "SHA-256".to_owned(),
        hash_value: vec![0u8; 32],
        hash_mode: "whole-sha256".to_owned(),
        part_count: None,
        status: VersionStatus::Pending,
        is_current: false,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(file_id, version_id),
        created_at: time::OffsetDateTime::now_utc(),
    };
    repo.insert(&conn, &scope, &pending)
        .await
        .expect("insert pending version");

    let hash_a = hash::sha256(b"first-call-bytes");
    let hash_b = hash::sha256(b"second-call-bytes");

    let first = repo
        .finalize(
            &conn,
            &scope,
            file_id,
            version_id,
            100,
            hash_a.clone(),
            "whole-sha256",
            None,
            None,
        )
        .await
        .expect("first finalize call");
    assert!(first, "first finalize call on a pending row must succeed");

    let second = repo
        .finalize(
            &conn,
            &scope,
            file_id,
            version_id,
            200,
            hash_b,
            "whole-sha256",
            None,
            None,
        )
        .await
        .expect("second finalize call");
    assert!(
        !second,
        "second finalize call on an already-Available row must be a no-op"
    );

    let row = repo
        .get(&conn, &scope, file_id, version_id)
        .await
        .expect("query version row")
        .expect("version row must still exist");
    assert_eq!(row.size, 100, "size must retain the FIRST call's value");
    assert_eq!(
        row.hash_value, hash_a,
        "hash must retain the FIRST call's value"
    );
    assert_eq!(row.status, VersionStatus::Available);
}

// -- 7. finalize_upload: already-finalized version yields Conflict (409) ----
// (P2 0.4 — distinguishes double-finalize from a genuinely missing row)

#[tokio::test]
async fn finalize_upload_after_already_available_returns_conflict() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, known_bytes.clone()).await.unwrap();

    let true_size = i64::try_from(known_bytes.len()).unwrap();
    let true_hash = hash::sha256(&known_bytes);

    // First finalize succeeds, version -> Available.
    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        true_size,
        true_hash.clone(),
    )
    .await
    .unwrap();

    // A second finalize call for the same version, replaying the SAME
    // (now-correct) size/hash so it clears the read-back checks and reaches
    // the repo-level CAS — which must reject it as a conflict, not silently
    // re-accept it. (A claim that doesn't match the real blob would instead
    // be rejected earlier by the size/hash read-back check in this same
    // function — that path is already covered by
    // `finalize_size_mismatch_is_rejected`/`finalize_hash_mismatch_is_rejected`
    // above; this test isolates the CAS guard specifically.)
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

    // The DB row must still hold the FIRST call's values.
    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(version.size, true_size);
    assert_eq!(version.hash_value, true_hash);
}

// -- 8. finalize_upload: content not matching the declared MIME is rejected -
// (P2 1.10 — declared MIME is validated against the read-back blob)

#[tokio::test]
async fn finalize_rejects_content_not_matching_declared_mime() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file_with_mime("image/png"), None, false)
        .await
        .unwrap();

    // Presigned/declared as `image/png`, but the bytes actually uploaded are
    // a recognizably different signature (PDF) — a policy-bypass attempt.
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

    // The version must NOT have been finalized: still pending, not available.
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

// -- 9. finalize_upload: matching content persists the validated MIME -------
// (P2 1.10 — positive control: stored mime_type is the sniffed/canonical
// type, not merely the client's literal declared string)

#[tokio::test]
async fn finalize_persists_validated_mime() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    // Declare with a `charset` parameter that the SNIFFED canonical type will
    // not carry, so a passing assertion on the stored value proves the
    // *validated* type was persisted rather than the raw declared string.
    let ticket = svc
        .create_file(&ctx, new_file_with_mime("image/png; charset=binary"), None, false)
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

// -- 10. finalize_upload: read-back streams a large object correctly --------
// (CodeRabbit follow-up — finalize's read-back must verify size/hash by
// streaming the object rather than buffering it whole; this exercises that
// path end-to-end with an object well beyond a single small chunk and
// asserts the persisted size/hash are exactly the ones an incremental
// SHA-256 over the real bytes would produce)

#[tokio::test]
async fn finalize_streams_readback_without_buffering_whole_blob() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    // 4 MiB of non-trivial (non-all-zero) content — large enough that a
    // regression back to whole-blob buffering would still "work" here, but a
    // streaming implementation must produce the exact same size/hash as an
    // incremental hash over the same bytes.
    let large_bytes: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let large_bytes = Bytes::from(large_bytes);

    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, large_bytes.clone()).await.unwrap();

    let true_size = i64::try_from(large_bytes.len()).unwrap();
    let true_hash = hash::sha256(&large_bytes);

    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        true_size,
        true_hash.clone(),
    )
    .await
    .unwrap();

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(version.size, true_size);
    assert_eq!(version.hash_value, true_hash);
}

// -- 11. handlers::finalize_version: internal-secret gate (P2 0.1 remaining) -
//
// These exercise the axum handler directly (unlike tests 1-10 above, which
// call `FileService`/`finalize_upload*` directly), so the interim
// gear-local shared-secret check added to `handlers::finalize_version` /
// `handlers::report_multipart_part` is actually on the call path.

#[tokio::test]
async fn finalize_with_internal_secret_required_rejects_missing_header() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, _msvc, _backend, _store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(ticket.file_id, ticket.version_id),
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    let verifier = Arc::new(svc.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(Some("interim-shared-secret".to_owned())));
    // Deliberately no `x-fs-internal-token` header.
    let headers = headers_with_token(&token);

    let req = FinalizeUploadReq {
        size: 5,
        hash_hex: hex::encode(hash::sha256(b"hello")),
    };

    let result = finalize_version(
        Extension(svc),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id)),
        headers,
        Json(req),
    )
    .await;

    // `impl IntoResponse` (the `Ok` side) isn't `Debug`, so `expect_err` can't
    // be used here — a `let...else` avoids it without matching manually.
    let Err(err) = result else {
        panic!("missing internal-token header must be rejected");
    };
    assert_eq!(
        err.status_code(),
        403,
        "missing internal credential must map to 403"
    );
}

#[tokio::test]
async fn finalize_with_internal_secret_required_accepts_matching_header() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, _msvc, backend, store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, known_bytes.clone()).await.unwrap();

    let true_size = i64::try_from(known_bytes.len()).unwrap();
    let true_hash = hash::sha256(&known_bytes);

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: path,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    let verifier = Arc::new(svc.verifier());
    let secret = "interim-shared-secret";
    let finalize_auth = Arc::new(FinalizeAuth::new(Some(secret.to_owned())));

    let mut headers = headers_with_token(&token);
    headers.insert(
        "x-fs-internal-token",
        secret.parse().expect("secret is a valid header value"),
    );

    let req = FinalizeUploadReq {
        size: true_size,
        hash_hex: hex::encode(&true_hash),
    };

    let result = finalize_version(
        Extension(Arc::clone(&svc)),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id)),
        headers,
        Json(req),
    )
    .await;

    let response = result
        .expect("matching internal-token header must be accepted")
        .into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(
        version.status,
        VersionStatus::Available,
        "finalize must have actually gone through once the internal credential matched"
    );
}

#[tokio::test]
async fn report_part_with_internal_secret_required_rejects_missing_header() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, _backend, _store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, false).await.unwrap();

    let upload_id = Uuid::now_v7();
    let part_number = 1u32;
    let claims = Claims {
        op: Op::MultipartPart,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(ticket.file_id, ticket.version_id),
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims {
            upload_id,
            part_number,
            offset: 0,
            size: 5,
            backend_handle: String::new(),
        },
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    let verifier = Arc::new(svc.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(Some("interim-shared-secret".to_owned())));
    // Deliberately no `x-fs-internal-token` header.
    let headers = headers_with_token(&token);

    let req = ReportPartReq {
        backend_etag: "etag-1".to_owned(),
        hash_hex: hex::encode(hash::sha256(b"hello")),
        size: 5,
    };

    let result = report_multipart_part(
        Extension(msvc),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id, upload_id, part_number)),
        headers,
        Json(req),
    )
    .await;

    // `impl IntoResponse` (the `Ok` side) isn't `Debug`, so `expect_err` can't
    // be used here — a `let...else` avoids it without matching manually.
    let Err(err) = result else {
        panic!("missing internal-token header must be rejected");
    };
    assert_eq!(
        err.status_code(),
        403,
        "missing internal credential must map to 403"
    );
}

// ── Upload-flow redesign: auto-bind on finalize (single-part, 2 requests) ────

use file_storage::domain::multipart::BindState;

/// (б) A `bind_on_finalize` token (minted by `POST /files` with the default
/// `bind: "auto"` for a NEW file) makes the finalize itself bind the first
/// content under a `content_id IS NULL` CAS — the whole upload is
/// `POST /files` + `PUT`, no separate bind request.
#[tokio::test]
async fn finalize_with_bind_claim_binds_first_content() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    let bytes = Bytes::from_static(b"auto-bind me");
    let path = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path, bytes.clone()).await.unwrap();

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: path,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: true,
    };
    let outcome = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(bytes.len()).unwrap(),
            hash::sha256(&bytes),
        )
        .await
        .unwrap();
    assert_eq!(outcome.bind_state, Some(BindState::Bound));
    assert!(outcome.etag.is_some(), "bound finalize must return the new ETag");

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap()
        .expect("file");
    assert_eq!(
        file.content_id,
        Some(ticket.version_id),
        "finalize with the bind claim must have bound the first content"
    );

    // Idempotent PUT retry (response lost): same claims, same size/hash →
    // converges to the SAME success (X-FS-Bound: true again), never a 409.
    let retry = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(bytes.len()).unwrap(),
            hash::sha256(&bytes),
        )
        .await
        .expect("honest PUT retry must converge to success");
    assert_eq!(retry.bind_state, Some(BindState::Bound));
    assert_eq!(retry.etag, outcome.etag);
}

/// Two create-tokens racing for the same new file: the second finalize loses
/// the `content_id IS NULL` CAS and reports `conflict` + the CURRENT ETag —
/// the upload itself succeeds (version available, manually bindable).
#[tokio::test]
async fn finalize_bind_claim_lost_cas_reports_conflict() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    // Winner: ordinary finalize + bind (simulates the first token's flow).
    let winner_bytes = Bytes::from_static(b"winner");
    let path_a = backend_path(ticket.file_id, ticket.version_id);
    backend.put(&path_a, winner_bytes.clone()).await.unwrap();
    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        i64::try_from(winner_bytes.len()).unwrap(),
        hash::sha256(&winner_bytes),
    )
    .await
    .unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .unwrap();

    // Loser: a second pending version on the same file, finalized under a
    // bind claim whose `content_id IS NULL` CAS can no longer win.
    let ticket2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    let loser_bytes = Bytes::from_static(b"loser!");
    let path_b = backend_path(ticket.file_id, ticket2.version_id);
    backend.put(&path_b, loser_bytes.clone()).await.unwrap();
    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket2.version_id,
        backend_id: "mem".to_owned(),
        backend_path: path_b,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: true,
    };
    let outcome = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(loser_bytes.len()).unwrap(),
            hash::sha256(&loser_bytes),
        )
        .await
        .expect("finalize itself succeeds — only the bind CAS is lost");
    assert_eq!(outcome.bind_state, Some(BindState::Conflict));
    assert!(
        outcome.current_etag.is_some(),
        "conflict must carry the CURRENT ETag for a manual rebind's If-Match"
    );
    assert_eq!(outcome.etag, None);

    // The upload was not wasted: the version is available and a manual bind
    // (with the reported current ETag) makes it live without a re-upload.
    let version = store
        .get_version(ticket.file_id, ticket2.version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, VersionStatus::Available);
    svc.bind(
        &ctx,
        ticket.file_id,
        ticket2.version_id,
        outcome.current_etag.as_deref(),
    )
    .await
    .expect("manual rebind with the conflict-reported ETag");
}
