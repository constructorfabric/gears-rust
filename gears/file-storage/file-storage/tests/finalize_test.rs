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

use async_trait::async_trait;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
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
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, PublishOutcome, StorageBackend,
};
use file_storage::infra::content::hash;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::VersionRepo;
use file_storage_sdk::{ByteRange, FileVersion, NewFile, OwnerKind, VersionStatus};

mod common;
use common::write_all;

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-finalize-test-{}.db",
        Uuid::now_v7().simple()
    ));
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
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
/// directly control what's stored via `write_all`/`get_stream`) and the
/// `Store` (for direct DB assertions on the version row).
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

/// Build a bare `FileService` over a caller-supplied `Store`/`BackendRegistry`
/// and `issuer`, with no previous-signing-key configuration (`verifier()`
/// defaults to the issuer's own single current key). Used by the
/// `signing_key_seed` rotation tests below, which need TWO `FileService`
/// instances sharing the same store/backend (one "before rotation", one
/// "after") but each with its own issuer -- unlike
/// `build_full_service_with_issuer`, which always creates a brand-new DB.
fn service_over(
    store: Store,
    backends: BackendRegistry,
    authorizer: Arc<dyn file_storage::domain::authz::Authorizer>,
    issuer: Arc<Issuer>,
) -> Arc<FileService> {
    let cfg = ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    };
    Arc::new(FileService::new(
        store, backends, issuer, authorizer, cfg, None, None,
    ))
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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, Bytes::from_static(b"hello")).await;

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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, Bytes::from_static(b"hello")).await;

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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, known_bytes.clone()).await;

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
    // streaming the backend's actual bytes back and recomputing `sha256`
    // over them, rejecting a mismatch before anything is persisted, so this
    // independently recomputed hash must match the persisted value
    // regardless of which of the two (guaranteed-identical) values the
    // implementation happens to persist.
    let independently_recomputed = hash::sha256(&known_bytes);
    assert_eq!(version.size, true_size);
    assert_eq!(version.hash_value, independently_recomputed);
}

// -- 5. finalize_upload_by_token: no prior PUT is rejected -------------------

#[tokio::test]
async fn finalize_by_token_without_prior_put_is_rejected() {
    let (svc, _backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
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
        bound_on_finalize: false,
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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, known_bytes.clone()).await;

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
    write_all(&backend, &path, Bytes::from_static(PDF_MAGIC)).await;

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
        .create_file(
            &ctx,
            new_file_with_mime("image/png; charset=binary"),
            None,
            false,
        )
        .await
        .unwrap();

    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, Bytes::from_static(PNG_MAGIC)).await;

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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    // 4 MiB of non-trivial (non-all-zero) content — large enough that a
    // regression back to whole-blob buffering would still "work" here, but a
    // streaming implementation must produce the exact same size/hash as an
    // incremental hash over the same bytes.
    let large_bytes: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let large_bytes = Bytes::from(large_bytes);

    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, large_bytes.clone()).await;

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

// -- 10b. finalize_upload: mid-stream read-back errors are classified by kind

/// A `StorageBackend` wrapper whose `get_stream` reads the wrapped backend's
/// real object in full, then replays it as two chunks: the first half as
/// `Ok`, the second replaced by a single `io::Error` of a caller-chosen
/// `kind` -- modeling a backend/transport fault that surfaces partway
/// through finalize's read-back verification
/// (`read_back_and_hash_streaming`), after the object has already opened
/// successfully. Everything else delegates straight to `inner`.
struct MidReadFaultBackend {
    inner: Arc<dyn StorageBackend>,
    kind: std::io::ErrorKind,
}

#[async_trait]
impl StorageBackend for MidReadFaultBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put_stream(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<(u64, [u8; 32]), DomainError> {
        self.inner.put_stream(path, stream, max_size).await
    }
    async fn publish_exclusive(
        &self,
        path: &str,
        stream: BoxStream<'_, std::io::Result<Bytes>>,
        max_size: Option<u64>,
    ) -> Result<PublishOutcome, DomainError> {
        self.inner.publish_exclusive(path, stream, max_size).await
    }
    async fn get_stream(
        &self,
        path: &str,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        let mut real = self.inner.get_stream(path, expected_len).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = real.next().await {
            bytes.extend_from_slice(
                &chunk.map_err(|e| DomainError::backend(self.id(), e.to_string()))?,
            );
        }
        let half = bytes.len() >> 1;
        let kind = self.kind;
        let chunks: Vec<std::io::Result<Bytes>> = vec![
            Ok(Bytes::copy_from_slice(&bytes[..half])),
            Err(std::io::Error::new(
                kind,
                "simulated mid-stream fault (test)",
            )),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Option<Bytes>, DomainError> {
        self.inner.read_prefix(path, max_bytes).await
    }
    async fn get_range_stream(
        &self,
        path: &str,
        range: ByteRange,
        expected_len: u64,
    ) -> Result<BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_range_stream(path, range, expected_len).await
    }
    async fn size(&self, path: &str) -> Result<u64, DomainError> {
        self.inner.size(path).await
    }
    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }
    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }
    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }
}

/// A transient (`TimedOut`) mid-stream read-back fault during finalize must
/// surface as a retryable `BackendUnavailable`, not the old always-`Backend`
/// behavior, and must not persist the version as `available`.
#[tokio::test]
async fn finalize_readback_mid_stream_timeout_is_retryable_backend_unavailable() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);

    let plain_backends = BackendRegistry::new(vec![Arc::clone(&inner)], "mem").expect("registry");
    let plain_svc = service_over(
        store.clone(),
        plain_backends,
        Arc::clone(&authorizer),
        Arc::clone(&issuer),
    );

    let ctx = ctx(Uuid::now_v7());
    let ticket = plain_svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let path = backend_path(ticket.file_id, ticket.version_id);
    let content = Bytes::from_static(b"content read back mid-stream during finalize");
    write_all(&inner, &path, content.clone()).await;

    let faulty: Arc<dyn StorageBackend> = Arc::new(MidReadFaultBackend {
        inner: Arc::clone(&inner),
        kind: std::io::ErrorKind::TimedOut,
    });
    let faulty_backends = BackendRegistry::new(vec![faulty], "mem").expect("registry");
    let faulty_svc = service_over(store.clone(), faulty_backends, authorizer, issuer);

    let err = faulty_svc
        .finalize_upload(
            &ctx,
            ticket.file_id,
            ticket.version_id,
            i64::try_from(content.len()).unwrap(),
            hash::sha256(&content),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::BackendUnavailable { .. }),
        "a transient (TimedOut) mid-stream read-back error must be retryable, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(
        version.status,
        VersionStatus::Pending,
        "finalize must not persist on a failed read-back"
    );
}

/// A permanent (`InvalidData`) mid-stream read-back fault during finalize
/// must still surface as `Backend` (not retryable), unlike the transient
/// case above.
#[tokio::test]
async fn finalize_readback_mid_stream_invalid_data_is_permanent_backend_error() {
    let db = build_db().await;
    let store = Store::new(Arc::clone(&db));
    let inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);

    let plain_backends = BackendRegistry::new(vec![Arc::clone(&inner)], "mem").expect("registry");
    let plain_svc = service_over(
        store.clone(),
        plain_backends,
        Arc::clone(&authorizer),
        Arc::clone(&issuer),
    );

    let ctx = ctx(Uuid::now_v7());
    let ticket = plain_svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let path = backend_path(ticket.file_id, ticket.version_id);
    let content = Bytes::from_static(b"content read back mid-stream during finalize");
    write_all(&inner, &path, content.clone()).await;

    let faulty: Arc<dyn StorageBackend> = Arc::new(MidReadFaultBackend {
        inner: Arc::clone(&inner),
        kind: std::io::ErrorKind::InvalidData,
    });
    let faulty_backends = BackendRegistry::new(vec![faulty], "mem").expect("registry");
    let faulty_svc = service_over(store.clone(), faulty_backends, authorizer, issuer);

    let err = faulty_svc
        .finalize_upload(
            &ctx,
            ticket.file_id,
            ticket.version_id,
            i64::try_from(content.len()).unwrap(),
            hash::sha256(&content),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Backend { .. }),
        "a permanent (InvalidData) mid-stream read-back error must not be classified \
         as retryable, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(
        version.status,
        VersionStatus::Pending,
        "finalize must not persist on a failed read-back"
    );
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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

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
    let finalize_auth = Arc::new(FinalizeAuth::new(
        Some("interim-shared-secret".to_owned()),
        time::Duration::ZERO,
    ));
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
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, known_bytes.clone()).await;

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
    let finalize_auth = Arc::new(FinalizeAuth::new(
        Some(secret.to_owned()),
        time::Duration::ZERO,
    ));

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

// -- finalize_token_grace_secs: grace window on the s2s finalize/report-part
// callbacks' re-check of the signed PUT token's `exp` (see
// `FileStorageConfig::finalize_token_grace_secs`, `Verifier::verify_with_grace`).
// The sidecar checks the token once at PUT start and never again, so a
// slow-but-live upload can legitimately reach finalize after the token's
// `exp` with its bytes already durably written; these two tests exercise
// that scenario end to end through the real `finalize_version` handler.

#[tokio::test]
async fn finalize_with_expired_token_accepted_within_grace() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, _msvc, backend, store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, known_bytes.clone()).await;

    let true_size = i64::try_from(known_bytes.len()).unwrap();
    let true_hash = hash::sha256(&known_bytes);

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: path,
        // Expired 2 minutes ago -- simulates a slow-but-live PUT that took
        // longer than the token's TTL to finish.
        exp: time::OffsetDateTime::now_utc().unix_timestamp() - 120,
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
    // 1-hour grace comfortably covers the 2-minute-past-exp token above.
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::seconds(3600)));
    let headers = headers_with_token(&token);

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
        .expect("an expired-but-within-grace token must still be accepted by finalize")
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
        "finalize must have actually gone through for the within-grace expired token"
    );
}

#[tokio::test]
async fn finalize_with_expired_token_rejected_beyond_grace() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, _msvc, _backend, _store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(ticket.file_id, ticket.version_id),
        // Expired 2 hours ago -- well beyond the 1-minute grace configured
        // below, so this must still be rejected as expired.
        exp: time::OffsetDateTime::now_utc().unix_timestamp() - 7200,
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
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::seconds(60)));
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

    let Err(err) = result else {
        panic!("a token expired well beyond the configured grace must be rejected");
    };
    assert_eq!(
        err.status_code(),
        403,
        "an expired-beyond-grace token must map to 403, same as any other invalid token"
    );
}

// -- signing_key_seed rotation (thread #35): `FileService::verifier()` must
// keep accepting a token signed under a PREVIOUS seed once the control plane
// has restarted on a new one and `previous_signing_public_keys` names the old
// seed's public key -- otherwise every upload started before the restart
// fails its finalize/report-part callback the instant the control plane
// comes back up, even though the sidecar fleet (rolled out first, per
// docs/operations.md's Rotation procedure) still honours the client's
// in-flight signed URL.

#[tokio::test]
async fn finalize_accepts_token_signed_by_previous_key_after_rotation() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));

    // "Before rotation": the control plane is running on the OLD seed, and a
    // client starts an upload against it.
    let old_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_before = service_over(
        store.clone(),
        backends.clone(),
        Arc::clone(&authorizer),
        Arc::clone(&old_issuer),
    );
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc_before
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, known_bytes.clone()).await;

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
    // Minted under the OLD seed -- exactly what a real client's in-flight
    // upload would carry into the callback below.
    let token = old_issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    // "After rotation": a fresh control-plane instance on a NEW seed, with
    // the old seed's public key retained via `previous_signing_public_keys`
    // (docs/operations.md's Rotation procedure, step 3).
    let new_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_after = Arc::new(
        FileService::new(
            store.clone(),
            backends.clone(),
            Arc::clone(&new_issuer),
            authorizer,
            ServiceConfig {
                default_url_ttl_secs: 3600,
                sidecar_base_url: "http://sidecar.test".to_owned(),
                default_page_size: 50,
                max_page_size: 1000,
                idempotency_ttl_secs: 86400,
            },
            None,
            None,
        )
        .with_previous_signing_public_keys(vec![old_issuer.public_key()])
        .expect("a valid-length previous key must be accepted"),
    );

    let verifier = Arc::new(svc_after.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::ZERO));
    let headers = headers_with_token(&token);
    let req = FinalizeUploadReq {
        size: i64::try_from(known_bytes.len()).unwrap(),
        hash_hex: hex::encode(hash::sha256(&known_bytes)),
    };

    let result = finalize_version(
        Extension(Arc::clone(&svc_after)),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id)),
        headers,
        Json(req),
    )
    .await;

    let response = result
        .expect(
            "a finalize callback signed under a retained previous signing key must still verify",
        )
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
        "finalize must have actually gone through for the previous-key-signed token"
    );
}

/// The mirror-image negative case: without configuring
/// `previous_signing_public_keys`, the exact same old-seed-signed token must
/// still be rejected after rotation -- proving the acceptance above comes
/// from the configured previous key, not from some accidental widening of
/// what `verify` accepts.
#[tokio::test]
async fn finalize_rejects_token_signed_by_previous_key_without_rotation_config() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));

    let old_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_before = service_over(
        store.clone(),
        backends.clone(),
        Arc::clone(&authorizer),
        Arc::clone(&old_issuer),
    );
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc_before
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

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
    let token = old_issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    // Same rotation, but `previous_signing_public_keys` is left empty --
    // `svc_after.verifier()` accepts only the new issuer's current key.
    let new_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_after = service_over(store, backends, authorizer, new_issuer);

    let verifier = Arc::new(svc_after.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::ZERO));
    let headers = headers_with_token(&token);
    let req = FinalizeUploadReq {
        size: 5,
        hash_hex: hex::encode(hash::sha256(b"hello")),
    };

    let result = finalize_version(
        Extension(svc_after),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id)),
        headers,
        Json(req),
    )
    .await;

    let Err(err) = result else {
        panic!(
            "an old-seed-signed token must be rejected once the control plane has rotated \
             without retaining that key in previous_signing_public_keys"
        );
    };
    assert_eq!(err.status_code(), 403);
}

/// The grace window (`finalize_token_grace_secs`) must apply on top of a
/// previous-key acceptance too, not just on the current key: a slow-but-live
/// upload that straddles BOTH the token's own `exp` and a `signing_key_seed`
/// rotation must still finalize.
#[tokio::test]
async fn finalize_accepts_expired_previous_key_token_within_grace_after_rotation() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));

    let old_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_before = service_over(
        store.clone(),
        backends.clone(),
        Arc::clone(&authorizer),
        Arc::clone(&old_issuer),
    );
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc_before
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let known_bytes = Bytes::from_static(b"hello, world!");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, known_bytes.clone()).await;

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: path,
        // Expired 2 minutes ago -- the same "slow-but-live upload" scenario
        // as `finalize_with_expired_token_accepted_within_grace`, now
        // combined with a rotation that has already happened by the time the
        // callback lands.
        exp: time::OffsetDateTime::now_utc().unix_timestamp() - 120,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let token = old_issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    let new_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_after = Arc::new(
        FileService::new(
            store.clone(),
            backends,
            Arc::clone(&new_issuer),
            authorizer,
            ServiceConfig {
                default_url_ttl_secs: 3600,
                sidecar_base_url: "http://sidecar.test".to_owned(),
                default_page_size: 50,
                max_page_size: 1000,
                idempotency_ttl_secs: 86400,
            },
            None,
            None,
        )
        .with_previous_signing_public_keys(vec![old_issuer.public_key()])
        .expect("a valid-length previous key must be accepted"),
    );

    let verifier = Arc::new(svc_after.verifier());
    // 1-hour grace comfortably covers the 2-minute-past-exp token above.
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::seconds(3600)));
    let headers = headers_with_token(&token);
    let req = FinalizeUploadReq {
        size: i64::try_from(known_bytes.len()).unwrap(),
        hash_hex: hex::encode(hash::sha256(&known_bytes)),
    };

    let result = finalize_version(
        Extension(Arc::clone(&svc_after)),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id)),
        headers,
        Json(req),
    )
    .await;

    let response = result
        .expect("an expired-but-within-grace previous-key token must still be accepted")
        .into_response();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let version = store
        .get_version(ticket.file_id, ticket.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.status, VersionStatus::Available);
}

/// Minting is unaffected by a rotation's previous-key configuration: a fresh
/// upload token issued AFTER `with_previous_signing_public_keys` is only ever
/// signed with the current (new) key, never the retained old one.
#[tokio::test]
async fn signing_after_rotation_uses_current_key_only() {
    let db = build_db().await;
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    let store = Store::new(Arc::clone(&db));

    let old_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let new_issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let svc_after = Arc::new(
        FileService::new(
            store,
            backends,
            Arc::clone(&new_issuer),
            authorizer,
            ServiceConfig {
                default_url_ttl_secs: 3600,
                sidecar_base_url: "http://sidecar.test".to_owned(),
                default_page_size: 50,
                max_page_size: 1000,
                idempotency_ttl_secs: 86400,
            },
            None,
            None,
        )
        .with_previous_signing_public_keys(vec![old_issuer.public_key()])
        .expect("a valid-length previous key must be accepted"),
    );

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc_after
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    // Pull the freshly-minted PUT token back out of the upload URL's
    // `fs-token` query parameter.
    let token = ticket
        .upload_url
        .split("fs-token=")
        .nth(1)
        .expect("upload_url must carry an fs-token query parameter")
        .to_owned();

    let now = time::OffsetDateTime::now_utc();
    assert!(
        new_issuer.verifier().verify(&token, now).is_ok(),
        "a token minted after rotation must verify against the NEW issuer's own key"
    );
    assert!(
        old_issuer.verifier().verify(&token, now).is_err(),
        "a token minted after rotation must NOT verify against the OLD issuer's key -- \
         minting always uses the current key only, regardless of previous_signing_public_keys"
    );
}

#[tokio::test]
async fn report_part_with_expired_token_passes_verification_within_grace() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, _backend, _store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let upload_id = Uuid::now_v7();
    let part_number = 1u32;
    let claims = Claims {
        op: Op::MultipartPart,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(ticket.file_id, ticket.version_id),
        // Expired 2 minutes ago, well inside the 1-hour grace below.
        exp: time::OffsetDateTime::now_utc().unix_timestamp() - 120,
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
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::seconds(3600)));
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

    // The call still fails -- this test deliberately never opened a multipart
    // session for `upload_id` -- but it must fail for that reason, not as an
    // expired token. `403` is the token-rejection status the
    // beyond-grace test above asserts, so anything else means verification
    // accepted the expired-but-within-grace token and the handler moved on.
    if let Err(err) = result {
        assert_ne!(
            err.status_code(),
            403,
            "an expired-but-within-grace token must pass verification, not be rejected as expired"
        );
    }
}

#[tokio::test]
async fn report_part_with_expired_token_rejected_beyond_grace() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, _backend, _store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let upload_id = Uuid::now_v7();
    let part_number = 1u32;
    let claims = Claims {
        op: Op::MultipartPart,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: backend_path(ticket.file_id, ticket.version_id),
        // Expired 2 hours ago -- well beyond the 1-minute grace configured
        // below, so this must still be rejected as expired.
        exp: time::OffsetDateTime::now_utc().unix_timestamp() - 7200,
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
    let finalize_auth = Arc::new(FinalizeAuth::new(None, time::Duration::seconds(60)));
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

    let Err(err) = result else {
        panic!("a token expired well beyond the configured grace must be rejected");
    };
    assert_eq!(
        err.status_code(),
        403,
        "an expired-beyond-grace token must map to 403, same as any other invalid token"
    );
}

#[tokio::test]
async fn report_part_with_internal_secret_required_rejects_missing_header() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, _backend, _store) = build_full_service_with_issuer(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

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
    let finalize_auth = Arc::new(FinalizeAuth::new(
        Some("interim-shared-secret".to_owned()),
        time::Duration::ZERO,
    ));
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
    write_all(&backend, &path, bytes.clone()).await;

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
    assert!(
        outcome.etag.is_some(),
        "bound finalize must return the new ETag"
    );

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

/// A retry of an auto-bind finalize must replay the ORIGINAL call's own
/// `Bound` decision, not recompute it from the file's CURRENT content
/// pointer: if a legitimate, unrelated rebind moves the file's content to a
/// different version between the original finalize and a retry of its own
/// (still-valid) token -- e.g. the original response was lost in transit,
/// and by the time the client retries, a separate later upload has rebound
/// the file -- the retry must still report `Bound` + the ORIGINAL version's
/// ETag, exactly what the client already received and acted on, never
/// `Conflict` against the file's now-different live pointer.
#[tokio::test]
async fn finalize_by_token_retry_replays_bound_decision_despite_later_rebind() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    let bytes_a = Bytes::from_static(b"version A content");
    let path_a = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path_a, bytes_a.clone()).await;

    let claims_a = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        backend_id: "mem".to_owned(),
        backend_path: path_a,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: true,
    };
    let original = svc
        .finalize_upload_by_token(
            &claims_a,
            i64::try_from(bytes_a.len()).unwrap(),
            hash::sha256(&bytes_a),
        )
        .await
        .unwrap();
    assert_eq!(original.bind_state, Some(BindState::Bound));
    assert!(original.etag.is_some());

    // A legitimate, unrelated rebind moves the file's content elsewhere
    // BEFORE the client's retry arrives.
    let ticket_b = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    let bytes_b = Bytes::from_static(b"version B content");
    let path_b = backend_path(ticket.file_id, ticket_b.version_id);
    write_all(&backend, &path_b, bytes_b.clone()).await;
    svc.finalize_upload(
        &ctx,
        ticket.file_id,
        ticket_b.version_id,
        i64::try_from(bytes_b.len()).unwrap(),
        hash::sha256(&bytes_b),
    )
    .await
    .unwrap();
    // The file is already bound to version A (the original auto-bind CAS
    // won it), so rebinding to B needs A's own ETag as `If-Match`.
    svc.bind(
        &ctx,
        ticket.file_id,
        ticket_b.version_id,
        original.etag.as_deref(),
    )
    .await
    .unwrap();

    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap()
        .expect("file");
    assert_eq!(
        file.content_id,
        Some(ticket_b.version_id),
        "the file must now legitimately point at version B"
    );

    // The client retries the ORIGINAL (version A) token -- its first
    // response was lost in transit. It must still see `Bound` + version A's
    // own ETag, exactly what the original call already decided and
    // returned, not `Conflict` against version B's live pointer.
    let retry = svc
        .finalize_upload_by_token(
            &claims_a,
            i64::try_from(bytes_a.len()).unwrap(),
            hash::sha256(&bytes_a),
        )
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
    write_all(&backend, &path_a, winner_bytes.clone()).await;
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
    write_all(&backend, &path_b, loser_bytes.clone()).await;
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
        .expect("finalize itself succeeds \u{2014} only the bind CAS is lost");
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

/// Regression guard alongside
/// [`finalize_by_token_retry_replays_bound_decision_despite_later_rebind`]:
/// a version whose auto-bind CAS was LOST at the ORIGINAL finalize never
/// gets `bound_on_finalize` set, so its retry legitimately keeps re-deriving
/// `Conflict` from a live read (there is no won decision to replay) --
/// reporting the SAME current ETag as the original response, since nothing
/// rebound the file in between.
#[tokio::test]
async fn finalize_by_token_retry_after_lost_cas_still_reports_conflict() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    // Winner: ordinary finalize + bind (simulates a first token's flow).
    let winner_bytes = Bytes::from_static(b"winner");
    let path_a = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path_a, winner_bytes.clone()).await;
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

    // Loser: a second pending version, finalized under a bind claim whose
    // CAS can no longer win.
    let ticket2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    let loser_bytes = Bytes::from_static(b"loser!");
    let path_b = backend_path(ticket.file_id, ticket2.version_id);
    write_all(&backend, &path_b, loser_bytes.clone()).await;
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
    let original = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(loser_bytes.len()).unwrap(),
            hash::sha256(&loser_bytes),
        )
        .await
        .unwrap();
    assert_eq!(original.bind_state, Some(BindState::Conflict));

    let retry = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(loser_bytes.len()).unwrap(),
            hash::sha256(&loser_bytes),
        )
        .await
        .expect("honest retry of a lost-CAS finalize must still converge");
    assert_eq!(retry.bind_state, Some(BindState::Conflict));
    assert_eq!(
        retry.current_etag, original.current_etag,
        "must report the SAME live current ETag as the original lost-CAS response"
    );
    assert_eq!(retry.etag, None);

    let version = store
        .get_version(ticket.file_id, ticket2.version_id)
        .await
        .unwrap()
        .expect("version");
    assert!(
        !version.bound_on_finalize,
        "a lost CAS must never set the persisted bind flag"
    );
}

// ── t22: manual-mode (`bind_on_finalize: false`) retry convergence ──────────
//
// The sidecar publishes every single-part upload — auto-bind AND manual —
// through the same replay-safe `publish_exclusive`. A lost finalize response
// can therefore retry into an already-`Available` version regardless of bind
// mode, and the retry must converge to the same success, never a 409.

/// (a) A manual token's finalize, replayed with the same size/hash after the
/// version is already `Available`, converges to the same success instead of
/// falling into `finalize_version`'s CAS (which would 409 as "already
/// finalized"). `bind_state` is `Manual` (never bound), no `content_id`.
#[tokio::test]
async fn finalize_manual_token_converges_on_retry() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let bytes = Bytes::from_static(b"manual retry");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, bytes.clone()).await;

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

    let outcome = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(bytes.len()).unwrap(),
            hash::sha256(&bytes),
        )
        .await
        .unwrap();
    assert_eq!(outcome.bind_state, None, "manual mode requests no bind");

    // Simulate the finalize response getting lost: the sidecar retries with
    // the identical claims/size/hash.
    let retry = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(bytes.len()).unwrap(),
            hash::sha256(&bytes),
        )
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
    let file = store
        .get_file(&toolkit_security::AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap()
        .expect("file");
    assert_eq!(
        file.content_id, None,
        "manual mode must never auto-bind, even on retry"
    );
}

/// (b) A manual token's version, explicitly bound via `POST /files/{id}/bind`
/// after the first finalize, then retried: the retry must report the
/// now-current `Bound` state (not fall back to `Manual`), with the content
/// ETag and no new CAS.
#[tokio::test]
async fn finalize_manual_token_converges_to_bound_after_manual_bind() {
    let (svc, backend, _store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let bytes = Bytes::from_static(b"manual then bound");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, bytes.clone()).await;

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

    svc.finalize_upload_by_token(
        &claims,
        i64::try_from(bytes.len()).unwrap(),
        hash::sha256(&bytes),
    )
    .await
    .unwrap();

    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("manual bind after finalize");

    // Lost finalize response, retried after the manual bind landed.
    let retry = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(bytes.len()).unwrap(),
            hash::sha256(&bytes),
        )
        .await
        .expect("retry after manual bind must converge to success");
    assert_eq!(retry.bind_state, Some(BindState::Bound));
    assert!(
        retry.etag.is_some(),
        "bound retry must report the content ETag"
    );
    assert_eq!(retry.current_etag, None);
}

/// (c) A manual token's finalize retried with a mismatched size/hash (a
/// corrupted or forged replay, not an honest retry) must stay rejected, same
/// as the auto-bind fast path.
#[tokio::test]
async fn finalize_manual_token_retry_with_mismatched_hash_is_rejected() {
    let (svc, backend, _store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let bytes = Bytes::from_static(b"manual original");
    let path = backend_path(ticket.file_id, ticket.version_id);
    write_all(&backend, &path, bytes.clone()).await;

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

    svc.finalize_upload_by_token(
        &claims,
        i64::try_from(bytes.len()).unwrap(),
        hash::sha256(&bytes),
    )
    .await
    .unwrap();

    // Retry claims different bytes than what was actually stored/verified.
    let mismatched = Bytes::from_static(b"forged replay!!!");
    let err = svc
        .finalize_upload_by_token(
            &claims,
            i64::try_from(mismatched.len()).unwrap(),
            hash::sha256(&mismatched),
        )
        .await
        .expect_err("a mismatched replay must not converge to success");
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "expected a hash-mismatch rejection, got {err:?}"
    );
}
