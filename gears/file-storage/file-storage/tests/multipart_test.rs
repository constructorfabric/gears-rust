#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::idempotency::compute_request_hash;
use file_storage::domain::multipart::{BindState, MultipartPlan, MultipartUploadState};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::domain::ports::{DataPlanePort, MultipartStore};
use file_storage::domain::service::FileService;
use file_storage::infra::backend::{InMemoryBackend, LocalFsBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::{HashMode, Manifest, ManifestEntry};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage_sdk::{CustomMetadataEntry, NewFile};

mod common;
use common::{
    CompleteOutcomeExt, CountingBackend, Stack, StackBuilder, count_files_rows, ctx, hex_encode,
    put_part,
};

async fn build_service() -> (Arc<FileService>, Arc<MultipartService>, DataPlaneService) {
    let s = StackBuilder::new("mp-test").build().await;
    (s.svc, s.msvc, s.dp)
}

async fn build_file_service_with_dsn() -> (Arc<FileService>, String) {
    let s = StackBuilder::new("mp-test").build().await;
    (s.svc, s.dsn)
}

/// Overwrites `request_hash` via raw SQL (stored records are immutable via the API) to
/// simulate a hash for another owner. `Uuid` columns are 16-byte BLOBs, hence `X'...'` literals.
async fn tamper_request_hash(
    dsn: &str,
    tenant_id: Uuid,
    owner_kind: &str,
    owner_id: Uuid,
    key: &str,
    request_hash: &[u8],
) {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let tenant_hex = hex_encode(tenant_id.as_bytes());
    let owner_hex = hex_encode(owner_id.as_bytes());
    let hash_hex = hex_encode(request_hash);
    let sql = format!(
        "UPDATE idempotency_keys SET request_hash = X'{hash_hex}' \
             WHERE tenant_id = X'{tenant_hex}' AND owner_kind = '{owner_kind}' \
             AND owner_id = X'{owner_hex}' AND idempotency_key = '{key}'"
    );
    let res = conn
        .execute_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("tamper request_hash");
    assert_eq!(
        res.rows_affected(),
        1,
        "tamper UPDATE must hit exactly the one row created by the test setup"
    );
}

fn new_file() -> NewFile {
    common::new_file("upload.bin")
}

/// The `fs-token` query value of a signed upload URL.
fn token_of(url: &str) -> &str {
    &url[url.find("fs-token=").expect("fs-token in URL") + "fs-token=".len()..]
}

/// A one-part (5 byte) session of `data` on `file_id`, its part uploaded, on `s`'s stack.
async fn session_with_part(
    s: &Stack,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: &'static [u8],
) -> MultipartPlan {
    common::session_with_part(
        &s.msvc,
        &s.multipart_store,
        &s.backend,
        ctx,
        file_id,
        data,
        false,
    )
    .await
}

/// Completes a fresh one-part session of `data` and binds it over `if_match`; returns the new ETag.
async fn complete_and_bind(
    s: &Stack,
    ctx: &SecurityContext,
    file_id: Uuid,
    data: &'static [u8],
    if_match: Option<&str>,
) -> String {
    let plan = session_with_part(s, ctx, file_id, data).await;
    s.msvc
        .complete_multipart_upload(ctx, file_id, plan.upload_id, None)
        .await
        .unwrap();
    let bound = s
        .svc
        .bind(ctx, file_id, plan.version_id, if_match)
        .await
        .unwrap();
    file_storage::domain::etag::etag_for(&bound).expect("etag after bind")
}

#[tokio::test]
async fn multipart_happy_path_in_memory() {
    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        backend,
        dp,
        msvc,
        multipart_store,
        store,
        svc,
        ..
    } = s;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size = 13u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();

    assert_eq!(plan.parts.len(), 1, "13 bytes fits in one part");
    assert!(!plan.upload_id.is_nil());

    let p = &plan.parts[0];
    assert_eq!(p.part_number, 1);
    assert_eq!(p.offset, 0);
    assert_eq!(p.size, declared_size);
    assert!(!p.upload_url.is_empty());

    // The session records the backend and path of the pending version it assembles.
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("pending version must exist");
    assert_eq!(
        session.backend_id.as_deref(),
        Some(version.backend_id.as_str())
    );
    assert_eq!(session.backend_path, version.backend_path);

    let data = Bytes::from_static(b"Hello, World!");
    put_part(&multipart_store, &backend, &plan, ticket.file_id, 1, data).await;

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    svc.bind(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();

    let content = dp
        .read_content(&ctx, ticket.file_id, plan.version_id, None)
        .await
        .unwrap();
    assert_eq!(content, Bytes::from_static(b"Hello, World!"));

    svc.delete_file(&ctx, ticket.file_id, Some("*"))
        .await
        .expect("delete must succeed");
    assert!(
        matches!(
            svc.get_file(&ctx, ticket.file_id).await,
            Err(DomainError::FileNotFound { .. })
        ),
        "file must be FileNotFound after delete",
    );
}

const JPEG_MAGIC: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
];

/// Declared `image/png` but the parts assemble into a JPEG: version and session stay unchanged,
/// and the assembled object is left for the orphan sweep.
#[tokio::test]
async fn multipart_complete_rejects_content_not_matching_declared_mime() {
    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        backend,
        msvc,
        multipart_store,
        svc,
        ..
    } = s;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let declared_size = JPEG_MAGIC.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "image/png",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    put_part(
        &multipart_store,
        &backend,
        &plan,
        ticket.file_id,
        1,
        Bytes::from_static(JPEG_MAGIC),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MimeMismatch { .. }),
        "expected MimeMismatch, got {err:?}"
    );

    let version = multipart_store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Pending);
    assert_eq!(version.mime_type, "image/png");

    let session_after = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must still exist");
    assert_eq!(session_after.state, MultipartUploadState::InProgress);
    assert!(
        !session_after.mime_validated,
        "mime_validated must stay false when validation failed"
    );
}

/// `parts = ceil(size / part_size)`, the last part takes the remainder, sizes sum to the declared
/// size, and every part URL carries a verifiable `MultipartPart` token for its own plan entry.
/// Uses the minimum valid `preferred_part_size`.
#[tokio::test]
async fn initiate_returns_coherent_parts_plan_with_valid_part_tokens() {
    use file_storage::infra::signed_url::Op;

    let Stack {
        issuer, msvc, svc, ..
    } = StackBuilder::new("mp-test").build().await;
    let verifier = issuer.verifier();
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let part_size = 5 * 1024 * 1024u64; // DEFAULT_MIN_PART_SIZE
    let declared_size = 2 * part_size + 3; // plan: [part_size, part_size, 3]
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            Some(part_size),
            false,
        )
        .await
        .unwrap();

    assert!(!plan.upload_id.is_nil());
    assert_eq!(plan.parts.len(), 3);
    assert_eq!(plan.part_hash_algorithm, "SHA-256");

    let now = time::OffsetDateTime::now_utc();
    let mut total = 0u64;
    for (i, p) in plan.parts.iter().enumerate() {
        assert_eq!(
            p.part_number as usize,
            i + 1,
            "parts must be 1-based sequential"
        );
        assert_eq!(p.offset, total, "offset must be contiguous");
        assert!(p.size > 0, "part size must be positive");

        let claims = verifier
            .verify(token_of(&p.upload_url), now)
            .expect("token must verify");
        assert_eq!(claims.op, Op::MultipartPart, "part {}", p.part_number);
        assert_eq!(claims.file_id, ticket.file_id);
        assert_eq!(claims.version_id, plan.version_id);
        assert_eq!(claims.multipart.upload_id, plan.upload_id);
        assert_eq!(claims.multipart.part_number, p.part_number);
        assert_eq!(claims.multipart.offset, p.offset);
        assert_eq!(claims.multipart.size, p.size, "part {}", p.part_number);
        total += p.size;
    }
    assert_eq!(
        total, declared_size,
        "sum of part sizes must equal declared_size"
    );
}

fn max_size_claim(url: &str, verifier: &file_storage::infra::signed_url::Verifier) -> Option<u64> {
    let now = time::OffsetDateTime::now_utc();
    verifier
        .verify(token_of(url), now)
        .expect("token must verify")
        .upload
        .max_size
}

#[tokio::test]
async fn idempotency_replay_reflects_tightened_size_policy() {
    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        issuer, psvc, svc, ..
    } = s;
    let verifier = issuer.verifier();

    let ctx = ctx(Uuid::now_v7());

    psvc.set_policy(
        &ctx,
        PolicyScope::Tenant,
        None,
        PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(1024 * 1024),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        },
    )
    .await
    .unwrap();

    let nf = new_file();
    let key = "size-policy-replay-key".to_owned();
    let original = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    assert_eq!(
        max_size_claim(&original.upload_url, &verifier),
        Some(1024 * 1024),
        "the original ticket's token must carry the permissive policy's max_size"
    );

    // Tighten the policy to 10 bytes, then replay with the same key.
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

    let replayed = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();
    assert_eq!(replayed.file_id, original.file_id);
    assert_eq!(replayed.version_id, original.version_id);

    assert_eq!(
        max_size_claim(&replayed.upload_url, &verifier),
        Some(10),
        "a replay must re-mint the upload URL against the CURRENT (tightened) policy's \
         max_size, not silently replay the original ticket's now-stale, larger constraint"
    );
}

/// A replay of the same key with a different name or custom metadata is a `Conflict` and creates no
/// second file.
#[tokio::test]
async fn idempotency_replay_with_diverging_name_or_metadata_returns_conflict() {
    let tag = |value: &str| {
        vec![CustomMetadataEntry {
            key: "tag".to_owned(),
            value: value.to_owned(),
        }]
    };
    let mut base = new_file();
    base.name = "original.bin".to_owned();
    base.custom_metadata = tag("a");
    let mut other_name = base.clone();
    other_name.name = "different.bin".to_owned();
    let mut other_metadata = base.clone();
    other_metadata.custom_metadata = tag("b");

    for (row, diverging) in [("name", other_name), ("metadata", other_metadata)] {
        let (svc, dsn) = build_file_service_with_dsn().await;
        let ctx = ctx(Uuid::now_v7());
        let key = format!("diverging-{row}-key");

        svc.create_file(&ctx, base.clone(), Some(key.clone()), false)
            .await
            .unwrap();
        let err = svc
            .create_file(&ctx, diverging, Some(key), false)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Conflict { .. }),
            "{row}: expected Conflict on a diverging {row}, got {err:?}"
        );
        assert_eq!(
            count_files_rows(&dsn).await,
            1,
            "{row}: a rejected replay must not create a second file"
        );
    }
}

/// Owner is part of the primary key, so a real owner change never finds the row; the stored hash is
/// tampered to exercise the owner leg of the comparison.
#[tokio::test]
async fn idempotency_replay_with_diverging_owner_returns_conflict() {
    let (svc, dsn) = build_file_service_with_dsn().await;
    let ctx = ctx(Uuid::now_v7());
    let key = "diverging-owner-key".to_owned();

    let nf = new_file();
    svc.create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    let other_owner = Uuid::now_v7();
    let tampered_hash = compute_request_hash(
        nf.owner_kind.as_str(),
        other_owner,
        &nf.name,
        &nf.gts_file_type,
        &nf.mime_type,
        &[],
    );
    tamper_request_hash(
        &dsn,
        ctx.subject_tenant_id(),
        nf.owner_kind.as_str(),
        nf.owner_id,
        &key,
        &tampered_hash,
    )
    .await;

    let err = svc
        .create_file(&ctx, nf, Some(key), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict when the stored hash reflects a different owner, got {err:?}"
    );
    assert_eq!(
        count_files_rows(&dsn).await,
        1,
        "a rejected replay must not create a second file"
    );
}

#[tokio::test]
async fn idempotency_different_owner_different_file() {
    let (svc, _msvc, _dp) = build_service().await;
    let tenant = Uuid::now_v7();
    let ctx_a = ctx(tenant);
    let ctx_b = ctx(tenant); // same tenant, different subject (different owner_id in NewFile)

    let key = "shared-key".to_owned();

    let mut nf_a = new_file();
    nf_a.owner_id = Uuid::now_v7();
    let mut nf_b = new_file();
    nf_b.owner_id = Uuid::now_v7(); // different owner_id

    let t_a = svc
        .create_file(&ctx_a, nf_a, Some(key.clone()), false)
        .await
        .unwrap();
    let t_b = svc
        .create_file(&ctx_b, nf_b, Some(key), false)
        .await
        .unwrap();

    assert_ne!(
        t_a.file_id, t_b.file_id,
        "different owners must get distinct files even with the same key"
    );
}

#[tokio::test]
async fn idempotency_expiry_creates_new_file() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;
    use toolkit_security::AccessScope;

    use file_storage::infra::storage::entity::idempotency_key::{Column, Entity};

    let Stack { db, svc, .. } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let nf = new_file();

    let key = "expiry-key".to_owned();
    let t1 = svc
        .create_file(&ctx, nf.clone(), Some(key.clone()), false)
        .await
        .unwrap();

    // Age the stored record past its expiry instead of waiting for the TTL.
    let conn = db.conn().expect("conn");
    let expired = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    Entity::update_many()
        .col_expr(Column::ExpiresAt, Expr::value(expired))
        .filter(Column::IdempotencyKey.eq(key.as_str()))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("expire the idempotency record");

    let t2 = svc.create_file(&ctx, nf, Some(key), false).await.unwrap();

    assert_ne!(
        t1.file_id, t2.file_id,
        "after expiry, the same key must create a new file"
    );
}

/// A declared size above the tenant's `max_bytes` is rejected at initiate, before any backend state
/// is created; one within it is accepted.
#[tokio::test]
async fn initiate_multipart_enforces_policy_max_bytes_on_declared_size() {
    for (max_bytes, declared_size, accepted) in [(10, 11, false), (100, 50, true)] {
        let Stack {
            msvc, psvc, svc, ..
        } = StackBuilder::new("mp-test").build().await;
        let ctx = ctx(Uuid::now_v7());
        psvc.set_policy(
            &ctx,
            PolicyScope::Tenant,
            None,
            PolicyBody {
                size_limits: SizeLimits {
                    max_bytes: Some(max_bytes),
                    ..SizeLimits::default()
                },
                ..PolicyBody::default()
            },
        )
        .await
        .unwrap();
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();

        let result = msvc
            .initiate_multipart_upload(
                &ctx,
                ticket.file_id,
                "application/octet-stream",
                declared_size,
                None,
                false,
            )
            .await;
        if accepted {
            result.unwrap_or_else(|e| panic!("{declared_size} <= {max_bytes}: got {e:?}"));
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, DomainError::PolicySizeExceeded { .. }),
                "{declared_size} > {max_bytes}: expected PolicySizeExceeded, got {err:?}"
            );
        }
    }
}

/// Absurd sizes must be rejected up front, before `compute_plan` could overflow or over-allocate.
#[tokio::test]
async fn initiate_multipart_rejects_absurd_sizes() {
    let (svc, msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    for (row, declared_size, preferred_part_size) in [
        ("preferred_part_size", 1024, Some(u64::MAX)),
        ("declared_size", u64::MAX, None),
    ] {
        let err = msvc
            .initiate_multipart_upload(
                &ctx,
                ticket.file_id,
                "application/octet-stream",
                declared_size,
                preferred_part_size,
                false,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "absurd {row}: expected Validation, got {err:?}"
        );
    }
}

/// Parts are reported through the real `report_multipart_part` handler. DB state is asserted
/// via the entity, not `list_multipart_parts` (the method under test). Declaring just over 2x
/// the minimum part size forces 3 parts [min, min, 3]; bytes are written only for MIME sniffing.
#[tokio::test]
async fn multipart_complete_uses_reported_parts_not_empty_list() {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;
    use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;
    use file_storage::infra::signed_url::Verifier;
    use file_storage::infra::storage::entity::multipart_upload_part;

    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        backend,
        db,
        issuer,
        msvc,
        store,
        svc,
        ..
    } = s;
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size = 2 * DEFAULT_MIN_PART_SIZE + 3;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        3,
        "declared_size = 2*min + 3 must plan exactly 3 parts"
    );

    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(
        "test-internal-secret".to_owned(),
        time::Duration::ZERO,
    ));

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report",
            post(handlers::report_multipart_part),
        )
        .layer(axum::Extension(Arc::clone(&verifier)))
        .layer(axum::Extension(finalize_auth))
        .layer(axum::Extension(Arc::clone(&msvc)));

    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    let backend_path = format!("/{}/{}", ticket.file_id, plan.version_id);

    let mut expected_total: i64 = 0;
    for part in &plan.parts {
        let token = token_of(&part.upload_url);

        let size = i64::try_from(part.size).unwrap();
        expected_total += size;

        backend
            .upload_part(
                &backend_path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                Bytes::from(vec![b'x'; usize::try_from(part.size).unwrap()]),
            )
            .await
            .expect("backend upload_part");

        let body = serde_json::json!({
            "backend_etag": format!("etag-{}", part.part_number),
            "hash_hex": hex::encode([u8::try_from(part.part_number % 256).unwrap(); 32]),
            "size": size,
        });

        let uri = format!(
            "/api/file-storage/v1/files/{}/versions/{}/multipart/{}/parts/{}/report",
            ticket.file_id, plan.version_id, plan.upload_id, part.part_number
        );
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-fs-token", token)
            .header("x-fs-internal-token", "test-internal-secret")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let resp = router.clone().oneshot(req).await.expect("router dispatch");
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "report_multipart_part must succeed for part {}",
            part.part_number
        );
    }

    msvc.complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap();

    let conn = db.conn().expect("conn");
    let rows = multipart_upload_part::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query multipart_upload_parts directly");
    assert_eq!(
        rows.len(),
        plan.parts.len(),
        "multipart_upload_parts must have exactly one row per reported part"
    );
    let db_total: i64 = rows.iter().map(|r| r.size).sum();
    assert_eq!(db_total, expected_total);

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(
        version.size, db_total,
        "completed version size must equal the sum of reported part sizes"
    );
}

/// The callback is token-authenticated, so the reported `size` must match `claims.multipart.size`;
/// no part row may be persisted for a forged size.
#[tokio::test]
async fn report_part_rejects_forged_size() {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    use toolkit_security::AccessScope;
    use tower::ServiceExt;

    use file_storage::api::rest::handlers;
    use file_storage::infra::signed_url::Verifier;
    use file_storage::infra::storage::entity::multipart_upload_part;

    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        db,
        issuer,
        msvc,
        svc,
        ..
    } = s;
    let verifier: Arc<Verifier> = Arc::new(issuer.verifier());

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let declared_size: u64 = 100;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        1,
        "small declared_size must plan one part"
    );
    let part = &plan.parts[0];
    let planned_size = i64::try_from(part.size).unwrap();

    let finalize_auth = Arc::new(handlers::FinalizeAuth::new(
        "test-internal-secret".to_owned(),
        time::Duration::ZERO,
    ));

    let router = Router::new()
        .route(
            "/api/file-storage/v1/files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report",
            post(handlers::report_multipart_part),
        )
        .layer(axum::Extension(Arc::clone(&verifier)))
        .layer(axum::Extension(finalize_auth))
        .layer(axum::Extension(Arc::clone(&msvc)));

    let token = token_of(&part.upload_url);

    let forged_size = planned_size + 1;
    let body = serde_json::json!({
        "backend_etag": "forged-etag",
        "hash_hex": hex::encode([7u8; 32]),
        "size": forged_size,
    });
    let uri = format!(
        "/api/file-storage/v1/files/{}/versions/{}/multipart/{}/parts/{}/report",
        ticket.file_id, plan.version_id, plan.upload_id, part.part_number
    );
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-fs-token", token)
        .header("x-fs-internal-token", "test-internal-secret")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = router.clone().oneshot(req).await.expect("router dispatch");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "a forged part size must be rejected"
    );

    let conn = db.conn().expect("conn");
    let rows = multipart_upload_part::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("query multipart_upload_parts directly");
    assert!(
        rows.is_empty(),
        "a rejected forged-size report must not persist any part row"
    );
}

/// Table-driven: a `local-fs` registry rejects initiate, a `memory` registry accepts it.
#[tokio::test]
async fn multipart_initiate_rejected_when_backend_not_multipart_native() {
    struct Case {
        name: &'static str,
        backend: fn() -> Arc<dyn StorageBackend>,
        backend_id: &'static str,
        expect_multipart_supported: bool,
    }

    let cases = [
        Case {
            name: "local-fs-only registry",
            backend: || {
                let tmp =
                    std::env::temp_dir().join(format!("cf-fs-mpn-{}", Uuid::now_v7().simple()));
                std::fs::create_dir_all(&tmp).expect("create tmp dir");
                Arc::new(LocalFsBackend::new("local-fs", tmp)) as Arc<dyn StorageBackend>
            },
            backend_id: "local-fs",
            expect_multipart_supported: false,
        },
        Case {
            name: "memory-only registry",
            backend: || Arc::new(InMemoryBackend::new("memory")) as Arc<dyn StorageBackend>,
            backend_id: "memory",
            expect_multipart_supported: true,
        },
    ];

    for case in cases {
        let backend = (case.backend)();
        let Stack { msvc, svc, .. } = StackBuilder::new("mp-test")
            .backends(vec![backend], case.backend_id)
            .build()
            .await;

        let ctx = ctx(Uuid::now_v7());
        let ticket = svc
            .create_file(&ctx, new_file(), None, false)
            .await
            .unwrap();

        let result = msvc
            .initiate_multipart_upload(
                &ctx,
                ticket.file_id,
                "application/octet-stream",
                1024,
                None,
                false,
            )
            .await;

        if case.expect_multipart_supported {
            assert!(
                result.is_ok(),
                "case '{}': expected multipart to be accepted, got {:?}",
                case.name,
                result.err()
            );
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, DomainError::MultipartNotSupported { .. }),
                "case '{}': expected MultipartNotSupported, got {err:?}",
                case.name
            );
        }
    }
}

#[tokio::test]
async fn complete_returns_version_size_and_composite_hash() {
    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        backend,
        msvc,
        multipart_store,
        store,
        svc,
        ..
    } = s;
    let ctx = ctx(Uuid::now_v7());

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let content = Bytes::from_static(b"Hello, World! This is plain text.");
    let declared_size = content.len() as u64;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "text/plain",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    put_part(
        &multipart_store,
        &backend,
        &plan,
        ticket.file_id,
        1,
        content.clone(),
    )
    .await;

    let completed = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    let expected_hash = hash::sha256(&content);

    assert_eq!(completed.version_id, plan.version_id);
    assert_eq!(completed.size, i64::try_from(declared_size).unwrap());
    assert_eq!(completed.hash_algorithm, "SHA-256");
    assert_eq!(completed.content_hash, expected_hash);
    assert_eq!(completed.hash_mode, HashMode::WholeSha256);
    assert_eq!(completed.part_count, 1);
    assert_eq!(completed.manifest, None);

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must exist");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Available);
    assert_eq!(version.mime_type, "text/plain");
    assert_eq!(version.hash_mode, HashMode::WholeSha256.as_str());
    assert_eq!(version.part_count, None);
    assert_eq!(version.hash_value, expected_hash);
    assert_eq!(
        store.get_version_manifest(plan.version_id).await.unwrap(),
        None,
        "a one-part completion must not persist a manifest row"
    );
    assert!(
        multipart_store
            .get_multipart_upload(plan.upload_id)
            .await
            .unwrap()
            .expect("session must still exist")
            .mime_validated,
        "mime_validated must be true after a successful complete"
    );
}

/// Bind A, then B (A's ETag is now stale); completing a third session with A's ETag must fail
/// before any session/version mutation.
#[tokio::test]
async fn complete_with_stale_if_match_is_rejected() {
    let s = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = s
        .svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;

    let etag_a = complete_and_bind(&s, &ctx, file_id, b"AAAAA", None).await;
    complete_and_bind(&s, &ctx, file_id, b"BBBBB", Some(&etag_a)).await;
    let plan_c = session_with_part(&s, &ctx, file_id, b"CCCCC").await;

    let err = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan_c.upload_id, Some(&etag_a))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "expected PreconditionFailed for a stale If-Match, got {err:?}"
    );

    let session_c_after = s
        .multipart_store
        .get_multipart_upload(plan_c.upload_id)
        .await
        .unwrap()
        .expect("session c must still exist");
    assert_eq!(
        session_c_after.state,
        MultipartUploadState::InProgress,
        "a rejected If-Match must not touch the session's state"
    );
}

/// `If-Match: *` skips the comparison even when the file has bound content.
#[tokio::test]
async fn complete_wildcard_if_match_succeeds() {
    let s = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = s
        .svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;

    complete_and_bind(&s, &ctx, file_id, b"AAAAA", None).await;
    let plan_b = session_with_part(&s, &ctx, file_id, b"BBBBB").await;

    let completed = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan_b.upload_id, Some("*"))
        .await
        .expect("If-Match: * must bypass the precondition check")
        .unwrap_completed();
    assert_eq!(completed.version_id, plan_b.version_id);
}

/// 3-part plan, only parts 1 and 3 reported: `complete` answers `MultipartPartsMissing [2]` without
/// reaching the backend's `complete_multipart`, introspection lists part 2 with its planned bounds
/// and a fresh URL, and once part 2 lands `complete` succeeds.
#[tokio::test]
async fn missing_part_is_reported_then_resumed_to_completion() {
    use file_storage::domain::multipart::DEFAULT_MIN_PART_SIZE;

    let counting = CountingBackend::new(Arc::new(InMemoryBackend::new("mem")));
    let backend: Arc<dyn StorageBackend> = Arc::clone(&counting) as Arc<dyn StorageBackend>;
    let Stack {
        msvc,
        multipart_store,
        svc,
        ..
    } = StackBuilder::new("mp-test")
        .backend(Arc::clone(&backend))
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;

    let declared_size = 2 * DEFAULT_MIN_PART_SIZE + 3;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        plan.parts.len(),
        3,
        "declared_size must plan exactly 3 parts"
    );
    let upload = |n: u32| {
        let size = usize::try_from(plan.parts[n as usize - 1].size).unwrap();
        put_part(
            &multipart_store,
            &backend,
            &plan,
            file_id,
            n,
            Bytes::from(vec![b'x'; size]),
        )
    };
    upload(1).await;
    upload(3).await;

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    match err {
        DomainError::MultipartPartsMissing { upload_id, missing } => {
            assert_eq!(upload_id, plan.upload_id);
            assert_eq!(missing, vec![2], "exactly part 2 must be reported missing");
        }
        other => panic!("expected MultipartPartsMissing, got {other:?}"),
    }
    assert_eq!(
        counting.complete_calls(),
        0,
        "a missing-parts rejection must never reach the backend's complete_multipart"
    );

    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    assert_eq!(status.upload_id, plan.upload_id);
    assert_eq!(status.version_id, plan.version_id);
    assert_eq!(
        status.state,
        MultipartUploadState::InProgress,
        "a rejected complete leaves the session in_progress"
    );
    assert_eq!(status.declared_size, declared_size);
    assert_eq!(status.part_size, plan.part_size);
    let received: Vec<u32> = status.received.iter().map(|r| r.part_number).collect();
    assert_eq!(received, vec![1, 3]);
    let [missing] = status.missing.as_slice() else {
        panic!("exactly part 2 is missing, got {:?}", status.missing);
    };
    assert_eq!(missing.part_number, 2);
    assert_eq!(missing.offset, plan.parts[1].offset);
    assert_eq!(missing.size, plan.parts[1].size);
    assert!(
        missing.upload_url.is_some(),
        "part 2 must have a fresh resume upload_url"
    );

    upload(2).await;
    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed.size, i64::try_from(declared_size).unwrap());
    assert_eq!(completed.part_count, 3);
}

/// A foreign `upload_id` is masked as not found, indistinguishable from a missing one.
#[tokio::test]
async fn introspect_foreign_upload_id_is_not_found() {
    let (svc, msvc, _dp) = build_service().await;
    let ctx = ctx(Uuid::now_v7());

    let ticket_a = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let ticket_b = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan_a = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket_a.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();

    let err = msvc
        .introspect_multipart_upload(&ctx, ticket_b.file_id, plan_a.upload_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartUploadNotFound { .. }),
        "expected MultipartUploadNotFound for a foreign upload_id, got {err:?}"
    );
}

/// Expired but still `in_progress` (no sweep ran): full accounting, but no resume URLs.
#[tokio::test]
async fn introspect_expired_session_returns_state_without_urls() {
    let s = StackBuilder::new("mp-test").build().await;
    let Stack {
        msvc, store, svc, ..
    } = s;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();

    // Backdate `expires_at`: session stays `in_progress` in the DB but is no longer resumable.
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let status = msvc
        .introspect_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();

    assert_eq!(status.state, MultipartUploadState::InProgress);
    assert_eq!(
        status.missing.len(),
        1,
        "the single-part plan has exactly one missing part"
    );
    for missing in &status.missing {
        assert!(
            missing.upload_url.is_none(),
            "an expired session must not mint a resume URL for part {}",
            missing.part_number
        );
    }
}

/// `build_service` that also exposes the `Store`, to read session/version rows back.
async fn build_service_with_store() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    DataPlaneService,
    Store,
) {
    let s = StackBuilder::new("mp-test").build().await;
    let dp = DataPlaneService::new(Arc::clone(&s.svc) as Arc<dyn DataPlanePort>);
    (s.svc, s.msvc, dp, s.store)
}

#[tokio::test]
async fn initiate_session_expiry_uses_dedicated_session_ttl_not_url_ttl() {
    let issuer = Arc::new(Issuer::generate(100_000).expect("issuer"));
    // url_ttl_secs = 60s (per-part URLs), session_ttl_secs = 3600s (60x longer) -- mirrors gear.rs
    // wiring `default_url_ttl_secs` vs the dedicated `multipart_session_ttl_secs`.
    let common::Stack {
        svc,
        msvc,
        multipart_store,
        ..
    } = common::build_stack_with("mp-test", Arc::clone(&issuer), 60, |m| {
        m.with_session_ttl_secs(3600)
    })
    .await;
    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();

    let before = time::OffsetDateTime::now_utc();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await
        .unwrap();
    let after = time::OffsetDateTime::now_utc();

    // The plan's own `expires_at` (per-part URL expiry) must reflect the short url_ttl_secs, not
    // the session TTL.
    assert!(
        plan.expires_at <= after + time::Duration::seconds(60 + 5),
        "plan.expires_at must use the short url_ttl_secs, got {} (now ~ {after})",
        plan.expires_at
    );

    // The persisted session row must use the much longer session_ttl_secs.
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session must exist");
    assert!(
        session.expires_at >= before + time::Duration::seconds(3600 - 5),
        "session.expires_at must be ~ now + session_ttl_secs (3600s), got {} (initiated ~ \
         {before})",
        session.expires_at
    );
    assert!(
        session.expires_at > plan.expires_at,
        "the session must outlive its own first batch of per-part URLs: session {} vs plan {}",
        session.expires_at,
        plan.expires_at
    );

    let introspect_started = time::OffsetDateTime::now_utc();
    let status = msvc
        .introspect_multipart_upload(&ctx, ticket.file_id, plan.upload_id)
        .await
        .unwrap();
    let missing = status
        .missing
        .first()
        .expect("single-part upload has exactly one missing part");
    let upload_url = missing
        .upload_url
        .as_deref()
        .expect("a live session must mint a resume URL");
    let verifier = issuer.verifier();
    let claims = verifier
        .verify(token_of(upload_url), time::OffsetDateTime::now_utc())
        .expect("resume token must verify");
    assert!(
        claims.exp <= (introspect_started + time::Duration::seconds(60 + 5)).unix_timestamp(),
        "resume token exp ({}) must be capped at now + url_ttl_secs (60s), not minted with the \
         session's long-lived expires_at",
        claims.exp
    );
    assert!(
        claims.exp < session.expires_at.unix_timestamp(),
        "resume token exp ({}) must be strictly less than the session's own expires_at ({}) -- \
         proving the URL TTL cap actually bites when it is much shorter than the session TTL",
        claims.exp,
        session.expires_at.unix_timestamp()
    );
}

#[tokio::test]
async fn abort_multipart_upload_uses_the_sessions_own_backend_when_version_is_already_gone() {
    let (mem_backend, alt_backend) = common::mem_and_alt_backends();
    let Stack {
        msvc, store, svc, ..
    } = StackBuilder::new("mp-test")
        .backends(vec![mem_backend, Arc::clone(&alt_backend)], "mem")
        .build()
        .await;

    let ctx = ctx(Uuid::now_v7());
    let file_id = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;
    let upload = common::start_raw_upload(&alt_backend, file_id).await;

    let now = time::OffsetDateTime::now_utc();
    let upload_id = Uuid::now_v7();
    store
        .create_multipart_upload(
            upload_id,
            file_id,
            upload.version_id,
            &upload.handle,
            "alt",
            &upload.backend_path,
            "application/octet-stream",
            0,
            0,
            false,
            now + time::Duration::hours(1),
            now,
        )
        .await
        .expect("insert session row");

    msvc.abort_multipart_upload(&ctx, file_id, upload_id)
        .await
        .expect("abort_multipart_upload");

    common::assert_raw_upload_aborted(&alt_backend, &upload).await;
}

#[tokio::test]
async fn initiate_multipart_upload_rejects_overflowing_session_ttl_instead_of_panicking() {
    let common::Stack { svc, msvc, .. } = common::build_stack_with(
        "mp-test",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        3600,
        |m| m.with_session_ttl_secs(i64::MAX),
    )
    .await;

    let ctx = ctx(Uuid::now_v7());
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .expect("create_file");

    let result = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            13,
            None,
            false,
        )
        .await;

    assert!(
        result.is_err(),
        "an overflowing session_ttl_secs must be rejected with a DomainError, not panic: \
         {result:?}"
    );
}

#[tokio::test]
async fn finalize_multipart_version_rejects_after_cleanup_aborts_completing_session() {
    use file_storage::domain::audit::{AuditEntry, AuditOperation};
    use file_storage::domain::ports::MultipartFinishSnapshot;
    use file_storage::infra::content::hash_mode::HashMode;
    use file_storage_sdk::VersionStatus;
    use toolkit_security::AccessScope;

    let (svc, msvc, _dp, store) = build_service_with_store().await;
    let tenant_id = Uuid::now_v7();
    let ctx = ctx(tenant_id);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            10 * 1024 * 1024,
            Some(5 * 1024 * 1024),
            false,
        )
        .await
        .unwrap();

    let now = time::OffsetDateTime::now_utc();
    let acquired = store
        .acquire_multipart_complete_lease(
            plan.upload_id,
            "stale-completer",
            now - time::Duration::seconds(1),
            now,
        )
        .await
        .unwrap();
    assert!(
        acquired,
        "a fresh in_progress session must accept the lease"
    );

    let abort_audit = AuditEntry::success(
        tenant_id,
        "system",
        Uuid::nil(),
        Some(ticket.file_id),
        AuditOperation::MultipartAbort,
        serde_json::json!({"reason": "expired_multipart_session_cleanup"}),
    );
    let aborted = store
        .abort_multipart_upload(plan.upload_id, abort_audit)
        .await
        .unwrap();
    assert!(
        aborted,
        "cleanup's abort must win the CAS while the lease is expired"
    );

    let finalize_audit = AuditEntry::success(
        tenant_id,
        "user",
        ctx.subject_id(),
        Some(ticket.file_id),
        AuditOperation::FinalizeVersion,
        serde_json::json!({"version_id": plan.version_id, "upload_id": plan.upload_id}),
    );
    let session_audit = AuditEntry::success(
        tenant_id,
        "user",
        ctx.subject_id(),
        Some(ticket.file_id),
        AuditOperation::MultipartComplete,
        serde_json::json!({"upload_id": plan.upload_id}),
    );
    let err = store
        .finalize_multipart_version(
            ticket.file_id,
            None,
            Some("application/octet-stream".to_owned()),
            finalize_audit,
            None,
            MultipartFinishSnapshot {
                upload_id: plan.upload_id,
                version_id: plan.version_id,
                size: 10 * 1024 * 1024,
                content_hash: vec![0u8; 32],
                hash_mode: HashMode::WholeSha256,
                part_count: None,
                session_audit,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict once the session is aborted, got {err:?}"
    );

    let version = store
        .get_version(ticket.file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version row must still exist");
    assert_eq!(
        version.status,
        VersionStatus::Pending,
        "the version must stay pending -- the finalize must have rolled back entirely"
    );

    let file = store
        .require_file(&AccessScope::allow_all(), ticket.file_id)
        .await
        .unwrap();
    assert!(
        file.content_id.is_none(),
        "content_id must remain unbound -- the rejected finalize must not have auto-bound"
    );

    let session_after = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session row must still exist");
    assert_eq!(
        session_after.state,
        MultipartUploadState::Aborted,
        "the session must remain aborted -- the rejected finalize must not have resurrected it"
    );
}

#[allow(clippy::type_complexity)]
async fn build_redesign_env() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn MultipartStore>,
    Arc<dyn StorageBackend>,
    Store,
    SecurityContext,
) {
    let s = StackBuilder::new("mp-test").build().await;
    (
        s.svc,
        s.msvc,
        s.multipart_store,
        s.backend,
        s.store,
        ctx(Uuid::now_v7()),
    )
}

/// A one-part (5 byte) session with its part already uploaded, on a fresh stack.
async fn ready_single_part_session(
    auto_bind: bool,
) -> (
    Arc<MultipartService>,
    Arc<dyn MultipartStore>,
    Store,
    SecurityContext,
    Uuid,
    MultipartPlan,
) {
    let (svc, msvc, multipart_store, backend, store, ctx) = build_redesign_env().await;
    let file_id = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;
    let plan = common::session_with_part(
        &msvc,
        &multipart_store,
        &backend,
        &ctx,
        file_id,
        b"AAAAA",
        auto_bind,
    )
    .await;
    (msvc, multipart_store, store, ctx, file_id, plan)
}

/// Plants a completion lease held by `owner` that expires `secs_from_now` seconds from now
/// (negative = already expired: a completer that died mid-assembly).
async fn plant_lease(
    multipart_store: &Arc<dyn MultipartStore>,
    upload_id: Uuid,
    owner: &str,
    secs_from_now: i64,
) {
    let now = time::OffsetDateTime::now_utc();
    let acquired = multipart_store
        .acquire_multipart_complete_lease(
            upload_id,
            owner,
            now + time::Duration::seconds(secs_from_now),
            now,
        )
        .await
        .unwrap();
    assert!(acquired);
}

async fn bound_content_id(store: &Store, file_id: Uuid) -> Option<Uuid> {
    store
        .get_file(&toolkit_security::AccessScope::allow_all(), file_id)
        .await
        .unwrap()
        .expect("file")
        .content_id
}

#[tokio::test]
async fn manual_session_complete_does_not_bind() {
    let (msvc, _ms, store, ctx, file_id, plan) = ready_single_part_session(false).await;
    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Manual);
    assert_eq!(completed.etag, None);
    assert_eq!(
        bound_content_id(&store, file_id).await,
        None,
        "manual complete must not bind"
    );
}

/// Takeover: the previous completer died mid-assembly (state stuck in `completing`, lease expired).
#[tokio::test]
async fn complete_takes_over_expired_lease_and_finishes() {
    let (msvc, ms, store, ctx, file_id, plan) = ready_single_part_session(true).await;
    plant_lease(&ms, plan.upload_id, "dead-completer", -5).await;

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("takeover after an expired lease must succeed")
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Bound);
    assert_eq!(
        bound_content_id(&store, file_id).await,
        Some(completed.version_id)
    );
    let version = store
        .get_version(file_id, completed.version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, file_storage_sdk::VersionStatus::Available);
}

/// An expired session is reported as `expired` whether it is still `in_progress` or stuck in
/// `completing` with a dead completer's lease: it is never retried into `Completing`.
#[tokio::test]
async fn complete_on_expired_session_returns_expired() {
    for (row, dead_lease) in [("in_progress", false), ("completing", true)] {
        let (msvc, ms, store, ctx, file_id, plan) = ready_single_part_session(true).await;
        if dead_lease {
            plant_lease(&ms, plan.upload_id, "dead-completer", -5).await;
        }
        store
            .set_multipart_expires_at_for_test(
                plan.upload_id,
                time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
            )
            .await
            .unwrap();

        let err = msvc
            .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
            .await
            .expect_err("an expired session must never complete");
        match err {
            DomainError::MultipartUploadNotInProgress { state, .. } => {
                assert_eq!(
                    state, "expired",
                    "{row}: must report expired, not the raw state"
                );
            }
            other => panic!("{row}: expected MultipartUploadNotInProgress, got {other:?}"),
        }
    }
}

/// Two or more parts finalize as `multipart-composite-sha256`: root over the part manifest, which
/// is persisted and returned (a one-part plan is plain `whole-sha256`, see
/// `complete_returns_version_size_and_composite_hash`).
#[tokio::test]
async fn complete_two_parts_returns_composite_root_and_manifest() {
    let (svc, msvc, multipart_store, backend, store, ctx) = build_redesign_env().await;
    let file_id = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .unwrap()
        .file_id;
    let part = 5 * 1024 * 1024u64;
    let declared = part + 5;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared,
            Some(part),
            false,
        )
        .await
        .unwrap();
    assert_eq!(plan.parts.len(), 2);
    let body: Vec<u8> = (0..declared)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let split = usize::try_from(part).unwrap();
    for (n, chunk) in [(1u32, &body[..split]), (2, &body[split..])] {
        put_part(
            &multipart_store,
            &backend,
            &plan,
            file_id,
            n,
            Bytes::copy_from_slice(chunk),
        )
        .await;
    }

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    let expected = Manifest::new(vec![
        ManifestEntry {
            offset: 0,
            digest: hash::digest_to_array(hash::sha256(&body[..split])),
        },
        ManifestEntry {
            offset: part,
            digest: hash::digest_to_array(hash::sha256(&body[split..])),
        },
    ])
    .unwrap();
    assert_eq!(completed.hash_mode, HashMode::MultipartCompositeSha256);
    assert_eq!(completed.part_count, 2);
    assert_eq!(completed.content_hash, expected.root().to_vec());
    assert_eq!(completed.manifest, Some(expected.to_wire_string()));
    assert_eq!(
        store.get_version_manifest(plan.version_id).await.unwrap(),
        Some(expected.to_wire_string()),
        "the manifest row is persisted with the version"
    );
}
