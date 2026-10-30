#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use sea_orm::{ConnectionTrait, Database, Statement};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::error::DomainError;
use file_storage::domain::etag;
use file_storage::domain::multipart::{
    BindState, CompletedMultipartUpload, MultipartCompleteOutcome, MultipartPlan,
};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::FileService;
use file_storage::infra::backend::StorageBackend;
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage_sdk::VersionStatus;

mod common;
use common::{CompleteOutcomeExt, ctx, new_file, new_file_for, uuid_blob};

struct Env {
    svc: Arc<FileService>,
    msvc: Arc<MultipartService>,
    multipart_store: Arc<dyn MultipartStore>,
    backend: Arc<dyn StorageBackend>,
    store: Store,
    ctx: SecurityContext,
    dsn: String,
}

async fn build_env() -> Env {
    let s = common::build_stack(
        "mp-replay",
        Arc::new(Issuer::generate(3600).expect("issuer")),
        Some(90),
    )
    .await;
    Env {
        svc: s.svc,
        msvc: s.msvc,
        multipart_store: s.multipart_store,
        backend: s.backend,
        store: s.store,
        ctx: ctx(Uuid::now_v7()),
        dsn: s.dsn,
    }
}

impl Env {
    /// Bare file plus a one-part session of exactly `data`, with that part uploaded.
    async fn session_with_part(
        &self,
        data: &'static [u8],
        auto_bind: bool,
    ) -> (Uuid, MultipartPlan) {
        let file_id = self
            .svc
            .create_file(&self.ctx, new_file("upload.bin"), None, false)
            .await
            .unwrap()
            .file_id;
        let plan = self.add_session_with_part(file_id, data, auto_bind).await;
        (file_id, plan)
    }

    /// One more one-part session (with its part uploaded) on an existing file.
    async fn add_session_with_part(
        &self,
        file_id: Uuid,
        data: &'static [u8],
        auto_bind: bool,
    ) -> MultipartPlan {
        common::session_with_part(
            &self.msvc,
            &self.multipart_store,
            &self.backend,
            &self.ctx,
            file_id,
            data,
            auto_bind,
        )
        .await
    }

    async fn try_complete(
        &self,
        file_id: Uuid,
        upload_id: Uuid,
        if_match: Option<&str>,
    ) -> Result<CompletedMultipartUpload, DomainError> {
        self.msvc
            .complete_multipart_upload(&self.ctx, file_id, upload_id, if_match)
            .await
            .map(MultipartCompleteOutcome::unwrap_completed)
    }

    async fn complete(&self, file_id: Uuid, upload_id: Uuid) -> CompletedMultipartUpload {
        self.try_complete(file_id, upload_id, None).await.unwrap()
    }
}

/// Run one raw-SQL statement against an independent connection to `dsn` and assert it hit exactly
/// the row the test set up — a silent no-op tamper would make the rest of the test meaningless.
async fn exec_expect_one_row(dsn: &str, label: &'static str, sql: &str) {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let res = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("tamper `{label}` failed: {e}"));
    assert_eq!(
        res.rows_affected(),
        1,
        "tamper `{label}` must hit exactly the one row the test prepared"
    );
}

/// Force `replay_completed` to fall through the persisted-snapshot fast path to its version-row
/// fallback, by wiping the snapshot the real `complete` just persisted.
async fn null_complete_result(dsn: &str, upload_id: Uuid) {
    exec_expect_one_row(
        dsn,
        "null_complete_result",
        &format!(
            "UPDATE multipart_uploads SET complete_result = NULL WHERE upload_id = {}",
            uuid_blob(upload_id)
        ),
    )
    .await;
}

/// With the snapshot wiped, `replay_completed` rebuilds the result from the version row: every
/// field equals the first complete's, and the bind state follows the session's `auto_bind`.
#[tokio::test]
async fn replay_completed_fallback_rebuilds_result_from_version_row() {
    // (auto_bind, expected bind_state, expects the content ETag derived from (file, version))
    for (auto_bind, expected_state, expects_etag) in [
        (false, BindState::Manual, false),
        (true, BindState::Bound, true),
    ] {
        let row = format!("auto_bind={auto_bind}");
        let env = build_env().await;
        let (file_id, plan) = env.session_with_part(b"Hello, World!", auto_bind).await;
        let first = env.complete(file_id, plan.upload_id).await;
        assert_eq!(first.bind_state, expected_state, "{row}: first complete");

        null_complete_result(&env.dsn, plan.upload_id).await;
        let replay = env
            .try_complete(file_id, plan.upload_id, None)
            .await
            .unwrap_or_else(|e| panic!("{row}: fallback replay must succeed: {e:?}"));

        assert_eq!(replay.version_id, first.version_id, "{row}");
        assert_eq!(replay.size, first.size, "{row}");
        assert_eq!(replay.hash_algorithm, first.hash_algorithm, "{row}");
        assert_eq!(replay.content_hash, first.content_hash, "{row}");
        assert_eq!(replay.hash_mode, first.hash_mode, "{row}");
        assert_eq!(
            replay.hash_mode,
            HashMode::WholeSha256,
            "{row}: single part degenerates to whole-sha256"
        );
        assert_eq!(replay.part_count, first.part_count, "{row}");
        assert_eq!(replay.manifest, first.manifest, "{row}");
        assert_eq!(replay.bind_state, expected_state, "{row}");
        let expected_etag = expects_etag.then(|| etag::content_etag(file_id, plan.version_id));
        assert_eq!(
            replay.etag, expected_etag,
            "{row}: Bound derives the content ETag from (file_id, version_id), Manual has none"
        );
        assert_eq!(replay.current_etag, None, "{row}");
    }
}

/// A manual session never reports a bind outcome from the fallback either, even after the version
/// was bound by hand (the pointer then equals the session's version).
#[tokio::test]
async fn replay_completed_fallback_of_a_manual_session_stays_manual_after_manual_bind() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"Hello, World!", false).await;
    let first = env.complete(file_id, plan.upload_id).await;
    assert_eq!(first.bind_state, BindState::Manual);
    env.svc
        .bind(&env.ctx, file_id, plan.version_id, None)
        .await
        .unwrap();

    null_complete_result(&env.dsn, plan.upload_id).await;
    let replay = env.complete(file_id, plan.upload_id).await;
    assert_eq!(replay.bind_state, BindState::Manual);
    assert_eq!(replay.etag, None);
    assert_eq!(replay.current_etag, None);
}

#[tokio::test]
async fn complete_takeover_finishes_without_reassembly_when_version_already_available() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    let part = env
        .store
        .list_multipart_parts(plan.upload_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("one part");

    let finalize_audit = AuditEntry::success(
        env.ctx.subject_tenant_id(),
        "user",
        env.ctx.subject_id(),
        Some(file_id),
        AuditOperation::FinalizeVersion,
        serde_json::json!({ "version_id": plan.version_id }),
    );
    let outcome = env
        .multipart_store
        .finalize_version(
            file_id,
            plan.version_id,
            part.size,
            part.part_hash.clone(),
            HashMode::WholeSha256,
            None,
            None,
            Some("application/octet-stream".to_owned()),
            finalize_audit,
            None,
        )
        .await
        .expect("finalize_version");
    assert!(
        outcome.updated,
        "the pending version row must have been finalized"
    );

    let now = time::OffsetDateTime::now_utc();
    let acquired = env
        .multipart_store
        .acquire_multipart_complete_lease(
            plan.upload_id,
            "dead-completer",
            now - time::Duration::seconds(5),
            now,
        )
        .await
        .unwrap();
    assert!(
        acquired,
        "the dead-completer lease must attach to the still in_progress session"
    );

    let completed = env
        .try_complete(file_id, plan.upload_id, None)
        .await
        .expect("takeover onto an already-finalized version must succeed");
    assert_eq!(completed.version_id, plan.version_id);
    assert_eq!(completed.size, part.size);

    let finished_session = env
        .multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session still exists");
    assert_eq!(
        finished_session.state,
        file_storage::domain::multipart::MultipartUploadState::Completed,
        "finish_session must flip completing -> completed"
    );
    assert!(
        finished_session.complete_result.is_some(),
        "finish_session must persist the response snapshot"
    );

    let version = env
        .store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, VersionStatus::Available);
}

/// The object is already assembled (the handle is consumed) but the session is still `in_progress`,
/// as after "assembly succeeded, finalize failed, lease released": a plain (non-takeover) `complete`
/// must recover from the existing object instead of re-assembling.
#[tokio::test]
async fn complete_recovers_from_assembled_object_without_takeover() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    let session = env
        .multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let part = env
        .store
        .list_multipart_parts(plan.upload_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("one part");

    // Assemble behind the service's back, with the same parts it would use.
    let digest: [u8; 32] = part.part_hash.clone().try_into().expect("32-byte digest");
    env.backend
        .complete_multipart(
            &session.backend_path,
            &session.backend_upload_handle,
            &[(1, 0, digest, part.backend_etag.clone())],
        )
        .await
        .expect("out-of-band assembly");
    assert!(env.backend.exists(&session.backend_path).await.unwrap());
    assert_eq!(
        session.state,
        file_storage::domain::multipart::MultipartUploadState::InProgress
    );
    // A second assembly now fails on the consumed handle.
    assert!(
        env.backend
            .complete_multipart(
                &session.backend_path,
                &session.backend_upload_handle,
                &[(1, 0, digest, part.backend_etag.clone())],
            )
            .await
            .is_err()
    );

    let completed = env
        .try_complete(file_id, plan.upload_id, None)
        .await
        .expect("complete must recover from the already assembled object");
    assert_eq!(completed.version_id, plan.version_id);
    assert_eq!(completed.size, part.size);

    let finished = env
        .multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    assert_eq!(
        finished.state,
        file_storage::domain::multipart::MultipartUploadState::Completed
    );
    let version = env
        .store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, VersionStatus::Available);
}

/// A session an old instance wrote after the upload-flow migration has NULL `backend_id` and
/// `backend_path`: it still reads, introspects, is listed as expired and completes.
#[tokio::test]
async fn session_with_null_backend_columns_reads_lists_and_completes() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    exec_expect_one_row(
        &env.dsn,
        "null_backend_pair",
        &format!(
            "UPDATE multipart_uploads SET backend_id = NULL, backend_path = NULL WHERE upload_id = {}",
            uuid_blob(plan.upload_id)
        ),
    )
    .await;

    let session = env
        .multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .expect("session with NULL pair must still map")
        .expect("session exists");
    assert_eq!(session.backend_id, None);
    assert_eq!(
        session.backend_path,
        format!("/{file_id}/{}", plan.version_id),
        "a NULL path falls back to the layout every version is written under"
    );

    let status = env
        .msvc
        .introspect_multipart_upload(&env.ctx, file_id, plan.upload_id)
        .await
        .expect("introspect a session with NULL pair");
    assert_eq!(status.upload_id, plan.upload_id);

    let later = time::OffsetDateTime::now_utc() + time::Duration::days(3650);
    let expired = env
        .store
        .list_expired_multipart_uploads(later)
        .await
        .expect("list_expired must not fail on a NULL pair");
    assert!(expired.iter().any(|s| s.upload_id == plan.upload_id));

    let done = env.complete(file_id, plan.upload_id).await;
    assert_eq!(done.version_id, plan.version_id);
}

/// An idempotency ticket stored before the `bind` flag existed (no `auto_bind` field) replays
/// under any `bind` mode, in manual mode: the re-signed token carries no `bind_on_finalize`.
#[tokio::test]
async fn legacy_idempotency_ticket_without_auto_bind_replays_manual() {
    let env = build_env().await;
    let key = "legacy-key".to_owned();
    let owner = Uuid::now_v7();
    let first = env
        .svc
        .create_file(
            &env.ctx,
            new_file_for(owner, "legacy.bin", "application/octet-stream"),
            Some(key.clone()),
            true,
        )
        .await
        .expect("initial create");
    exec_expect_one_row(
        &env.dsn,
        "strip_auto_bind",
        "UPDATE idempotency_keys SET response_body = json_remove(response_body, '$.auto_bind') \
         WHERE idempotency_key = 'legacy-key'",
    )
    .await;

    for auto in [true, false] {
        let replay = env
            .svc
            .create_file(
                &env.ctx,
                new_file_for(owner, "legacy.bin", "application/octet-stream"),
                Some(key.clone()),
                auto,
            )
            .await
            .unwrap_or_else(|e| panic!("legacy replay with auto_bind={auto} must succeed: {e:?}"));
        assert_eq!(replay.file_id, first.file_id);
        assert_eq!(replay.version_id, first.version_id);

        let token = &replay.upload_url
            [replay.upload_url.find("fs-token=").expect("fs-token") + "fs-token=".len()..];
        let claims = env
            .svc
            .verifier()
            .verify(token, time::OffsetDateTime::now_utc())
            .expect("re-signed token verifies");
        assert!(
            !claims.bind_on_finalize,
            "a legacy ticket is replayed in manual mode"
        );
    }
}
