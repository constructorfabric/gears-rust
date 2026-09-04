//! Handler-level tests for the two response-shaping branches in
//! `src/api/rest/handlers.rs` that aren't exercised elsewhere:
//!
//!   - `complete_multipart`'s `Completed` (200, full `MultipartCompleteDto`)
//!     and `Completing` (202 + `Retry-After`) branches.
//!   - `finalize_version`'s auto-bind header mapping: `x-fs-bound: true` +
//!     `ETag` on a won CAS, `x-fs-bound: conflict` + `x-fs-current-etag` on
//!     a lost one.
//!
//! Calls the handler functions directly (mirroring `finalize_test.rs`'s own
//! style for the same handlers) rather than through an `axum::Router` —
//! `complete_multipart` already returns a plain `axum::response::Response`
//! and `finalize_version`'s `impl IntoResponse` converts to one with
//! `.into_response()`, so a full router adds no coverage here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use bytes::Bytes;
use sea_orm_migration::MigratorTrait;
use serde_json::Value;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::api::rest::handlers::{self, FinalizeAuth, FinalizeUploadReq};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::etag::etag_for;
use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

// -- shared test harness (copied/trimmed from api_handlers_test.rs / multipart_test.rs) --

async fn build_db() -> Arc<DBProvider<DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-complete-bind-test-{}.db",
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

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .expect("ctx")
}

fn new_file() -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "complete-bind.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

fn backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
}

/// Build `FileService` + `MultipartService` sharing one store/backend, on a
/// caller-supplied `Issuer` (`finalize_version` tests below need a real
/// signed token verified by `svc.verifier()`, which derives from this same
/// issuer -- matching `finalize_test.rs::build_full_service_with_issuer`).
async fn build_env(
    issuer: Arc<Issuer>,
) -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn MultipartStore>,
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
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
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
        Arc::clone(&multipart_store),
        backends,
        authorizer,
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    (svc, msvc, multipart_store, backend, store)
}

/// Simulate the sidecar writing a part for a `multipart_native` backend
/// (copied from `multipart_test.rs::simulate_sidecar_put_part` -- separate
/// test crates don't share code).
async fn simulate_sidecar_put_part(
    store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &file_storage::domain::multipart::MultipartPlan,
    backend_path: &str,
    backend_handle: &str,
    part_number: u32,
    data: Bytes,
) {
    let part = plan
        .parts
        .iter()
        .find(|p| p.part_number == part_number)
        .unwrap_or_else(|| panic!("part {part_number} not in plan"));
    assert_eq!(
        data.len() as u64,
        part.size,
        "part {part_number}: size mismatch"
    );

    let (backend_etag, part_hash) = backend
        .upload_part(backend_path, backend_handle, part_number, part.offset, data)
        .await
        .expect("backend upload_part");

    let size = i64::try_from(part.size).unwrap();
    let now = time::OffsetDateTime::now_utc();
    let part_number_i32 = i32::try_from(part_number).unwrap();
    store
        .upsert_multipart_part(
            plan.upload_id,
            part_number_i32,
            &backend_etag,
            part_hash,
            size,
            now,
        )
        .await
        .unwrap();
}

/// Read an axum response body as parsed JSON (copied from
/// `api_handlers_test.rs::body_json`).
async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid JSON body")
}

// ── complete_multipart: Completed (200, full DTO) ───────────────────────────

/// A 2-part auto-bind upload, completed through the actual handler: covers
/// the `MultipartCompleteOutcome::Completed` branch and every field of
/// `MultipartCompleteDto`. Cross-checks each field against the persisted
/// version row / file row rather than hardcoding expected hash bytes, so the
/// test still passes if the hashing details change but fails if the
/// handler's field mapping (or the bind wiring) breaks.
#[tokio::test]
async fn complete_multipart_completed_returns_200_with_full_dto() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, multipart_store, backend, store) = build_env(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    // 5 MiB + 5 bytes at a 5 MiB preferred part size -> exactly 2 parts, so
    // the completion exercises the composite hash mode / manifest fields
    // (a single-part plan would leave `hash_mode`/`manifest` degenerate).
    let part_size = DEFAULT_MIN_PART_SIZE;
    let declared_size = part_size + 5;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            Some(part_size),
            None,
            true, // auto_bind
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 2, "plan must have exactly 2 parts");

    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let path = backend_path(file_id, plan.version_id);

    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &path,
        &session.backend_upload_handle,
        1,
        Bytes::from(vec![0u8; usize::try_from(part_size).unwrap()]),
    )
    .await;
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &path,
        &session.backend_upload_handle,
        2,
        Bytes::from_static(b"AAAAA"),
    )
    .await;

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

// ── complete_multipart: Completing (202 + Retry-After) ──────────────────────

/// A `complete` that races another caller's LIVE completion lease must
/// answer HTTP 202 with a JSON `{"state":"completing","retry_after_secs":N}`
/// body AND a `Retry-After` header carrying that same `N` -- the key
/// 202-polling contract. The lease is acquired directly through
/// `MultipartStore::acquire_multipart_complete_lease` (no sleep, no race):
/// this deterministically puts the session into `completing` with a lease
/// this test's own `complete` call cannot win.
#[tokio::test]
async fn complete_multipart_while_lease_held_returns_202_with_matching_retry_after() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, msvc, multipart_store, _backend, _store) = build_env(Arc::clone(&issuer)).await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            5,
            None,
            None,
            false,
        )
        .await
        .unwrap();

    // Another caller holds a live completion lease -- no part upload is
    // needed since the handler must answer 202 before ever inspecting parts.
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

// ── finalize_version: auto-bind header mapping ──────────────────────────────

/// `create_file(..., auto_bind: true)` mints a `bind_on_finalize` token; a
/// finalize that lands via the real handler (real signed token, real
/// verifier) must set `x-fs-bound: true` and an `ETag` matching the file's
/// new content etag.
#[tokio::test]
async fn finalize_version_bind_claim_won_sets_bound_header_and_etag() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, _msvc, _multipart_store, backend, store) = build_env(Arc::clone(&issuer)).await;
    let verifier = Arc::new(svc.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(None));
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
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    let mut headers = HeaderMap::new();
    headers.insert("x-fs-token", token.parse().expect("valid header value"));
    let req = FinalizeUploadReq {
        size: i64::try_from(bytes.len()).unwrap(),
        hash_hex: hex::encode(hash::sha256(&bytes)),
    };

    let resp = handlers::finalize_version(
        Extension(svc),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket.version_id)),
        headers,
        Json(req),
    )
    .await
    .expect("finalize with a winning bind claim must succeed")
    .into_response();

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
    assert_eq!(
        file.content_id,
        Some(ticket.version_id),
        "the bind claim must have actually bound the version"
    );
    let expected_etag = etag_for(&file).expect("bound file must have a content etag");
    assert_eq!(
        resp.headers()
            .get(header::ETAG)
            .expect("ETag header")
            .to_str()
            .unwrap(),
        expected_etag,
        "ETag header must be the file's new content etag"
    );
}

/// Two `bind_on_finalize` completions racing the same file's
/// `content_id IS NULL` CAS: the loser's finalize still succeeds (the bytes
/// are uploaded and the version becomes available) but the embedded bind
/// loses, so the handler must answer `x-fs-bound: conflict` +
/// `x-fs-current-etag` carrying the CURRENT (winner's) etag -- mirrors
/// `finalize_test.rs::finalize_bind_claim_lost_cas_reports_conflict` but
/// drives the loser through the actual REST handler.
#[tokio::test]
async fn finalize_version_bind_claim_lost_cas_reports_conflict_header() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let (svc, _msvc, _multipart_store, backend, store) = build_env(Arc::clone(&issuer)).await;
    let verifier = Arc::new(svc.verifier());
    let finalize_auth = Arc::new(FinalizeAuth::new(None));
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc.create_file(&ctx, new_file(), None, true).await.unwrap();

    // Winner: ordinary finalize + explicit bind (simulates the first
    // token's flow already having landed).
    let winner_bytes = Bytes::from_static(b"winner");
    let winner_path = backend_path(ticket.file_id, ticket.version_id);
    backend
        .put(&winner_path, winner_bytes.clone())
        .await
        .unwrap();
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

    // Loser: a second pending version on the SAME file, finalized through
    // the handler with a `bind_on_finalize` token whose `content_id IS NULL`
    // CAS can no longer win.
    let ticket2 = svc.presign_version(&ctx, ticket.file_id).await.unwrap();
    let loser_bytes = Bytes::from_static(b"loser!");
    let loser_path = backend_path(ticket.file_id, ticket2.version_id);
    backend.put(&loser_path, loser_bytes.clone()).await.unwrap();

    let claims = Claims {
        op: Op::Put,
        file_id: ticket.file_id,
        version_id: ticket2.version_id,
        backend_id: "mem".to_owned(),
        backend_path: loser_path,
        exp: time::OffsetDateTime::now_utc().unix_timestamp() + 3600,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: true,
    };
    let token = issuer
        .issue(claims, time::OffsetDateTime::now_utc())
        .expect("issue token");

    let mut headers = HeaderMap::new();
    headers.insert("x-fs-token", token.parse().expect("valid header value"));
    let req = FinalizeUploadReq {
        size: i64::try_from(loser_bytes.len()).unwrap(),
        hash_hex: hex::encode(hash::sha256(&loser_bytes)),
    };

    let file_before = store
        .get_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap()
        .expect("file");
    let winner_etag = etag_for(&file_before).expect("winner bind must have set a content etag");

    let resp = handlers::finalize_version(
        Extension(svc),
        Extension(verifier),
        Extension(finalize_auth),
        Path((ticket.file_id, ticket2.version_id)),
        headers,
        Json(req),
    )
    .await
    .expect("finalize itself succeeds -- only the embedded bind CAS is lost")
    .into_response();

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
    // Losing the bind CAS must not have moved the file off the winner.
    assert!(
        resp.headers().get(header::ETAG).is_none(),
        "a conflict response must not also carry an ETag header"
    );
}
