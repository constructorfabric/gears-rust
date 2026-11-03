#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use bytes::Bytes;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{BindState, MultipartPlan, MultipartUploadState};
use file_storage::domain::policy::{PolicyBody, PolicyScope, SizeLimits};
use file_storage::infra::backend::{InMemoryBackend, LocalFsBackend, StorageBackend};
use file_storage::infra::content::hash;
use file_storage::infra::content::hash_mode::{HashMode, Manifest, ManifestEntry};
use file_storage::infra::signed_url::{Issuer, Op};
use file_storage::infra::storage::Store;
use file_storage_sdk::VersionStatus;

mod common;
use common::{
    CompleteOutcomeExt, CountingBackend, MULTI_PART_DECLARED_SIZE, Stack, StackBuilder, bare_file,
    ctx, initiate, put_part,
};

const MIN_PART: u64 = 5 * 1024 * 1024;

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

/// Completes a fresh one-part session of `data` and binds it over `if_match`; returns the new `ETag`.
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

/// Declared `image/png` but the parts assemble into a JPEG: version and session stay unchanged,
/// and the assembled object is left for the orphan sweep.
#[tokio::test]
async fn multipart_complete_rejects_content_not_matching_declared_mime() {
    const JPEG_MAGIC: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00,
    ];
    let Stack {
        backend,
        msvc,
        multipart_store,
        svc,
        ..
    } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "image/png",
            JPEG_MAGIC.len() as u64,
            None,
            false,
        )
        .await
        .unwrap();
    put_part(
        &multipart_store,
        &backend,
        &plan,
        file_id,
        1,
        Bytes::from_static(JPEG_MAGIC),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MimeMismatch { .. }),
        "got {err:?}"
    );

    let version = multipart_store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Pending);
    assert_eq!(version.mime_type, "image/png");
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state, MultipartUploadState::InProgress);
    assert!(!session.mime_validated);
}

/// `parts = ceil(size / part_size)`, the last part takes the remainder, sizes sum to the declared
/// size, and every part URL carries a verifiable `MultipartPart` token for its own plan entry.
#[tokio::test]
async fn initiate_returns_coherent_parts_plan_with_valid_part_tokens() {
    let Stack {
        issuer, msvc, svc, ..
    } = StackBuilder::new("mp-test").build().await;
    let verifier = issuer.verifier();
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;

    let declared_size = 2 * MIN_PART + 3; // plan: [MIN_PART, MIN_PART, 3]
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            Some(MIN_PART),
            false,
        )
        .await
        .unwrap();

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
        let claims = verifier
            .verify(common::token_of(&p.upload_url), now)
            .expect("token must verify");
        assert_eq!(claims.op, Op::MultipartPart, "part {}", p.part_number);
        assert_eq!(claims.file_id, file_id);
        assert_eq!(claims.version_id, plan.version_id);
        assert_eq!(claims.multipart.upload_id, plan.upload_id);
        assert_eq!(claims.multipart.part_number, p.part_number);
        assert_eq!(claims.multipart.offset, p.offset);
        assert_eq!(claims.multipart.size, p.size, "part {}", p.part_number);
        total += p.size;
    }
    assert_eq!(total, declared_size);
    assert_eq!(plan.parts[2].size, 3);
}

/// A declared size above the tenant's `max_bytes` is rejected at initiate; one within it is accepted.
#[tokio::test]
async fn initiate_multipart_enforces_policy_max_bytes_on_declared_size() {
    for (max_bytes, declared_size, accepted) in [(10, 11, false), (100, 50, true)] {
        let Stack {
            msvc, psvc, svc, ..
        } = StackBuilder::new("mp-test").build().await;
        let ctx = ctx(Uuid::now_v7());
        let body = PolicyBody {
            size_limits: SizeLimits {
                max_bytes: Some(max_bytes),
                ..SizeLimits::default()
            },
            ..PolicyBody::default()
        };
        psvc.set_policy(&ctx, PolicyScope::Tenant, None, body)
            .await
            .unwrap();
        let file_id = bare_file(&svc, &ctx).await;

        let result = msvc
            .initiate_multipart_upload(
                &ctx,
                file_id,
                "application/octet-stream",
                declared_size,
                None,
                false,
            )
            .await;
        match (accepted, result) {
            (true, Ok(_)) | (false, Err(DomainError::PolicySizeExceeded { .. })) => {}
            (_, other) => panic!("{declared_size} vs max {max_bytes}: got {other:?}"),
        }
    }
}

/// A `local-fs` registry rejects initiate (no native multipart), a `memory` registry accepts it.
#[tokio::test]
async fn multipart_initiate_rejected_when_backend_not_multipart_native() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cases = [
        (
            "local-fs",
            Arc::new(LocalFsBackend::new("local-fs", tmp.path())) as Arc<dyn StorageBackend>,
            false,
        ),
        (
            "memory",
            Arc::new(InMemoryBackend::new("memory")) as Arc<dyn StorageBackend>,
            true,
        ),
    ];
    for (id, backend, supported) in cases {
        let Stack { msvc, svc, .. } = StackBuilder::new("mp-test")
            .backends(vec![backend], id)
            .build()
            .await;
        let ctx = ctx(Uuid::now_v7());
        let file_id = bare_file(&svc, &ctx).await;

        let result = msvc
            .initiate_multipart_upload(&ctx, file_id, "application/octet-stream", 1024, None, false)
            .await;
        match (supported, result) {
            (true, Ok(_)) | (false, Err(DomainError::MultipartNotSupported { .. })) => {}
            (_, other) => panic!("{id}: got {other:?}"),
        }
    }
}

/// A one-part completion is plain `whole-sha256` (no manifest row), validates the declared MIME and
/// finalizes the version the session recorded (same backend and path).
#[tokio::test]
async fn complete_single_part_returns_whole_sha256_and_validates_mime() {
    let Stack {
        backend,
        msvc,
        multipart_store,
        store,
        svc,
        ..
    } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let content = Bytes::from_static(b"Hello, World! This is plain text.");
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "text/plain",
            content.len() as u64,
            None,
            false,
        )
        .await
        .unwrap();
    let pending = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        session.backend_id.as_deref(),
        Some(pending.backend_id.as_str())
    );
    assert_eq!(session.backend_path, pending.backend_path);
    put_part(
        &multipart_store,
        &backend,
        &plan,
        file_id,
        1,
        content.clone(),
    )
    .await;

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    let expected_hash = hash::sha256(&content);
    assert_eq!(completed.version_id, plan.version_id);
    assert_eq!(completed.size, i64::try_from(content.len()).unwrap());
    assert_eq!(completed.hash_algorithm, "SHA-256");
    assert_eq!(completed.content_hash, expected_hash);
    assert_eq!(completed.hash_mode, HashMode::WholeSha256);
    assert_eq!(completed.part_count, 1);
    assert_eq!(completed.manifest, None);

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Available);
    assert_eq!(version.mime_type, "text/plain");
    assert_eq!(version.hash_mode, HashMode::WholeSha256.as_str());
    assert_eq!(version.hash_value, expected_hash);
    assert_eq!(
        store.get_version_manifest(plan.version_id).await.unwrap(),
        None
    );
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert!(session.mime_validated);
}

/// With A then B bound (A's `ETag` is stale), completing a third session with A's `ETag` fails
/// before any session/version mutation; the same session then completes with `If-Match: *`.
#[tokio::test]
async fn complete_rejects_stale_if_match_and_wildcard_bypasses_it() {
    let s = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&s.svc, &ctx).await;
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
        "got {err:?}"
    );
    let session = s
        .multipart_store
        .get_multipart_upload(plan_c.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state, MultipartUploadState::InProgress);

    let completed = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan_c.upload_id, Some("*"))
        .await
        .expect("If-Match: * must bypass the precondition check")
        .unwrap_completed();
    assert_eq!(completed.version_id, plan_c.version_id);
}

/// 3-part plan, only parts 1 and 3 reported: `complete` answers `MultipartPartsMissing [2]` without
/// reaching the backend's `complete_multipart`, introspection lists part 2 with its planned bounds
/// and a fresh URL, and once part 2 lands `complete` succeeds.
#[tokio::test]
async fn missing_part_is_reported_then_resumed_to_completion() {
    let counting = CountingBackend::new(Arc::new(InMemoryBackend::new("mem")));
    let backend: Arc<dyn StorageBackend> = Arc::clone(&counting) as Arc<dyn StorageBackend>;
    let Stack {
        msvc,
        multipart_store,
        svc,
        ..
    } = StackBuilder::new("mp-test")
        .backends(vec![Arc::clone(&backend)], "mem")
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, MULTI_PART_DECLARED_SIZE).await;
    assert_eq!(plan.parts.len(), 3);
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
    assert!(
        matches!(&err, DomainError::MultipartPartsMissing { missing, .. } if *missing == [2]),
        "got {err:?}"
    );
    assert_eq!(
        counting.complete_calls(),
        0,
        "must never reach the backend's complete_multipart"
    );

    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    assert_eq!(status.state, MultipartUploadState::InProgress);
    let received: Vec<u32> = status.received.iter().map(|r| r.part_number).collect();
    assert_eq!(received, vec![1, 3]);
    let [missing] = status.missing.as_slice() else {
        panic!("exactly part 2 is missing, got {:?}", status.missing);
    };
    assert_eq!(missing.part_number, 2);
    assert_eq!(
        (missing.offset, missing.size),
        (plan.parts[1].offset, plan.parts[1].size)
    );
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
    assert_eq!(
        completed.size,
        i64::try_from(MULTI_PART_DECLARED_SIZE).unwrap()
    );
    assert_eq!(completed.part_count, 3);
}

/// A foreign `upload_id` (another file's) and an unknown one are both reported as not found.
#[tokio::test]
async fn unknown_or_foreign_upload_id_is_not_found() {
    let Stack { svc, msvc, .. } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_a = bare_file(&svc, &ctx).await;
    let file_b = bare_file(&svc, &ctx).await;
    let plan_a = initiate(&msvc, &ctx, file_a, 13).await;

    let err = msvc
        .introspect_multipart_upload(&ctx, file_b, plan_a.upload_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartUploadNotFound { .. }),
        "got {err:?}"
    );

    let bogus = Uuid::now_v7();
    let err = msvc
        .complete_multipart_upload(&ctx, file_a, bogus, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::MultipartUploadNotFound { upload_id } if upload_id == bogus),
        "got {err:?}"
    );
}

/// Expired but still `in_progress` (no sweep ran): full accounting, but no resume URLs.
#[tokio::test]
async fn introspect_expired_session_returns_state_without_urls() {
    let Stack {
        msvc, store, svc, ..
    } = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, 13).await;
    store
        .set_multipart_expires_at_for_test(
            plan.upload_id,
            time::OffsetDateTime::now_utc() - time::Duration::hours(1),
        )
        .await
        .unwrap();

    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    assert_eq!(status.state, MultipartUploadState::InProgress);
    let [missing] = status.missing.as_slice() else {
        panic!("the single-part plan has exactly one missing part");
    };
    assert!(
        missing.upload_url.is_none(),
        "an expired session must not mint a resume URL"
    );
}

/// The session lives for `session_ttl_secs` (3600s), while the plan's part URLs and the resume
/// URLs minted by introspect are capped at the much shorter `url_ttl_secs` (60s).
#[tokio::test]
async fn initiate_session_ttl_is_independent_of_url_ttl() {
    let issuer = Arc::new(Issuer::generate(100_000).expect("issuer"));
    let Stack {
        svc,
        msvc,
        multipart_store,
        ..
    } = StackBuilder::new("mp-test")
        .issuer(Arc::clone(&issuer))
        .url_ttl_secs(60)
        .tweak_msvc(|m| m.with_session_ttl_secs(3600))
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;

    let before = time::OffsetDateTime::now_utc();
    let plan = initiate(&msvc, &ctx, file_id, 13).await;
    assert!(
        plan.expires_at <= time::OffsetDateTime::now_utc() + time::Duration::seconds(65),
        "plan.expires_at must use the short url_ttl_secs, got {}",
        plan.expires_at
    );
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        session.expires_at >= before + time::Duration::seconds(3595),
        "session.expires_at must be ~ now + session_ttl_secs, got {}",
        session.expires_at
    );

    let status = msvc
        .introspect_multipart_upload(&ctx, file_id, plan.upload_id)
        .await
        .unwrap();
    let upload_url = status.missing[0]
        .upload_url
        .as_deref()
        .expect("a live session mints a resume URL");
    let claims = issuer
        .verifier()
        .verify(
            common::token_of(upload_url),
            time::OffsetDateTime::now_utc(),
        )
        .expect("resume token must verify");
    assert!(
        claims.exp
            <= (time::OffsetDateTime::now_utc() + time::Duration::seconds(65)).unix_timestamp(),
        "resume token exp ({}) must be capped at now + url_ttl_secs",
        claims.exp
    );
    assert!(claims.exp < session.expires_at.unix_timestamp());
}

/// Abort goes to the backend recorded on the session, even when the version row is gone.
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
    let file_id = bare_file(&svc, &ctx).await;
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
        .expect("abort");

    common::assert_raw_upload_aborted(&alt_backend, &upload).await;
}

#[tokio::test]
async fn initiate_multipart_rejects_overflowing_session_ttl() {
    let Stack { svc, msvc, .. } = StackBuilder::new("mp-test")
        .tweak_msvc(|m| m.with_session_ttl_secs(i64::MAX))
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&svc, &ctx).await;

    let err = msvc
        .initiate_multipart_upload(&ctx, file_id, "application/octet-stream", 13, None, false)
        .await
        .expect_err("an overflowing session_ttl_secs must be rejected, not panic");
    assert!(matches!(err, DomainError::Database { .. }), "got {err:?}");
}

/// Cleanup aborts a `completing` session whose lease expired; a finalize racing it then loses the
/// session CAS and rolls back entirely (version pending, file unbound, session stays aborted).
#[tokio::test]
async fn finalize_multipart_version_rejects_after_cleanup_aborts_completing_session() {
    use file_storage::domain::audit::{AuditEntry, AuditOperation};
    use file_storage::domain::ports::MultipartFinishSnapshot;
    use toolkit_security::AccessScope;

    let Stack {
        svc,
        msvc,
        multipart_store,
        store,
        ..
    } = StackBuilder::new("mp-test").build().await;
    let tenant_id = Uuid::now_v7();
    let ctx = ctx(tenant_id);
    let file_id = bare_file(&svc, &ctx).await;
    let plan = initiate(&msvc, &ctx, file_id, 2 * MIN_PART).await;
    common::plant_lease(&multipart_store, plan.upload_id, "stale-completer", -1).await;

    let audit = |actor: &str, subject: Uuid, op, detail| {
        AuditEntry::success(tenant_id, actor, subject, Some(file_id), op, detail)
    };
    let aborted = store
        .abort_multipart_upload(
            plan.upload_id,
            audit(
                "system",
                Uuid::nil(),
                AuditOperation::MultipartAbort,
                serde_json::json!({}),
            ),
        )
        .await
        .unwrap();
    assert!(
        aborted,
        "cleanup's abort must win the CAS while the lease is expired"
    );

    let user = ctx.subject_id();
    let err = store
        .finalize_multipart_version(
            file_id,
            None,
            Some("application/octet-stream".to_owned()),
            audit(
                "user",
                user,
                AuditOperation::FinalizeVersion,
                serde_json::json!({}),
            ),
            None,
            MultipartFinishSnapshot {
                upload_id: plan.upload_id,
                version_id: plan.version_id,
                size: i64::try_from(2 * MIN_PART).unwrap(),
                content_hash: vec![0u8; 32],
                hash_mode: HashMode::WholeSha256,
                part_count: None,
                session_audit: audit(
                    "user",
                    user,
                    AuditOperation::MultipartComplete,
                    serde_json::json!({}),
                ),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Conflict { .. }), "got {err:?}");

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        version.status,
        VersionStatus::Pending,
        "the finalize must roll back entirely"
    );
    let file = store
        .require_file(&AccessScope::allow_all(), file_id)
        .await
        .unwrap();
    assert!(
        file.content_id.is_none(),
        "the rejected finalize must not auto-bind"
    );
    let session = store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state, MultipartUploadState::Aborted);
}

/// A fresh stack with a bare file and a one-part (5 byte) session whose part is uploaded.
struct ReadySession {
    s: Stack,
    ctx: SecurityContext,
    file_id: Uuid,
    plan: MultipartPlan,
}

async fn ready_single_part_session() -> ReadySession {
    let s = StackBuilder::new("mp-test").build().await;
    let ctx = ctx(Uuid::now_v7());
    let file_id = bare_file(&s.svc, &ctx).await;
    let plan = common::session_with_part(
        &s.msvc,
        &s.multipart_store,
        &s.backend,
        &ctx,
        file_id,
        b"AAAAA",
        true,
    )
    .await;
    ReadySession {
        s,
        ctx,
        file_id,
        plan,
    }
}

/// Takeover: the previous completer died mid-assembly (state stuck in `completing`, lease expired).
#[tokio::test]
async fn complete_takes_over_expired_lease_and_finishes() {
    let ReadySession {
        s,
        ctx,
        file_id,
        plan,
    } = ready_single_part_session().await;
    common::plant_lease(&s.multipart_store, plan.upload_id, "dead-completer", -5).await;

    let completed = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("takeover after an expired lease must succeed")
        .unwrap_completed();
    assert_eq!(completed.bind_state, BindState::Bound);
    assert_eq!(
        common::content_id_of(&s.store, file_id).await,
        Some(completed.version_id)
    );
    let version = s
        .store
        .get_version(file_id, completed.version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(version.status, VersionStatus::Available);
}

/// An expired session is reported as `expired` whether it is still `in_progress` or stuck in
/// `completing` with a dead completer's lease: it is never retried into `Completing`.
#[tokio::test]
async fn complete_on_expired_session_returns_expired() {
    for (row, dead_lease) in [("in_progress", false), ("completing", true)] {
        let ReadySession {
            s,
            ctx,
            file_id,
            plan,
        } = ready_single_part_session().await;
        if dead_lease {
            common::plant_lease(&s.multipart_store, plan.upload_id, "dead-completer", -5).await;
        }
        s.store
            .set_multipart_expires_at_for_test(
                plan.upload_id,
                time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
            )
            .await
            .unwrap();

        let err = s
            .msvc
            .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
            .await
            .expect_err("an expired session must never complete");
        assert!(
            matches!(&err, DomainError::MultipartUploadNotInProgress { state, .. } if state == "expired"),
            "{row}: got {err:?}"
        );
    }
}

/// Two parts finalize as `multipart-composite-sha256`: root over the part manifest, persisted and
/// returned; completing issues no read of the assembled object, and the stored manifest lets a
/// client verify the content (a change in the first part or in the tail is detected).
#[tokio::test]
async fn complete_composite_returns_root_and_manifest_without_rereading() {
    let counting = CountingBackend::new(Arc::new(InMemoryBackend::new("mem")));
    let s = StackBuilder::new("mp-test")
        .backends(
            vec![Arc::clone(&counting) as Arc<dyn StorageBackend>],
            "mem",
        )
        .build()
        .await;
    let ctx = ctx(Uuid::now_v7());
    let (file_id, plan, full) = common::composite_session(&s, &ctx).await;

    let completed = s
        .msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(
        counting.reads(),
        0,
        "complete must not re-read the assembled object"
    );

    let split = usize::try_from(MIN_PART).unwrap();
    let digest = |bytes: &[u8]| hash::digest_to_array(hash::sha256(bytes));
    let expected = Manifest::new(vec![
        ManifestEntry {
            offset: 0,
            digest: digest(&full[..split]),
        },
        ManifestEntry {
            offset: MIN_PART,
            digest: digest(&full[split..]),
        },
    ])
    .unwrap();
    assert_eq!(completed.hash_mode, HashMode::MultipartCompositeSha256);
    assert_eq!(completed.part_count, 2);
    assert_eq!(completed.content_hash, expected.root().to_vec());
    assert_eq!(completed.manifest, Some(expected.to_wire_string()));
    let stored = s.store.get_version_manifest(plan.version_id).await.unwrap();
    assert_eq!(stored, Some(expected.to_wire_string()));

    let verify = |content: &[u8]| {
        Store::verify_content_hash(
            content,
            HashMode::MultipartCompositeSha256,
            &completed.content_hash,
            stored.as_deref(),
        )
    };
    verify(&full).expect("untampered content must verify");
    for (name, at) in [("first part", 10), ("tail", full.len() - 1)] {
        let mut tampered = full.clone();
        tampered[at] ^= 0xff;
        assert!(
            matches!(verify(&tampered), Err(DomainError::HashMismatch { .. })),
            "a change in the {name} must fail verification"
        );
    }
}
