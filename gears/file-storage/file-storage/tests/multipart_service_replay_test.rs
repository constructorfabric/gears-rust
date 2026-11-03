#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::error::DomainError;
use file_storage::domain::etag;
use file_storage::domain::multipart::{
    BindState, CompletedMultipartUpload, MultipartCompleteOutcome, MultipartPart, MultipartPlan,
    MultipartUploadSession, MultipartUploadState,
};
use file_storage::infra::content::hash_mode::HashMode;
use file_storage_sdk::{FileVersion, VersionStatus};

mod common;
use common::{
    CompleteOutcomeExt, Stack, StackBuilder, bare_file, ctx, new_file_for, token_of, uuid_blob,
};

struct Env {
    stack: Stack,
    ctx: SecurityContext,
}

async fn build_env() -> Env {
    let stack = StackBuilder::new("mp-replay")
        .tweak_msvc(|msvc| msvc.with_complete_lease_secs(90))
        .build()
        .await;
    Env {
        stack,
        ctx: ctx(Uuid::now_v7()),
    }
}

impl Env {
    /// Bare file plus a one-part session of exactly `data`, with that part uploaded.
    async fn session_with_part(
        &self,
        data: &'static [u8],
        auto_bind: bool,
    ) -> (Uuid, MultipartPlan) {
        let file_id = bare_file(&self.stack.svc, &self.ctx).await;
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
            &self.stack.msvc,
            &self.stack.multipart_store,
            &self.stack.backend,
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
        self.stack
            .msvc
            .complete_multipart_upload(&self.ctx, file_id, upload_id, if_match)
            .await
            .map(MultipartCompleteOutcome::unwrap_completed)
    }

    async fn complete(&self, file_id: Uuid, upload_id: Uuid) -> CompletedMultipartUpload {
        self.try_complete(file_id, upload_id, None).await.unwrap()
    }

    /// The single uploaded part of a one-part session.
    async fn only_part(&self, upload_id: Uuid) -> MultipartPart {
        let parts = self
            .stack
            .store
            .list_multipart_parts(upload_id)
            .await
            .unwrap();
        parts.into_iter().next().expect("one part")
    }

    async fn version(&self, file_id: Uuid, version_id: Uuid) -> FileVersion {
        let v = self
            .stack
            .store
            .get_version(file_id, version_id)
            .await
            .unwrap();
        v.expect("version row")
    }

    async fn session(&self, upload_id: Uuid) -> MultipartUploadSession {
        let s = self
            .stack
            .multipart_store
            .get_multipart_upload(upload_id)
            .await
            .unwrap();
        s.expect("session row")
    }
}

/// Force `replay_completed` to fall through the persisted-snapshot fast path to its version-row
/// fallback, by wiping the snapshot the real `complete` just persisted.
async fn null_complete_result(dsn: &str, upload_id: Uuid) {
    common::exec_expect_one_row(
        dsn,
        "null_complete_result",
        &format!(
            "UPDATE multipart_uploads SET complete_result = NULL WHERE upload_id = {}",
            uuid_blob(upload_id)
        ),
    )
    .await;
}

async fn set_hash_mode_bypassing_check(dsn: &str, version_id: Uuid, value: &str) {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let backend = conn.get_database_backend();
    let txn = conn.begin().await.expect("begin txn");
    txn.execute_raw(Statement::from_string(
        backend,
        "PRAGMA ignore_check_constraints = ON;".to_owned(),
    ))
    .await
    .expect("disable CHECK enforcement for this connection");
    let res = txn
        .execute_raw(Statement::from_string(
            backend,
            format!(
                "UPDATE file_versions SET hash_mode = '{value}' WHERE version_id = {}",
                uuid_blob(version_id)
            ),
        ))
        .await
        .expect("tamper hash_mode");
    assert_eq!(
        res.rows_affected(),
        1,
        "tamper UPDATE must hit exactly the one row the test prepared"
    );
    txn.commit().await.expect("commit tamper txn");
}

/// With the snapshot wiped, `replay_completed` rebuilds the result from the version row: every
/// field equals the first complete's, and the bind state follows the session's `auto_bind`, not
/// the live pointer (a manual session stays manual even after the version was bound by hand).
#[tokio::test]
async fn replay_completed_fallback_rebuilds_result_from_version_row() {
    // (row, auto_bind, bound by hand after complete, expected bind_state)
    let rows = [
        ("manual", false, false, BindState::Manual),
        ("auto", true, false, BindState::Bound),
        ("manual, then bound by hand", false, true, BindState::Manual),
    ];
    for (row, auto_bind, bind_by_hand, expected_state) in rows {
        let env = build_env().await;
        let (file_id, plan) = env.session_with_part(b"Hello, World!", auto_bind).await;
        let first = env.complete(file_id, plan.upload_id).await;
        assert_eq!(first.bind_state, expected_state, "{row}: first complete");
        let own_etag = auto_bind.then(|| etag::content_etag(file_id, plan.version_id));
        assert_eq!(
            common::content_id_of(&env.stack.store, file_id).await,
            auto_bind.then_some(plan.version_id),
            "{row}: only an auto-bind session binds on complete"
        );
        if bind_by_hand {
            let bound = env
                .stack
                .svc
                .bind(&env.ctx, file_id, plan.version_id, None)
                .await;
            bound.unwrap();
        }

        null_complete_result(&env.stack.dsn, plan.upload_id).await;
        let replay = env.complete(file_id, plan.upload_id).await;

        assert_eq!(replay.version_id, first.version_id, "{row}");
        assert_eq!(replay.size, first.size, "{row}");
        assert_eq!(replay.hash_algorithm, first.hash_algorithm, "{row}");
        assert_eq!(replay.content_hash, first.content_hash, "{row}");
        assert_eq!(replay.hash_mode, HashMode::WholeSha256, "{row}");
        assert_eq!(replay.part_count, first.part_count, "{row}");
        assert_eq!(replay.manifest, first.manifest, "{row}");
        assert_eq!(replay.bind_state, expected_state, "{row}");
        assert_eq!(
            replay.etag, own_etag,
            "{row}: Bound derives the ETag from (file, version)"
        );
        assert_eq!(replay.current_etag, None, "{row}");
    }
}

/// Snapshot wiped AND the version row mutated: each row must fail with its own error.
#[tokio::test]
async fn replay_completed_fallback_errors_on_unusable_version_row() {
    for row in [
        "version row is gone",
        "version not available",
        "unrecognized hash mode",
    ] {
        let env = build_env().await;
        let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
        env.complete(file_id, plan.upload_id).await;
        null_complete_result(&env.stack.dsn, plan.upload_id).await;
        let version = uuid_blob(plan.version_id);
        let dsn = &env.stack.dsn;
        match row {
            "version row is gone" => {
                let sql = format!("DELETE FROM file_versions WHERE version_id = {version}");
                common::exec_expect_one_row(dsn, row, &sql).await;
            }
            "version not available" => {
                let sql = format!(
                    "UPDATE file_versions SET status = 'pending' WHERE version_id = {version}"
                );
                common::exec_expect_one_row(dsn, row, &sql).await;
            }
            _ => set_hash_mode_bypassing_check(dsn, plan.version_id, "bogus-mode").await,
        }

        let err = env
            .try_complete(file_id, plan.upload_id, None)
            .await
            .expect_err("replay must fail");
        match (row, err) {
            (
                "version row is gone",
                DomainError::VersionNotFound {
                    file_id: f,
                    version_id: v,
                },
            ) => {
                assert_eq!((f, v), (file_id, plan.version_id));
            }
            ("version not available", DomainError::MultipartUploadNotInProgress { state, .. }) => {
                assert_eq!(
                    state, "completed",
                    "reported state is the SESSION's own state"
                );
            }
            ("unrecognized hash mode", DomainError::Database { .. }) => {}
            (_, other) => panic!("{row}: unexpected error {other:?}"),
        }
    }
}

/// A session completed (and auto-bound) before the file was rebound to another version: a bind won
/// at finalize is sticky, so both the persisted snapshot and the version-row fallback (snapshot
/// wiped, `bound_on_finalize` read from the version) keep reporting `Bound` with the original `ETag`.
#[tokio::test]
async fn replay_after_a_later_rebind_keeps_reporting_the_won_bind() {
    let env = build_env().await;
    let (file_id, plan_a) = env.session_with_part(b"AAAAA", true).await;
    let completed_a = env.complete(file_id, plan_a.upload_id).await;
    let original_etag = completed_a
        .etag
        .clone()
        .expect("a Bound completion carries an etag");
    let plan_b = env.add_session_with_part(file_id, b"BBBBB", false).await;
    env.complete(file_id, plan_b.upload_id).await;
    let rebound = env
        .stack
        .svc
        .bind(&env.ctx, file_id, plan_b.version_id, Some("*"))
        .await;
    rebound.expect("rebind to version B");

    for wiped in [false, true] {
        if wiped {
            null_complete_result(&env.stack.dsn, plan_a.upload_id).await;
        }
        let replay = env.complete(file_id, plan_a.upload_id).await;
        assert_eq!(
            replay.bind_state,
            BindState::Bound,
            "snapshot wiped={wiped}"
        );
        assert_eq!(
            replay.etag.as_deref(),
            Some(original_etag.as_str()),
            "wiped={wiped}"
        );
        assert_eq!(replay.version_id, plan_a.version_id, "wiped={wiped}");
        assert_eq!(replay.current_etag, None, "wiped={wiped}");
    }
}

/// The finalize transaction loses its own session CAS (a trigger completes the session the moment
/// the version flips to `available`): the result still reports THIS transaction's bind decision.
#[tokio::test]
async fn complete_reports_transaction_bind_outcome_when_own_session_cas_is_lost() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"Hello, World!", true).await;
    common::raw_exec(
        &env.stack.dsn,
        "CREATE TRIGGER session_completed_out_from_under AFTER UPDATE OF status ON file_versions \
         WHEN NEW.status = 'available' BEGIN \
         UPDATE multipart_uploads SET state = 'completed', complete_result = NULL, \
         lease_owner = NULL, lease_until = NULL \
         WHERE version_id = NEW.version_id AND state = 'completing'; END",
    )
    .await;

    let completed = env.complete(file_id, plan.upload_id).await;

    assert_eq!(
        completed.bind_state,
        BindState::Bound,
        "this transaction's own bind decision"
    );
    let expected = etag::content_etag(file_id, plan.version_id);
    assert_eq!(completed.etag.as_deref(), Some(expected.as_str()));
    assert_eq!(completed.current_etag, None);
    let content_id = common::content_id_of(&env.stack.store, file_id).await;
    assert_eq!(content_id, Some(plan.version_id));
    let session = env.session(plan.upload_id).await;
    assert_eq!(session.state, MultipartUploadState::Completed);
}

#[tokio::test]
async fn replay_completed_snapshot_404s_when_its_composite_version_is_deleted() {
    let env = build_env().await;
    let (file_id, plan, _full) = common::composite_session(&env.stack, &env.ctx).await;
    env.complete(file_id, plan.upload_id).await;
    let sql = format!(
        "DELETE FROM file_versions WHERE version_id = {}",
        uuid_blob(plan.version_id)
    );
    common::exec_expect_one_row(&env.stack.dsn, "delete_version_row", &sql).await;

    // The snapshot is still there but its version is gone: 404, never a response with `manifest: null`.
    let err = env
        .try_complete(file_id, plan.upload_id, None)
        .await
        .expect_err("replay of a completed session whose version was since deleted must fail");
    assert!(
        matches!(&err, DomainError::VersionNotFound { file_id: f, version_id: v }
            if (*f, *v) == (file_id, plan.version_id)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn complete_takeover_finishes_without_reassembly_when_version_already_available() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    let part = env.only_part(plan.upload_id).await;
    let audit = AuditEntry::success(
        env.ctx.subject_tenant_id(),
        "user",
        env.ctx.subject_id(),
        Some(file_id),
        AuditOperation::FinalizeVersion,
        serde_json::json!({ "version_id": plan.version_id }),
    );
    let outcome = env
        .stack
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
            audit,
            None,
        )
        .await
        .expect("finalize_version");
    assert!(
        outcome.updated,
        "the pending version row must have been finalized"
    );
    common::plant_lease(
        &env.stack.multipart_store,
        plan.upload_id,
        "dead-completer",
        -5,
    )
    .await;

    let completed = env.complete(file_id, plan.upload_id).await;
    assert_eq!(
        (completed.version_id, completed.size),
        (plan.version_id, part.size)
    );

    let session = env.session(plan.upload_id).await;
    assert_eq!(session.state, MultipartUploadState::Completed);
    assert!(
        session.complete_result.is_some(),
        "finish_session persists the response snapshot"
    );
    assert_eq!(
        env.version(file_id, plan.version_id).await.status,
        VersionStatus::Available
    );
}

/// The object is already assembled (the handle is consumed) but the session is still `in_progress`,
/// as after "assembly succeeded, finalize failed, lease released": a plain (non-takeover) `complete`
/// must recover from the existing object instead of re-assembling.
#[tokio::test]
async fn complete_recovers_from_assembled_object_without_takeover() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    let session = env.session(plan.upload_id).await;
    assert_eq!(session.state, MultipartUploadState::InProgress);
    let part = env.only_part(plan.upload_id).await;

    // Assemble behind the service's back, with the same parts it would use.
    let digest: [u8; 32] = part.part_hash.clone().try_into().expect("32-byte digest");
    env.stack
        .backend
        .complete_multipart(
            &session.backend_path,
            &session.backend_upload_handle,
            &[(1, 0, digest, part.backend_etag.clone())],
        )
        .await
        .expect("out-of-band assembly");

    let completed = env.complete(file_id, plan.upload_id).await;
    assert_eq!(
        (completed.version_id, completed.size),
        (plan.version_id, part.size)
    );
    assert_eq!(
        env.session(plan.upload_id).await.state,
        MultipartUploadState::Completed
    );
    assert_eq!(
        env.version(file_id, plan.version_id).await.status,
        VersionStatus::Available
    );
}

/// A session an old instance wrote after the upload-flow migration has NULL `backend_id` and
/// `backend_path`: it still reads, introspects, is listed as expired and completes.
#[tokio::test]
async fn session_with_null_backend_columns_reads_lists_and_completes() {
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    common::exec_expect_one_row(
        &env.stack.dsn,
        "null_backend_pair",
        &format!(
            "UPDATE multipart_uploads SET backend_id = NULL, backend_path = NULL \
             WHERE upload_id = {}",
            uuid_blob(plan.upload_id)
        ),
    )
    .await;

    let session = env.session(plan.upload_id).await;
    assert_eq!(session.backend_id, None);
    assert_eq!(
        session.backend_path,
        format!("/{file_id}/{}", plan.version_id),
        "a NULL path falls back to the layout every version is written under"
    );
    let status = env
        .stack
        .msvc
        .introspect_multipart_upload(&env.ctx, file_id, plan.upload_id)
        .await
        .expect("introspect a session with NULL pair");
    assert_eq!(status.upload_id, plan.upload_id);
    let later = time::OffsetDateTime::now_utc() + time::Duration::days(3650);
    let expired = env.stack.store.list_expired_multipart_uploads(later).await;
    assert!(
        expired
            .expect("list_expired")
            .iter()
            .any(|s| s.upload_id == plan.upload_id)
    );

    let done = env.complete(file_id, plan.upload_id).await;
    assert_eq!(done.version_id, plan.version_id);
}

/// An idempotency ticket stored before the `bind` flag existed (no `auto_bind` field) replays
/// under any `bind` mode, in manual mode: the re-signed token carries no `bind_on_finalize`.
#[tokio::test]
async fn legacy_idempotency_ticket_without_auto_bind_replays_manual() {
    let env = build_env().await;
    let owner = Uuid::now_v7();
    let file = || new_file_for(owner, "legacy.bin", "application/octet-stream");
    let key = Some("legacy-key".to_owned());
    let first = env
        .stack
        .svc
        .create_file(&env.ctx, file(), key.clone(), true)
        .await
        .unwrap();
    common::exec_expect_one_row(
        &env.stack.dsn,
        "strip_auto_bind",
        "UPDATE idempotency_keys SET response_body = json_remove(response_body, '$.auto_bind') \
         WHERE idempotency_key = 'legacy-key'",
    )
    .await;

    for auto in [true, false] {
        let replay = env
            .stack
            .svc
            .create_file(&env.ctx, file(), key.clone(), auto)
            .await
            .unwrap_or_else(|e| panic!("legacy replay with auto_bind={auto} must succeed: {e:?}"));
        assert_eq!(
            (replay.file_id, replay.version_id),
            (first.file_id, first.version_id)
        );
        let claims = env
            .stack
            .svc
            .verifier()
            .verify(
                token_of(&replay.upload_url),
                time::OffsetDateTime::now_utc(),
            )
            .expect("re-signed token verifies");
        assert!(
            !claims.bind_on_finalize,
            "a legacy ticket is replayed in manual mode"
        );
    }
}

/// `complete` replays a `completed` session before it evaluates `If-Match`, so a retry gets the
/// persisted result whether it carries the now-stale precondition or none.
#[tokio::test]
async fn complete_replay_of_completed_session_ignores_stale_if_match() {
    let env = build_env().await;
    let initial = common::bound_file(&env.stack.svc, &env.stack.dp, &env.ctx, b"initial").await;
    let current = env
        .stack
        .svc
        .get_file(&env.ctx, initial.file_id)
        .await
        .unwrap();
    let stale = etag::etag_for(&current).expect("bound file must have an etag");
    let plan = env
        .add_session_with_part(initial.file_id, b"0123456789", true)
        .await;
    let first = env
        .try_complete(initial.file_id, plan.upload_id, Some(&stale))
        .await
        .expect("first complete: If-Match matches the initial content pointer");
    assert_eq!(first.bind_state, BindState::Bound);

    // The first complete moved the file's ETag, so the initial one is now stale.
    for (row, if_match) in [
        ("stale If-Match", Some(stale.as_str())),
        ("no If-Match", None),
    ] {
        let retry = env
            .try_complete(initial.file_id, plan.upload_id, if_match)
            .await;
        let retry = retry.unwrap_or_else(|e| panic!("{row}: a retry must replay: {e:?}"));
        assert_eq!(retry.version_id, first.version_id, "{row}");
    }
}
