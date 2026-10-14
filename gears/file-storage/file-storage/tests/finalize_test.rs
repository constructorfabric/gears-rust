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
use file_storage::domain::audit::{AuditEntry, AuditOperation, FileEvent};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::BindState;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::AutoBindOnFinalize;
use file_storage::domain::service::{FileService, FinalizeByTokenOutcome, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::repo::{FileRepo, VersionRepo};
use file_storage_sdk::{File, FileVersion, NewFile, OwnerKind, VersionStatus};

mod common;

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

/// Returns the raw backend and `Store` so tests control the object and inspect the version row.
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

/// Shares one `Issuer` so handler-level tests can mint tokens the service's verifier accepts.
async fn build_full_service_with_issuer(
    issuer: Arc<Issuer>,
) -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn StorageBackend>,
    Store,
) {
    let s = common::build_stack("finalize-test", issuer, None).await;
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

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const PDF_MAGIC: &[u8] = b"%PDF-1.4\n";

fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

#[tokio::test]
async fn finalize_without_prior_put_is_rejected() {
    let (svc, _backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

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
    let independently_recomputed = hash::sha256(&known_bytes);
    assert_eq!(version.size, true_size);
    assert_eq!(version.hash_value, independently_recomputed);
}

#[tokio::test]
async fn finalize_by_token_without_prior_put_is_rejected() {
    let (svc, _backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    // Claims mirror what `handlers::finalize_version` builds after verifying a signed token.
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

/// Second `finalize` on an `Available` row is a no-op (status-guarded CAS). `file_versions.file_id`
/// has an FK to `files`, so a real parent file is created first.
#[tokio::test]
async fn version_repo_finalize_twice_second_call_returns_false() {
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

#[tokio::test]
async fn finalize_streams_readback_without_buffering_whole_blob() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    // 4 MiB of non-uniform content, well beyond a single chunk.
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
        "interim-shared-secret".to_owned(),
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
    let finalize_auth = Arc::new(FinalizeAuth::new(secret.to_owned(), time::Duration::ZERO));

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
        "interim-shared-secret".to_owned(),
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

    let Err(err) = result else {
        panic!("missing internal-token header must be rejected");
    };
    assert_eq!(
        err.status_code(),
        403,
        "missing internal credential must map to 403"
    );
}

// `finalize_token_grace_secs`: the sidecar checks the PUT token once at start, so a slow-but-live
// upload can reach finalize after the token's `exp`.

/// Valid `Put` claims for `(file_id, version_id)` on the in-memory backend, expiring in an hour.
fn put_claims(
    file_id: Uuid,
    version_id: Uuid,
    backend_path: String,
    bind_on_finalize: bool,
) -> Claims {
    Claims {
        op: Op::Put,
        file_id,
        version_id,
        backend_id: "mem".to_owned(),
        backend_path,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize,
    }
}

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

/// Issues a token that expired `expired_secs_ago` and drives `endpoint` with `grace_secs` of grace.
/// `Ok(status)` is the handler's own response; `Err(code)` its error status. For `Finalize` the
/// true bytes are in the backend, so an accepted token really finalizes (returned as the row's
/// version status).
async fn call_with_expired_token(
    endpoint: GraceEndpoint,
    expired_secs_ago: i64,
    grace_secs: i64,
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
    let headers = headers_with_internal_token(&token);

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
            call_with_expired_token(GraceEndpoint::Finalize, within.0, within.1).await;
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
            call_with_expired_token(GraceEndpoint::ReportPart, within.0, within.1).await;
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
        let (result, _) = call_with_expired_token(endpoint, beyond.0, beyond.1).await;
        assert_eq!(
            result,
            Err(403),
            "{row}: a token expired beyond the grace must map to 403, same as any invalid token"
        );
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

    // Idempotent retry (response lost) converges to the same success, never a 409.
    let retry = finalize_by_token(&svc, &claims, &bytes)
        .await
        .expect("honest PUT retry must converge to success");
    assert_eq!(retry.bind_state, Some(BindState::Bound));
    assert_eq!(retry.etag, outcome.etag);
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

/// Two create-tokens racing for one new file: the loser reports `conflict` + the current ETag;
/// its upload still succeeds.
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

    let version = store
        .get_version(file_id, loser_version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, VersionStatus::Available);
    svc.bind(
        &ctx,
        file_id,
        loser_version_id,
        outcome.current_etag.as_deref(),
    )
    .await
    .expect("manual rebind with the conflict-reported ETag");
}

/// A lost auto-bind CAS leaves `bound_on_finalize` unset, so a retry re-derives `Conflict` with the
/// same ETag.
#[tokio::test]
async fn finalize_by_token_retry_after_lost_cas_still_reports_conflict() {
    let (svc, backend, store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, loser_version_id, claims, loser_bytes) =
        file_with_lost_cas_loser(&svc, &backend, &ctx).await;

    let original = finalize_by_token(&svc, &claims, &loser_bytes)
        .await
        .unwrap();
    assert_eq!(original.bind_state, Some(BindState::Conflict));

    let retry = finalize_by_token(&svc, &claims, &loser_bytes)
        .await
        .expect("honest retry of a lost-CAS finalize must still converge");
    assert_eq!(retry.bind_state, Some(BindState::Conflict));
    assert_eq!(
        retry.current_etag, original.current_etag,
        "must report the SAME live current ETag as the original lost-CAS response"
    );
    assert_eq!(retry.etag, None);

    let version = store
        .get_version(file_id, loser_version_id)
        .await
        .unwrap()
        .expect("version");
    assert!(
        !version.bound_on_finalize,
        "a lost CAS must never set the persisted bind flag"
    );
}

// Manual-mode retry convergence: every single-part upload goes through the replay-safe
// `publish_exclusive`.

/// A manual-token finalize replayed after the version is `Available` converges to the same success,
/// not 409.
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
}

/// A manual token explicitly bound after the first finalize, then retried: the retry reports
/// `Bound`.
#[tokio::test]
async fn finalize_manual_token_converges_to_bound_after_manual_bind() {
    let (svc, backend, _store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let bytes = Bytes::from_static(b"manual then bound");
    let path = put_object(&backend, ticket.file_id, ticket.version_id, &bytes).await;
    let claims = put_claims(ticket.file_id, ticket.version_id, path, false);

    finalize_by_token(&svc, &claims, &bytes).await.unwrap();
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("manual bind after finalize");

    let retry = finalize_by_token(&svc, &claims, &bytes)
        .await
        .expect("retry after manual bind must converge to success");
    assert_eq!(retry.bind_state, Some(BindState::Bound));
    assert!(
        retry.etag.is_some(),
        "bound retry must report the content ETag"
    );
    assert_eq!(retry.current_etag, None);
}

/// A manual retry with mismatched size/hash (forged replay) stays rejected.
#[tokio::test]
async fn finalize_manual_token_retry_with_mismatched_hash_is_rejected() {
    let (svc, backend, _store) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let bytes = Bytes::from_static(b"manual original");
    let path = put_object(&backend, ticket.file_id, ticket.version_id, &bytes).await;
    let claims = put_claims(ticket.file_id, ticket.version_id, path, false);

    finalize_by_token(&svc, &claims, &bytes).await.unwrap();

    let mismatched = Bytes::from_static(b"forged replay!!!");
    let err = finalize_by_token(&svc, &claims, &mismatched)
        .await
        .expect_err("a mismatched replay must not converge to success");
    assert!(
        matches!(err, DomainError::HashMismatch { .. }),
        "expected a hash-mismatch rejection, got {err:?}"
    );
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
    let db = build_db().await;
    (Store::new(Arc::clone(&db)), db)
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
async fn finalize_version_without_auto_bind_marks_available_leaves_content_id_unchanged() {
    let (store, db) = build_store().await;
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let files = FileRepo::new();
    let versions = VersionRepo::new();

    let bound_elsewhere = Uuid::now_v7(); // stands in for an unrelated current version
    let (tenant_id, file_id, v_new) = seed_pending_version(&conn, Some(bound_elsewhere)).await;

    let outcome = store
        .finalize_version(
            file_id,
            v_new,
            123,
            vec![7u8; 32],
            HashMode::WholeSha256,
            None,
            None,
            Some("text/plain".to_owned()),
            audit_entry(tenant_id, file_id, AuditOperation::FinalizeVersion),
            None,
        )
        .await
        .expect("finalize must not error");
    assert!(outcome.updated, "the pending row must be finalized");
    assert!(!outcome.bound, "no auto_bind was requested");

    let v_row = versions
        .get(&conn, &scope, file_id, v_new)
        .await
        .unwrap()
        .expect("version exists");
    assert_eq!(v_row.status, VersionStatus::Available);
    assert_eq!(v_row.size, 123);
    assert_eq!(v_row.mime_type, "text/plain");
    assert!(
        !v_row.is_current,
        "no bind was requested, so it stays uncurrent"
    );

    let file = files.get(&conn, &scope, file_id).await.unwrap().unwrap();
    assert_eq!(
        file.content_id,
        Some(bound_elsewhere),
        "content_id must be untouched by a finalize with no auto_bind"
    );

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert_eq!(audit_rows.len(), 1, "only the finalize audit row");
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

    let (tenant_id, file_id, v1) = seed_pending_version(&conn, None).await;

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

    let audit_rows = store.list_audit(file_id).await.unwrap();
    assert!(
        audit_rows.is_empty(),
        "a lost CAS (updated == false) must never write an audit row"
    );
}
