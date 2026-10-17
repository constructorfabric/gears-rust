#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};
use time::OffsetDateTime;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::data_plane::DataPlaneService;
use file_storage::domain::error::DomainError;
use file_storage::domain::etag;
use file_storage::domain::multipart::{
    BindState, CompletedMultipartUpload, MultipartCompleteOutcome, MultipartPart, MultipartPlan,
    MultipartUploadSession,
};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{PolicyScope, StoredPolicy};
use file_storage::domain::ports::{
    AutoBindOnFinalize, DataPlanePort, FinalizeMultipartOutcome, FinalizeVersionOutcome,
    MultipartFinishSnapshot, MultipartStore,
};
use file_storage::domain::service::FileService;
use file_storage::infra::backend::{BackendRegistry, StorageBackend};
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage_sdk::{File, FileVersion, VersionStatus};

mod common;
use common::{ctx, new_file, put_part};

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
            .create_file_bare(&self.ctx, new_file("upload.bin"))
            .await
            .unwrap();
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
        let plan = self
            .msvc
            .initiate_multipart_upload(
                &self.ctx,
                file_id,
                "application/octet-stream",
                data.len() as u64,
                None,
                auto_bind,
            )
            .await
            .unwrap();
        put_part(
            &self.multipart_store,
            &self.backend,
            &plan,
            file_id,
            1,
            Bytes::from_static(data),
        )
        .await;
        plan
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

    /// File A auto-bound via session A (`AAAAA`), then manual session B (`BBBBB`) completed and
    /// explicitly rebound over it: the file now points at B, A's session is a historical `Bound`.
    async fn bound_a_then_rebound_to_b(
        &self,
    ) -> (Uuid, MultipartPlan, CompletedMultipartUpload, MultipartPlan) {
        let (file_id, plan_a) = self.session_with_part(b"AAAAA", true).await;
        let completed_a = self.complete(file_id, plan_a.upload_id).await;
        assert_eq!(completed_a.bind_state, BindState::Bound);

        let plan_b = self.add_session_with_part(file_id, b"BBBBB", false).await;
        let completed_b = self.complete(file_id, plan_b.upload_id).await;
        assert_eq!(completed_b.bind_state, BindState::Manual);
        self.svc
            .bind(&self.ctx, file_id, plan_b.version_id, Some("*"))
            .await
            .expect("rebind to version B");
        (file_id, plan_a, completed_a, plan_b)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut acc, b| {
        write!(acc, "{b:02x}").expect("writing to a String cannot fail");
        acc
    })
}

fn uuid_blob(id: Uuid) -> String {
    format!("X'{}'", hex_encode(id.as_bytes()))
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

async fn reset_session_to_in_progress(dsn: &str, upload_id: Uuid) {
    exec_expect_one_row(
        dsn,
        "reset_session_to_in_progress",
        &format!(
            "UPDATE multipart_uploads SET state = 'in_progress', lease_owner = NULL, \
             lease_until = NULL WHERE upload_id = {}",
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

async fn tamper_session_to_completed_out_from_under(dsn: &str, upload_id: Uuid) {
    exec_expect_one_row(
        dsn,
        "session_completed_out_from_under",
        &format!(
            "UPDATE multipart_uploads SET state = 'completed', complete_result = NULL, \
             lease_owner = NULL, lease_until = NULL WHERE upload_id = {} AND state = 'completing'",
            uuid_blob(upload_id)
        ),
    )
    .await;
}

async fn tamper_rebind_content(dsn: &str, file_id: Uuid, new_content_id: Uuid) {
    exec_expect_one_row(
        dsn,
        "rebind_content",
        &format!(
            "UPDATE files SET content_id = {} WHERE file_id = {}",
            uuid_blob(new_content_id),
            uuid_blob(file_id)
        ),
    )
    .await;
}

struct SessionCasRaceStore {
    inner: Arc<dyn MultipartStore>,
    dsn: String,
    rebind_content_id: Uuid,
    armed: AtomicBool,
}

#[async_trait]
impl MultipartStore for SessionCasRaceStore {
    async fn require_file(&self, scope: &AccessScope, file_id: Uuid) -> Result<File, DomainError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            tamper_rebind_content(&self.dsn, file_id, self.rebind_content_id).await;
        }
        self.inner.require_file(scope, file_id).await
    }

    async fn get_policy(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        self.inner
            .get_policy(scope, tenant_id, policy_scope, scope_owner_id)
            .await
    }

    async fn insert_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        mime_type: &str,
        backend_id: &str,
        backend_path: &str,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        self.inner
            .insert_pending_version(
                file_id,
                version_id,
                mime_type,
                backend_id,
                backend_path,
                now,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
        backend_id: &str,
        backend_path: &str,
        declared_mime: &str,
        declared_size: u64,
        part_size: u64,
        auto_bind: bool,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        self.inner
            .create_multipart_upload(
                upload_id,
                file_id,
                version_id,
                backend_upload_handle,
                backend_id,
                backend_path,
                declared_mime,
                declared_size,
                part_size,
                auto_bind,
                expires_at,
                now,
            )
            .await
    }

    async fn get_multipart_upload(
        &self,
        upload_id: Uuid,
    ) -> Result<Option<MultipartUploadSession>, DomainError> {
        self.inner.get_multipart_upload(upload_id).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<FileVersion>, DomainError> {
        self.inner.get_version(file_id, version_id).await
    }

    async fn get_version_manifest(&self, version_id: Uuid) -> Result<Option<String>, DomainError> {
        self.inner.get_version_manifest(version_id).await
    }

    async fn upsert_multipart_part(
        &self,
        upload_id: Uuid,
        part_number: i32,
        backend_etag: &str,
        part_hash: Vec<u8>,
        size: i64,
        now: OffsetDateTime,
    ) -> Result<(), DomainError> {
        self.inner
            .upsert_multipart_part(upload_id, part_number, backend_etag, part_hash, size, now)
            .await
    }

    async fn list_multipart_parts(
        &self,
        upload_id: Uuid,
    ) -> Result<Vec<MultipartPart>, DomainError> {
        self.inner.list_multipart_parts(upload_id).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        hash_mode: HashMode,
        part_count: Option<i32>,
        manifest: Option<String>,
        validated_mime: Option<String>,
        audit: AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
    ) -> Result<FinalizeVersionOutcome, DomainError> {
        self.inner
            .finalize_version(
                file_id,
                version_id,
                size,
                hash_value,
                hash_mode,
                part_count,
                manifest,
                validated_mime,
                audit,
                auto_bind,
            )
            .await
    }

    async fn finalize_multipart_version(
        &self,
        file_id: Uuid,
        manifest: Option<String>,
        validated_mime: Option<String>,
        finalize_audit: AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
        finish: MultipartFinishSnapshot,
    ) -> Result<FinalizeMultipartOutcome, DomainError> {
        tamper_session_to_completed_out_from_under(&self.dsn, finish.upload_id).await;
        self.armed.store(true, Ordering::SeqCst);
        self.inner
            .finalize_multipart_version(
                file_id,
                manifest,
                validated_mime,
                finalize_audit,
                auto_bind,
                finish,
            )
            .await
    }

    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        lease_owner: &str,
        result_json: &str,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner
            .complete_multipart_upload(upload_id, lease_owner, result_json, audit)
            .await
    }

    async fn acquire_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
        lease_until: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        self.inner
            .acquire_multipart_complete_lease(upload_id, owner, lease_until, now)
            .await
    }

    async fn release_multipart_complete_lease(
        &self,
        upload_id: Uuid,
        owner: &str,
    ) -> Result<bool, DomainError> {
        self.inner
            .release_multipart_complete_lease(upload_id, owner)
            .await
    }

    async fn abort_multipart_upload(
        &self,
        upload_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.abort_multipart_upload(upload_id, audit).await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.delete_version(file_id, version_id, audit).await
    }
}

#[tokio::test]
async fn complete_multipart_upload_unknown_upload_id_is_not_found() {
    let env = build_env().await;
    let file_id = env
        .svc
        .create_file_bare(&env.ctx, new_file("upload.bin"))
        .await
        .unwrap();

    let bogus_upload_id = Uuid::now_v7();
    let err = env
        .try_complete(file_id, bogus_upload_id, None)
        .await
        .expect_err("an unknown upload_id must never succeed");
    match err {
        DomainError::MultipartUploadNotFound { upload_id } => {
            assert_eq!(upload_id, bogus_upload_id);
        }
        other => panic!("expected MultipartUploadNotFound, got {other:?}"),
    }
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

/// Snapshot wiped AND the version row mutated: each row must fail with its own error.
#[tokio::test]
async fn replay_completed_fallback_errors_on_unusable_version_row() {
    enum Tamper {
        DeleteRow,
        MarkPending,
        BogusHashMode,
    }
    for (row, tamper) in [
        ("version row is gone", Tamper::DeleteRow),
        ("version not available", Tamper::MarkPending),
        ("unrecognized hash mode", Tamper::BogusHashMode),
    ] {
        let env = build_env().await;
        let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
        let first_complete = env.complete(file_id, plan.upload_id).await;
        // A replay only means anything once the original complete really finished.
        assert!(
            first_complete.size > 0,
            "{row}: the first complete must assemble a non-empty version"
        );

        null_complete_result(&env.dsn, plan.upload_id).await;
        let version = uuid_blob(plan.version_id);
        match tamper {
            Tamper::DeleteRow => {
                exec_expect_one_row(
                    &env.dsn,
                    "delete_version_row",
                    &format!("DELETE FROM file_versions WHERE version_id = {version}"),
                )
                .await;
            }
            Tamper::MarkPending => {
                exec_expect_one_row(
                    &env.dsn,
                    "mark_version_pending",
                    &format!(
                        "UPDATE file_versions SET status = 'pending' WHERE version_id = {version}"
                    ),
                )
                .await;
            }
            Tamper::BogusHashMode => {
                set_hash_mode_bypassing_check(&env.dsn, plan.version_id, "bogus-mode").await;
            }
        }

        let err = env
            .try_complete(file_id, plan.upload_id, None)
            .await
            .expect_err(&format!("{row}: replay must fail"));
        match (tamper, err) {
            (
                Tamper::DeleteRow,
                DomainError::VersionNotFound {
                    file_id: f,
                    version_id: v,
                },
            ) => {
                assert_eq!(f, file_id, "{row}");
                assert_eq!(v, plan.version_id, "{row}");
            }
            (
                Tamper::MarkPending,
                DomainError::MultipartUploadNotInProgress { upload_id, state },
            ) => {
                assert_eq!(upload_id, plan.upload_id, "{row}");
                assert_eq!(
                    state, "completed",
                    "{row}: reported state is the SESSION's own state"
                );
            }
            (Tamper::BogusHashMode, DomainError::Database { .. }) => {}
            (_, other) => panic!("{row}: unexpected error {other:?}"),
        }
    }
}

#[tokio::test]
async fn replay_completed_fallback_reports_conflict_when_content_rebound_elsewhere() {
    let env = build_env().await;
    let (file_id, plan_a, _completed_a, _plan_b) = env.bound_a_then_rebound_to_b().await;

    null_complete_result(&env.dsn, plan_a.upload_id).await;
    let replay_a = env
        .try_complete(file_id, plan_a.upload_id, None)
        .await
        .expect("fallback replay after an external rebind must still succeed");
    assert_eq!(replay_a.bind_state, BindState::Conflict);
    assert_eq!(replay_a.etag, None);
    let file = env.svc.get_file(&env.ctx, file_id).await.expect("file");
    assert_eq!(
        replay_a.current_etag,
        Some(etag::content_etag(
            file_id,
            file.content_id.expect("bound to B")
        )),
        "Conflict branch must report the file's CURRENT etag, pointing at B"
    );
}

#[tokio::test]
async fn replay_completed_snapshot_survives_a_later_rebind_without_tampering() {
    let env = build_env().await;
    let (file_id, plan_a, completed_a, plan_b) = env.bound_a_then_rebound_to_b().await;
    let original_etag = completed_a
        .etag
        .clone()
        .expect("a Bound completion must carry an etag");

    let replay_a = env
        .try_complete(file_id, plan_a.upload_id, None)
        .await
        .expect("idempotent re-complete after an unrelated later rebind must still succeed");
    assert_eq!(
        replay_a.bind_state,
        BindState::Bound,
        "the snapshot must report the historical Bound outcome, not today's Conflict"
    );
    assert_eq!(
        replay_a.etag.as_deref(),
        Some(original_etag.as_str()),
        "the snapshot must report the ORIGINAL etag, not one derived from B's current bind"
    );
    assert_eq!(replay_a.version_id, plan_a.version_id);

    // Sanity: the file itself really did move on to B -- this is a genuine divergence between
    // "current state" and "this session's historical outcome", not a no-op rebind.
    let file = env.svc.get_file(&env.ctx, file_id).await.expect("file");
    assert_eq!(file.content_id, Some(plan_b.version_id));
}

#[tokio::test]
async fn complete_reports_transaction_bind_outcome_when_own_session_cas_is_lost() {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let stack = common::build_stack("mp-replay", Arc::clone(&issuer), None).await;
    let (svc, backend, real_multipart_store, dsn) =
        (stack.svc, stack.backend, stack.multipart_store, stack.dsn);
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let raced_store: Arc<dyn MultipartStore> = Arc::new(SessionCasRaceStore {
        inner: Arc::clone(&real_multipart_store),
        dsn: dsn.clone(),
        // Stands in for "some other, unrelated version" -- never dereferenced as a real row, only
        // ever compared against by `bind_state_for` on the (buggy) fallback path.
        rebind_content_id: Uuid::now_v7(),
        armed: AtomicBool::new(false),
    });
    let msvc = Arc::new(
        MultipartService::new(
            Arc::clone(&raced_store),
            backends,
            Arc::new(TenantOnlyAuthorizer),
            None,
            issuer,
            "http://sidecar.test".to_owned(),
            3600,
        )
        .with_complete_lease_secs(90),
    );
    let ctx = ctx(Uuid::now_v7());
    let file_id = svc
        .create_file_bare(&ctx, new_file("upload.bin"))
        .await
        .unwrap();

    // auto_bind: the finalize's own auto-bind CAS is what must win authoritatively.
    let plan = msvc
        .initiate_multipart_upload(&ctx, file_id, "application/octet-stream", 13, None, true)
        .await
        .unwrap();
    put_part(
        &real_multipart_store,
        &backend,
        &plan,
        file_id,
        1,
        Bytes::from_static(b"Hello, World!"),
    )
    .await;

    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("finalize won its own CAS -- session_completed==false must still succeed")
        .unwrap_completed();

    assert_eq!(
        completed.bind_state,
        BindState::Bound,
        "must report THIS transaction's own auto-bind decision, not a live recompute"
    );
    assert_eq!(
        completed.etag.as_deref(),
        Some(etag::content_etag(file_id, plan.version_id).as_str()),
        "must carry this version's own content etag"
    );
    assert_eq!(completed.current_etag, None);

    let file = svc.get_file(&ctx, file_id).await.expect("file");
    assert_eq!(
        file.content_id,
        Some(plan.version_id),
        "the file must really be bound to this version -- the rebind tamper never fired"
    );
}

#[tokio::test]
async fn replay_completed_snapshot_404s_when_its_composite_version_is_deleted() {
    let env = build_env().await;
    let file_id = env
        .svc
        .create_file_bare(&env.ctx, new_file("upload.bin"))
        .await
        .unwrap();

    let part_size = 5 * 1024 * 1024usize;
    let part1 = vec![b'a'; part_size];
    let part2 = vec![b'b'; 4096];
    let declared_size = (part1.len() + part2.len()) as u64;
    let plan = env
        .msvc
        .initiate_multipart_upload(
            &env.ctx,
            file_id,
            "application/octet-stream",
            declared_size,
            None,
            false,
        )
        .await
        .unwrap();
    for (n, part) in [(1, part1), (2, part2)] {
        put_part(
            &env.multipart_store,
            &env.backend,
            &plan,
            file_id,
            n,
            Bytes::from(part),
        )
        .await;
    }

    let first = env.complete(file_id, plan.upload_id).await;
    assert_eq!(
        first.hash_mode,
        HashMode::MultipartCompositeSha256,
        "sanity: 2 parts must produce a composite version with a real manifest"
    );
    assert!(
        first.manifest.is_some(),
        "sanity: a composite version must have a manifest"
    );

    env.svc
        .bind(&env.ctx, file_id, plan.version_id, None)
        .await
        .expect("first bind");
    let plan2 = env.add_session_with_part(file_id, b"BBBBB", false).await;
    env.complete(file_id, plan2.upload_id).await;
    env.svc
        .bind(
            &env.ctx,
            file_id,
            plan2.version_id,
            Some(&etag::content_etag(file_id, plan.version_id)),
        )
        .await
        .expect("rebind to V2");

    env.svc
        .delete_version(&env.ctx, file_id, plan.version_id)
        .await
        .expect("delete the now-unbound V1");

    // Replay the ORIGINAL complete for V1's upload_id: the persisted snapshot is still there, but
    // its version is gone -- must 404, never a response with `manifest: null`.
    let err = env
        .try_complete(file_id, plan.upload_id, None)
        .await
        .expect_err("replay of a completed session whose version was since deleted must fail");
    match err {
        DomainError::VersionNotFound {
            file_id: f,
            version_id: v,
        } => {
            assert_eq!(f, file_id);
            assert_eq!(v, plan.version_id);
        }
        other => panic!("expected VersionNotFound, got {other:?}"),
    }
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

#[tokio::test]
async fn complete_converges_after_in_progress_reset_instead_of_reinvoking_consumed_backend_handle()
{
    let env = build_env().await;
    let (file_id, plan) = env.session_with_part(b"AAAAA", false).await;
    let first = env.complete(file_id, plan.upload_id).await;

    // Model the bug's end state: the session looks like a never-attempted, fresh session, even
    // though the work already happened for real.
    reset_session_to_in_progress(&env.dsn, plan.upload_id).await;

    let retried = env
        .try_complete(file_id, plan.upload_id, None)
        .await
        .expect(
            "a retry after an in_progress reset must converge to the already-finalized version \
         instead of re-invoking the consumed backend handle",
        );

    assert_eq!(retried.version_id, first.version_id);
    assert_eq!(retried.size, first.size);
    assert_eq!(retried.content_hash, first.content_hash);

    let finished_session = env
        .multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session still exists");
    assert_eq!(
        finished_session.state,
        file_storage::domain::multipart::MultipartUploadState::Completed,
        "the retry must converge the session back to completed"
    );
}

/// FS-06: `complete` replays a `completed` session before it evaluates `If-Match`, so an honest
/// retry gets the persisted result whether it carries the now-stale precondition (F4) or none (the
/// negative control).
#[tokio::test]
async fn fs06_completed_retry_replays_with_or_without_stale_if_match() {
    let env = build_env().await;
    let dp = DataPlaneService::new(Arc::clone(&env.svc) as Arc<dyn DataPlanePort>);

    let ticket = env
        .svc
        .create_file(&env.ctx, new_file("drift.bin"), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &env.ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"initial content"),
    )
    .await
    .expect("put_content");
    let bound = env
        .svc
        .bind(&env.ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind initial content");
    let etag_of_initial_content = etag::etag_for(&bound).expect("bound file must have an etag");

    let plan = env
        .add_session_with_part(ticket.file_id, b"0123456789", true)
        .await;
    let completed_first = env
        .try_complete(
            ticket.file_id,
            plan.upload_id,
            Some(&etag_of_initial_content),
        )
        .await
        .expect("first complete: If-Match matches the initial content pointer");
    assert_eq!(completed_first.bind_state, BindState::Bound);

    // The first complete moved the file's ETag, so the first row's If-Match is now stale.
    for (row, if_match) in [
        (
            "retry with stale If-Match",
            Some(etag_of_initial_content.as_str()),
        ),
        ("retry without If-Match", None),
    ] {
        let retry = env
            .try_complete(ticket.file_id, plan.upload_id, if_match)
            .await
            .unwrap_or_else(|e| {
                panic!("{row}: a retry against an already-Completed session must replay: {e:?}")
            });
        assert_eq!(
            retry.version_id, completed_first.version_id,
            "{row}: the replayed result must match the original completion"
        );
    }
}
