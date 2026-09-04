//! Coverage for `MultipartService`'s idempotent-replay path
//! (`replay_completed`, `bind_state_for`) and the two other multipart-service
//! branches flagged as uncovered:
//!
//!   * `with_complete_lease_secs` — the completion-lease-duration builder.
//!   * `complete_multipart_upload`'s "unknown `upload_id`" guard.
//!   * the lease-takeover fast path in `assemble_and_finish_inner` that skips
//!     re-assembly when the version already reads as `available`.
//!
//! `replay_completed` first tries the persisted `complete_result` JSON
//! snapshot; only a session whose snapshot is missing (pre-snapshot rows, or
//! a crash between finalizing the version and persisting the snapshot) falls
//! through to rebuilding the response from the `file_versions` row. That
//! fallback is otherwise never exercised in the redesigned upload-flow
//! (`multipart_test.rs`'s idempotent-retry test always finds a snapshot), so
//! every test below clears `complete_result` via raw SQL after a real
//! `complete` to force the fallback deterministically — there is no
//! production API to produce a snapshot-less `completed` session other than
//! this exact crash window, and this is the same "tamper via raw SQL"
//! pattern `multipart_test.rs::tamper_request_hash` and
//! `content_hash_modes_test.rs` use for their own otherwise-unreachable rows.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use std::sync::Arc;

use bytes::Bytes;
use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use file_storage::domain::audit::{AuditEntry, AuditOperation};
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::error::DomainError;
use file_storage::domain::etag;
use file_storage::domain::multipart::BindState;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::content::hash_mode::HashMode;
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind, VersionStatus};

const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// A unique temp-file SQLite DB (mirrors `multipart_test.rs::build_db_with_dsn`)
/// — the raw DSN is kept around so tests below can open a second, independent
/// connection to tamper with rows there is no production API to produce.
async fn build_db_with_dsn() -> (Arc<DBProvider<DbError>>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-fs-mp-replay-test-{}.db",
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
    (Arc::new(DBProvider::new(db)), dsn)
}

/// Build `FileService` + `MultipartService` sharing one store/backend, plus
/// the raw DSN for tampering and the concrete `Store` for direct-repo calls
/// (`finalize_version`, `get_version`) that the tests below need but
/// `MultipartService` itself only exposes indirectly.
///
/// Installs a non-default `with_complete_lease_secs` so that builder (185-188
/// in `multipart_service.rs`) is actually exercised — the exact value is not
/// asserted on; the flows below fake an expired lease directly via
/// `acquire_multipart_complete_lease` rather than waiting out this duration.
async fn build_env() -> (
    Arc<FileService>,
    Arc<MultipartService>,
    Arc<dyn MultipartStore>,
    Arc<dyn StorageBackend>,
    Store,
    SecurityContext,
    String,
) {
    let (db, dsn) = build_db_with_dsn().await;
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
    let msvc = Arc::new(
        MultipartService::new(
            Arc::clone(&multipart_store),
            backends,
            Arc::clone(&authorizer),
            None,
            issuer,
            "http://sidecar.test".to_owned(),
            3600,
        )
        .with_complete_lease_secs(90),
    );
    (
        svc,
        msvc,
        multipart_store,
        backend,
        store,
        ctx(Uuid::now_v7()),
        dsn,
    )
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
        name: "upload.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// Same dance as `multipart_test.rs::simulate_sidecar_put_part`: write the
/// part bytes through the backend's native-multipart path, then persist the
/// part row exactly as the sidecar's SDK callback would.
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
        "test data must match the plan"
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
        .expect("upsert part");
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut acc, b| {
        write!(acc, "{b:02x}").expect("writing to a String cannot fail");
        acc
    })
}

/// `sea_orm`'s sqlite driver binds `Uuid` columns as a raw 16-byte BLOB (see
/// `multipart_test.rs::tamper_request_hash`'s comment) — every raw-SQL
/// tamper below has to match that with an `X'...'` literal, not a
/// hyphenated string.
fn uuid_blob(id: Uuid) -> String {
    format!("X'{}'", hex_encode(id.as_bytes()))
}

/// Run one raw-SQL statement against an independent connection to `dsn` and
/// assert it hit exactly the row the test set up — a silent no-op tamper
/// would make the rest of the test meaningless.
async fn exec_expect_one_row(dsn: &str, sql: &str) {
    let conn = Database::connect(dsn).await.expect("raw connect");
    let res = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("tamper SQL failed: {sql}: {e}"));
    assert_eq!(
        res.rows_affected(),
        1,
        "tamper SQL must hit exactly the one row the test prepared: {sql}"
    );
}

/// Force `replay_completed` to fall through the persisted-snapshot fast path
/// to its version-row fallback, by wiping the snapshot the real `complete`
/// just persisted.
async fn null_complete_result(dsn: &str, upload_id: Uuid) {
    exec_expect_one_row(
        dsn,
        &format!(
            "UPDATE multipart_uploads SET complete_result = NULL WHERE upload_id = {}",
            uuid_blob(upload_id)
        ),
    )
    .await;
}

/// `file_versions.hash_mode` carries a DB `CHECK` constraint restricting it
/// to the two real spellings, so a plain `UPDATE` to a bogus value is
/// rejected by SQLite itself before `HashMode::parse` ever runs — which is
/// exactly why this defensive branch in `replay_completed` is otherwise
/// unreachable. Model the corruption it guards against directly: disable
/// constraint enforcement with `PRAGMA ignore_check_constraints` (a
/// per-connection setting) and run the tamper inside one transaction, so
/// both statements are pinned to the same underlying connection regardless
/// of how the pool would otherwise hand out connections.
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

// -- with_complete_lease_secs + unknown upload_id (multipart_service.rs 185-188, 760) ---

/// `complete` against an `upload_id` that was never created must answer
/// `MultipartUploadNotFound`, not panic or fall through to some other error
/// — the very first `get_multipart_upload` lookup in `complete_multipart_upload`.
/// Also exercises `with_complete_lease_secs` via `build_env`'s builder chain.
#[tokio::test]
async fn complete_multipart_upload_unknown_upload_id_is_not_found() {
    let (svc, msvc, _store, _backend, _s, ctx, _dsn) = build_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    let bogus_upload_id = Uuid::now_v7();
    let err = msvc
        .complete_multipart_upload(&ctx, file_id, bogus_upload_id, None)
        .await
        .expect_err("an unknown upload_id must never succeed");
    match err {
        DomainError::MultipartUploadNotFound { upload_id } => {
            assert_eq!(upload_id, bogus_upload_id);
        }
        other => panic!("expected MultipartUploadNotFound, got {other:?}"),
    }
}

// -- replay_completed fallback: success (multipart_service.rs 972-1008, 1029-1031) ------

/// A `completed` session whose persisted `complete_result` snapshot is gone
/// (the pre-snapshot-crash scenario `replay_completed`'s fallback exists
/// for) must still replay the exact same result by rebuilding it from the
/// `file_versions` row — same version, size, hash, hash_mode, part_count,
/// manifest and (`auto_bind: false`) `BindState::Manual`.
#[tokio::test]
async fn replay_completed_fallback_rebuilds_result_from_version_row() {
    let (svc, msvc, multipart_store, backend, _store, ctx, dsn) = build_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            13,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"Hello, World!"),
    )
    .await;

    let first = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();

    null_complete_result(&dsn, plan.upload_id).await;

    let replay = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("fallback replay of a snapshot-less completed session must succeed")
        .unwrap_completed();

    assert_eq!(replay.version_id, first.version_id);
    assert_eq!(replay.size, first.size);
    assert_eq!(replay.hash_algorithm, first.hash_algorithm);
    assert_eq!(replay.content_hash, first.content_hash);
    assert_eq!(replay.hash_mode, first.hash_mode);
    assert_eq!(
        replay.hash_mode,
        HashMode::WholeSha256,
        "single part degenerates to whole-sha256"
    );
    assert_eq!(replay.part_count, first.part_count);
    assert_eq!(replay.manifest, first.manifest);
    assert_eq!(
        replay.bind_state,
        BindState::Manual,
        "auto_bind: false session was never bound"
    );
    assert_eq!(replay.etag, None);
    assert_eq!(replay.current_etag, None);
}

// -- replay_completed fallback error branches (multipart_service.rs 980-994) -------------

/// The fallback's `get_version` lookup returning `None` (the version row
/// disappeared) must surface as `VersionNotFound`, not a panic or a generic
/// database error — there is no production path that deletes a
/// `file_versions` row still referenced by a `completed` session, so this is
/// simulated directly via raw SQL.
#[tokio::test]
async fn replay_completed_fallback_errors_when_version_row_is_gone() {
    let (svc, msvc, multipart_store, backend, _store, ctx, dsn) = build_env().await;
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
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    let first_complete = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    // A replay only means anything once the original complete really finished.
    assert!(
        first_complete.size > 0,
        "the first complete must assemble a non-empty version"
    );

    null_complete_result(&dsn, plan.upload_id).await;
    exec_expect_one_row(
        &dsn,
        &format!(
            "DELETE FROM file_versions WHERE version_id = {}",
            uuid_blob(plan.version_id)
        ),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect_err("replay against a deleted version row must fail");
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

/// A version row that reads as anything other than `Available` (here forced
/// back to `pending`, mimicking a session whose finalize never actually
/// landed) must reject the replay with `MultipartUploadNotInProgress`
/// carrying the session's own state — never silently succeed with stale
/// data.
#[tokio::test]
async fn replay_completed_fallback_errors_when_version_not_available() {
    let (svc, msvc, multipart_store, backend, _store, ctx, dsn) = build_env().await;
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
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    let first_complete = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    // A replay only means anything once the original complete really finished.
    assert!(
        first_complete.size > 0,
        "the first complete must assemble a non-empty version"
    );

    null_complete_result(&dsn, plan.upload_id).await;
    exec_expect_one_row(
        &dsn,
        &format!(
            "UPDATE file_versions SET status = 'pending' WHERE version_id = {}",
            uuid_blob(plan.version_id)
        ),
    )
    .await;

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect_err("replay against a non-available version must fail");
    match err {
        DomainError::MultipartUploadNotInProgress { upload_id, state } => {
            assert_eq!(upload_id, plan.upload_id);
            assert_eq!(
                state, "completed",
                "reported state is the SESSION's own state"
            );
        }
        other => panic!("expected MultipartUploadNotInProgress, got {other:?}"),
    }
}

/// A `hash_mode` value on the version row that doesn't parse (DB corruption,
/// or a value written by a future version of the code) must surface as a
/// `Database` error rather than panicking or defaulting silently.
#[tokio::test]
async fn replay_completed_fallback_errors_on_unrecognized_hash_mode() {
    let (svc, msvc, multipart_store, backend, _store, ctx, dsn) = build_env().await;
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
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    let first_complete = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    // A replay only means anything once the original complete really finished.
    assert!(
        first_complete.size > 0,
        "the first complete must assemble a non-empty version"
    );

    null_complete_result(&dsn, plan.upload_id).await;
    set_hash_mode_bypassing_check(&dsn, plan.version_id, "bogus-mode").await;

    let err = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect_err("an unrecognized hash_mode must fail, not panic or silently pick one");
    assert!(
        matches!(err, DomainError::Database { .. }),
        "expected Database, got {err:?}"
    );
}

// -- bind_state_for (multipart_service.rs 1015-1031), via the fallback replay -----------

/// `bind_state_for`'s `Bound` branch: the version is still the file's
/// current content at replay time, so the fallback must report `Bound` with
/// a freshly-derived content ETag — exactly like the first-time complete's
/// own (separate) bind-state computation, proving the two stay consistent.
#[tokio::test]
async fn replay_completed_fallback_reports_bound_state_when_still_bound() {
    let (svc, msvc, multipart_store, backend, _store, ctx, dsn) = build_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            5,
            None,
            None,
            true,
        )
        .await
        .unwrap();
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    let first = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(
        first.bind_state,
        BindState::Bound,
        "auto_bind session must self-bind"
    );

    null_complete_result(&dsn, plan.upload_id).await;

    let replay = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("fallback replay of a still-bound version must succeed")
        .unwrap_completed();
    assert_eq!(replay.bind_state, BindState::Bound);
    assert_eq!(
        replay.etag,
        Some(etag::content_etag(file_id, plan.version_id)),
        "Bound branch must derive the content ETag from (file_id, version_id)"
    );
    assert_eq!(replay.current_etag, None);
}

/// `bind_state_for`'s `Conflict` branch: an `auto_bind` session's version was
/// bound at complete time, but the file's content pointer has since moved to
/// a *different* version (a later, unrelated rebind) — the fallback must
/// report `Conflict` with the CURRENT content ETag (not the stale one this
/// session's own version would produce), so a client knows a manual rebind
/// is needed rather than trusting a bind that no longer holds.
#[tokio::test]
async fn replay_completed_fallback_reports_conflict_when_content_rebound_elsewhere() {
    let (svc, msvc, multipart_store, backend, _store, ctx, dsn) = build_env().await;
    let file_id = svc.create_file_bare(&ctx, new_file()).await.unwrap();

    // Session A: auto_bind, binds the file to version A.
    let plan_a = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            5,
            None,
            None,
            true,
        )
        .await
        .unwrap();
    let session_a = multipart_store
        .get_multipart_upload(plan_a.upload_id)
        .await
        .unwrap()
        .expect("session a");
    let path_a = format!("/{file_id}/{}", plan_a.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_a,
        &path_a,
        &session_a.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    let completed_a = msvc
        .complete_multipart_upload(&ctx, file_id, plan_a.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed_a.bind_state, BindState::Bound);

    // Session B: a second, independent (manual) upload on the same file,
    // then an explicit rebind moves `files.content_id` to version B.
    let plan_b = msvc
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
    let session_b = multipart_store
        .get_multipart_upload(plan_b.upload_id)
        .await
        .unwrap()
        .expect("session b");
    let path_b = format!("/{file_id}/{}", plan_b.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan_b,
        &path_b,
        &session_b.backend_upload_handle,
        1,
        Bytes::from_static(b"BBBBB"),
    )
    .await;
    let completed_b = msvc
        .complete_multipart_upload(&ctx, file_id, plan_b.upload_id, None)
        .await
        .unwrap()
        .unwrap_completed();
    assert_eq!(completed_b.bind_state, BindState::Manual);
    svc.bind(&ctx, file_id, plan_b.version_id, Some("*"))
        .await
        .expect("rebind to version B");

    // Replay session A's (now stale) auto-bind: the file no longer points at
    // version A, so this must read as a Conflict, not a (wrong) Bound.
    null_complete_result(&dsn, plan_a.upload_id).await;
    let replay_a = msvc
        .complete_multipart_upload(&ctx, file_id, plan_a.upload_id, None)
        .await
        .expect("fallback replay after an external rebind must still succeed")
        .unwrap_completed();
    assert_eq!(replay_a.bind_state, BindState::Conflict);
    assert_eq!(replay_a.etag, None);
    let file = svc.get_file(&ctx, file_id).await.expect("file");
    assert_eq!(
        replay_a.current_etag,
        Some(etag::content_etag(
            file_id,
            file.content_id.expect("bound to B")
        )),
        "Conflict branch must report the file's CURRENT etag, pointing at B"
    );
}

// -- lease takeover fast path (multipart_service.rs 1088-1090) --------------------------

/// Lease-takeover fast path in `assemble_and_finish_inner`: a completer that
/// crashed AFTER `finalize_version` (the version is already `available`) but
/// BEFORE flipping the session to `completed` leaves the session stuck in
/// `completing`. The next `complete`, taking over the (now expired) lease,
/// must see the version is already there and just finish the state machine
/// (replay + `finish_session`) rather than re-running the whole assembly.
///
/// The crash window is reproduced directly: call `finalize_version` through
/// the store (exactly what `assemble_and_finish_inner` itself would have
/// done) without ever calling `complete_multipart_upload`, then fake the
/// dead lease the same way `multipart_test.rs::complete_takes_over_expired_lease_and_finishes`
/// does via `acquire_multipart_complete_lease` — no sleeping, no racing.
#[tokio::test]
async fn complete_takeover_finishes_without_reassembly_when_version_already_available() {
    let (svc, msvc, multipart_store, backend, store, ctx, _dsn) = build_env().await;
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
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .unwrap()
        .expect("session");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    simulate_sidecar_put_part(
        &multipart_store,
        &backend,
        &plan,
        &backend_path,
        &session.backend_upload_handle,
        1,
        Bytes::from_static(b"AAAAA"),
    )
    .await;
    let part = store
        .list_multipart_parts(plan.upload_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("one part");

    // Simulate the crashed completer's own finalize — the version becomes
    // `available` with no session-state change at all (still `in_progress`).
    let finalize_audit = AuditEntry::success(
        ctx.subject_tenant_id(),
        "user",
        ctx.subject_id(),
        Some(file_id),
        AuditOperation::FinalizeVersion,
        serde_json::json!({ "version_id": plan.version_id }),
    );
    let outcome = multipart_store
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

    // Now fake the dead completer's lease: CAS from `in_progress` straight to
    // `completing` with a lease that already expired.
    let now = time::OffsetDateTime::now_utc();
    let acquired = multipart_store
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

    // The next complete takes over and, finding the version already
    // available, must just finish -- not re-run assembly (which would fail
    // here anyway: this backend's multipart handle was never completed).
    let completed = msvc
        .complete_multipart_upload(&ctx, file_id, plan.upload_id, None)
        .await
        .expect("takeover onto an already-finalized version must succeed")
        .unwrap_completed();
    assert_eq!(completed.version_id, plan.version_id);
    assert_eq!(completed.size, part.size);

    let finished_session = multipart_store
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

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .unwrap()
        .expect("version");
    assert_eq!(version.status, VersionStatus::Available);
}
