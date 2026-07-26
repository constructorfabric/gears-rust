// Created: 2026-07-27 by Constructor Tech
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
//! PostgreSQL concurrency harness for the file-storage DB-behavior audit
//! (Step 4 of the DB-behavior audit program -- see
//! `docs/toolkit_unified_system/14_db_behavior_testing.md`, methodology
//! validated against `resource-group`'s own PG suite on
//! `audit/rg-db-behavior`).
//!
//! Same reasons real PostgreSQL is needed here as for resource-group:
//! SQLite's own "SERIALIZABLE" is a whole-database writer lock, not
//! row/predicate-level SSI, and file-storage's concurrency-safety strategy is
//! single-statement CAS `UPDATE ... WHERE` predicates whose *interleaving*
//! behavior under a real connection pool and real wall-clock timing cannot be
//! faithfully exercised against an in-memory SQLite connection. Runs for
//! real, automatically, as part of a normal
//! `cargo test -p cf-gears-file-storage` -- no `#[ignore]`, no environment
//! variable to remember to set; every test skips itself gracefully (passes,
//! with a stderr message) if Docker isn't reachable.
//!
//! ## Running locally
//!
//! ```sh
//! cargo test -p cf-gears-file-storage --test pg_concurrency_test
//! ```
//!
//! ## Scenarios
//!
//! - `f1_*` -- known defect FS-01/F1: a multipart-initiate failure (capability
//!   reject on a non-`multipart_native` backend, or a backend-level
//!   initiation error) leaves a version-less orphan `files` row that a real
//!   sweep pass does not reclaim.
//! - `f2_*` -- known defect FS-02/F2 (the audit's single most severe finding):
//!   completion is not owner-fenced end-to-end. Two completers, A and B, race
//!   the same session's lease; deterministic checkpoints (`tokio::sync::
//!   Notify` gates threaded through a `MultipartStore` decorator, not
//!   `sleep`-based timing) force the exact interleaving `tmp-review0.md`
//!   describes: stale A wins the version-finalize CAS, B's own (redundant)
//!   finalize attempt then fails and releases B's *own* lease (owner-scoped,
//!   but the session is still `completing` at that moment) back to
//!   `in_progress`, and A's own `finish_session` CAS then fails too (the
//!   state it expected, `completing`, is gone) -- both callers see an error,
//!   even though the content was, in fact, correctly finalized and bound.
//!   The session is left stranded at `in_progress` with no live lease and no
//!   missing parts, so a third caller re-attempts the (already-done)
//!   assembly, fails again (the specific error shape varies -- a DB conflict
//!   or a backend "handle already consumed" error, depending on how far the
//!   redundant attempt gets -- but it always fails), and re-strands it --
//!   this repeats until `expires_at` passes and the background sweep aborts
//!   the session, permanently, with the content already live underneath it.
//! - `f9_*` -- known defect FS-04/F9: multipart auto-bind's CAS target is
//!   whatever `content_id` was observed at the *start* of `complete` (not
//!   `IS NULL`, unlike the single-part path), so a legitimate rebind that
//!   happened *before* an auto-bind `complete` with no `If-Match` is silently
//!   overwritten. This is a **sequential temporal gap** (the exploitable
//!   window is the ordinary, often long, user-driven span between multipart
//!   *initiate* and its *complete*, not a tight concurrent race), so unlike
//!   F2 this scenario does not need barrier/gate synchronization to
//!   reproduce -- it is deterministic by construction. Included here (run
//!   against real PostgreSQL, not just the SQLite unit-level pin in
//!   `db_behavior_audit_test.rs`) for parity with the other scenarios and to
//!   confirm the same CAS shape holds under the production dialect. A
//!   negative control shows supplying `If-Match` (the fix already available
//!   to a careful caller) correctly turns the clobber into a clean
//!   `PreconditionFailed`.
//! - `f10_*` -- known defect FS-05/F10 ("second path into F1"): within a
//!   single `run_sweep()` call, step 1 (`sweep_abandoned_pending`) reclaims
//!   an abandoned pending version whose backing multipart session is
//!   *expired-but-still-`in_progress`* -- `has_in_progress_for_file` blocks
//!   the parent-file deletion at that moment (the session hasn't been
//!   aborted by step 2 yet), but the version row is deleted anyway. Step 2
//!   then aborts the session. The parent `files` row survives this sweep
//!   pass as a version-less orphan, and -- because the orphan-file check
//!   only ever runs as a side effect of deleting *a version* -- no later
//!   sweep pass ever revisits it: a second `run_sweep()` call confirms the
//!   file is still stuck. Deterministic by construction (backdated
//!   timestamps, no barrier needed), included here for parity and to confirm
//!   the same defect holds against a real PostgreSQL FK/cascade dialect.
//! - `invariant_checker_*` -- the post-state invariant checker required by
//!   the audit: no version-less `files` rows outside a documented window (a
//!   window the audit's own findings show is NOT actually bounded --
//!   demonstrated directly against the f1/f10 fixtures above), every
//!   non-`NULL` `files.content_id` points at an `available` version, and no
//!   `pending` version older than a threshold still has a live backing
//!   session. Usage-invariants are out of scope for an executable check --
//!   see the module doc's own note below.
//!
//! ## Usage-invariant note (Runtime Caveat, mirrored from `tmp-review0.md`)
//!
//! `gear.rs` wires `FileService`/`MultipartService`/`CleanupEngine` with
//! `usage_reporter: None` in the shipped build (`gear.rs:189-217,230,236,264`,
//! confirmed by reading the gear wiring) -- no usage delta is emitted at all
//! today. There is therefore no live usage-invariant to *check* against a
//! real reporter in this suite; the accounting defects this would otherwise
//! surface (F1/F3/F10's overcounts/undercounts) are latent until a reporter
//! is wired, exactly as `tmp-review0.md` notes. This suite does not simulate
//! a reporter or assert on `report_usage`'s fire-and-forget calls -- doing so
//! would test a code path with no observable effect in production today and
//! risks masking the real gap (documented as latent, not "checked and fine").

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::cleanup::{CleanupConfig, CleanupEngine};
use file_storage::domain::error::DomainError;
use file_storage::domain::multipart::{BindState, MultipartPart};
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::policy::{PolicyScope, StoredPolicy};
use file_storage::domain::ports::{
    AutoBindOnFinalize, CleanupStore, DataPlanePort, FinalizeVersionOutcome, MultipartStore,
};
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend,
};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::VersionStatus;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sea_orm_migration::MigratorTrait;
use testcontainers::{ContainerRequest, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use tokio::sync::{Mutex, Notify, OnceCell};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

/// Serializes every test in this file against every other one: they share
/// one PostgreSQL database (started once, lazily), and while most scenarios
/// here use uniquely-generated ids (no cross-test invariant), running them
/// fully concurrently would make `eprintln!` diagnostics from different
/// scenarios interleave confusingly and (for the `f2_*`/`f10_*` scenarios,
/// which depend on real wall-clock lease/session expiry) could make one
/// test's real sleep windows overlap another's timing-sensitive assertions
/// under CI load. Mirrors resource-group's own `PG_TEST_LOCK`.
static PG_TEST_LOCK: Mutex<()> = Mutex::const_new(());

struct PgFixture {
    dsn: String,
    _container: testcontainers::ContainerAsync<Postgres>,
}

static PG: OnceCell<Option<Arc<PgFixture>>> = OnceCell::const_new();
static MIGRATIONS_DONE: OnceCell<()> = OnceCell::const_new();

/// Bring up (once per process) a `testcontainers` PostgreSQL, mirroring
/// resource-group's own `pg_concurrency_test.rs::shared_pg`. Returns `None`
/// if Docker isn't reachable -- callers treat that as a graceful skip.
async fn shared_pg() -> Option<Arc<PgFixture>> {
    PG.get_or_init(|| async {
        let request = ContainerRequest::from(Postgres::default())
            .with_tag("16-alpine")
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app");
        match request.start().await {
            Ok(container) => match container.get_host_port_ipv4(5432).await {
                Ok(port) => Some(Arc::new(PgFixture {
                    dsn: format!("postgres://user:pass@127.0.0.1:{port}/app"),
                    _container: container,
                })),
                Err(e) => {
                    eprintln!(
                        "skipping PostgreSQL concurrency tests: container started but its \
                         port could not be resolved ({e}). Is Docker healthy?"
                    );
                    None
                }
            },
            Err(e) => {
                eprintln!(
                    "skipping PostgreSQL concurrency tests: could not start a PostgreSQL \
                     container via testcontainers ({e}). Install/start Docker to run these \
                     for real -- see this file's module docs."
                );
                None
            }
        }
    })
    .await
    .clone()
}

/// Connect to the shared PostgreSQL fixture and ensure file-storage's
/// migrations have run against it. Returns `None` when Docker isn't
/// available.
async fn pg_db() -> Option<Arc<DBProvider<DbError>>> {
    let fixture = shared_pg().await?;
    let opts = ConnectOpts {
        max_conns: Some(10),
        min_conns: Some(2),
        ..Default::default()
    };
    let db = connect_db(&fixture.dsn, opts)
        .await
        .expect("connect to the testcontainers PostgreSQL");
    MIGRATIONS_DONE
        .get_or_init(|| async {
            run_migrations_for_testing(&db, Migrator::migrations())
                .await
                .expect("run file-storage migrations against PostgreSQL");
        })
        .await;
    Some(Arc::new(DBProvider::new(db)))
}

/// Shorthand for the common test-entry sequence, mirroring resource-group's
/// own macro of the same name and purpose.
macro_rules! pg_db_or_skip {
    () => {{
        let _guard = PG_TEST_LOCK.lock().await;
        match pg_db().await {
            Some(db) => (db, _guard),
            None => return,
        }
    }};
}

// =========================================================================
// Shared construction helpers
// =========================================================================

/// `Display`-based diagnostic summary of a fallible outcome, for `eprintln!`
/// (avoids `clippy::use_debug` -- mirrors resource-group's own
/// `describe_membership_result` in its `pg_concurrency_test.rs`).
fn describe_result<T>(r: &Result<T, DomainError>) -> String {
    match r {
        Ok(_) => "Ok".to_owned(),
        Err(e) => format!("Err({e})"),
    }
}

/// `Display`-based diagnostic summary of a [`file_storage::domain::cleanup::SweepResult`]
/// (its fields are all plain integers; this just avoids reaching for
/// `clippy::use_debug`-denied `{:?}` on the struct itself).
fn describe_sweep(r: &file_storage::domain::cleanup::SweepResult) -> String {
    format!(
        "pending_deleted={} files_deleted={} multipart_aborted={} retention_deleted={} idempotency_deleted={}",
        r.abandoned_pending_deleted,
        r.abandoned_files_deleted,
        r.expired_multipart_aborted,
        r.retention_expired_deleted,
        r.idempotency_keys_deleted,
    )
}

fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

fn new_file() -> file_storage_sdk::NewFile {
    file_storage_sdk::NewFile {
        owner_kind: file_storage_sdk::OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "pg-audit.bin".to_owned(),
        gts_file_type: toolkit_gts::gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~")
            .to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

fn service_config() -> ServiceConfig {
    ServiceConfig {
        default_url_ttl_secs: 3600,
        sidecar_base_url: "http://sidecar.test".to_owned(),
        default_page_size: 50,
        max_page_size: 1000,
        idempotency_ttl_secs: 86400,
    }
}

fn make_file_service(store: Store, backends: BackendRegistry) -> Arc<FileService> {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    Arc::new(FileService::new(
        store,
        backends,
        issuer,
        authorizer,
        service_config(),
        None,
        None,
    ))
}

fn make_multipart_service(
    store: Arc<dyn MultipartStore>,
    backends: BackendRegistry,
    complete_lease_secs: i64,
) -> Arc<MultipartService> {
    let issuer = Arc::new(Issuer::generate(3600).expect("issuer"));
    let authorizer: Arc<dyn file_storage::domain::authz::Authorizer> =
        Arc::new(TenantOnlyAuthorizer);
    Arc::new(
        MultipartService::new(
            store,
            backends,
            authorizer,
            None,
            issuer,
            "http://sidecar.test".to_owned(),
            3600,
        )
        .with_complete_lease_secs(complete_lease_secs),
    )
}

fn make_engine(store: Store, backends: BackendRegistry, orphan_grace_secs: u64) -> CleanupEngine {
    let cleanup_store: Arc<dyn CleanupStore> = Arc::new(store);
    CleanupEngine::new(cleanup_store, backends, CleanupConfig { orphan_grace_secs })
}

/// Drive every part in `plan` through the backend + `MultipartStore`
/// directly, all-zero filler bytes -- mirrors
/// `tests/common/mod.rs::simulate_all_parts`, duplicated here (rather than
/// pulled in via `mod common`'s own copy) because this file's helpers build
/// their own bespoke `MultipartService`/`Store` combinations per scenario,
/// not `common::Services`.
async fn simulate_all_parts(
    multipart_store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &file_storage::domain::multipart::MultipartPlan,
    file_id: Uuid,
) {
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .expect("get_multipart_upload")
        .expect("session must exist");
    let backend_path = format!("/{file_id}/{}", plan.version_id);
    for part in &plan.parts {
        let data = Bytes::from(vec![
            0u8;
            usize::try_from(part.size).expect("part size fits")
        ]);
        let (backend_etag, part_hash) = backend
            .upload_part(
                &backend_path,
                &session.backend_upload_handle,
                part.part_number,
                part.offset,
                data,
            )
            .await
            .expect("backend upload_part");
        let size = i64::try_from(part.size).expect("part size fits in i64");
        let part_number_i32 = i32::try_from(part.part_number).expect("part_number fits in i32");
        multipart_store
            .upsert_multipart_part(
                plan.upload_id,
                part_number_i32,
                &backend_etag,
                part_hash,
                size,
                OffsetDateTime::now_utc(),
            )
            .await
            .expect("upsert_multipart_part");
    }
}

// =========================================================================
// F1 / FS-01: orphan file when multipart initiation fails, real sweep does
// not reclaim it
// =========================================================================

/// Capability-reject half of F1: `LocalFsBackend` never advertises
/// `multipart_native` (confirmed by reading `local_fs.rs`'s
/// `BackendCapabilities::default()`), so initiate is rejected before any
/// pending version is ever created. A real `run_sweep()` pass against
/// PostgreSQL does not reclaim the resulting version-less orphan `files`
/// row -- `sweep_abandoned_pending` only ever visits rows returned by
/// `list_abandoned_pending_versions`, and there is no pending version here
/// at all to trigger it.
///
/// FS-01/F1 is now FIXED, but at the orchestration layer
/// (`api/rest/handlers.rs::create_file`'s multipart branch calls
/// `FileService::compensate_failed_multipart_initiate` on exactly this
/// failure) -- correctly: only that caller knows "this file was just
/// created together with this specific initiate attempt." This test calls
/// `create_file_bare` + `initiate_multipart_upload` directly, the same raw
/// sequence the handler makes *before* its own error-handling branch, so it
/// still (by design) shows the sweep alone not reclaiming the orphan. See
/// `f1_capability_reject_with_compensation_reclaims_orphan` below for the
/// fixed end-to-end flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f1_capability_reject_orphan_survives_real_sweep() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("fs", tmp.keep()));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "fs").expect("registry");
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());
    let msvc = make_multipart_service(multipart_store, backends.clone(), 120);
    let engine = make_engine(store.clone(), backends, 0); // grace=0: everything eligible immediately

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let file_id = svc
        .create_file_bare(&ctx, new_file())
        .await
        .expect("create_file_bare commits the bare file row");

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            20,
            Some(10),
            None,
            false,
        )
        .await
        .expect_err("local-fs backend does not advertise multipart_native");
    assert!(matches!(err, DomainError::MultipartNotSupported { .. }));

    let result = engine.run_sweep().await;
    eprintln!(
        "f1_capability_reject_orphan_survives_real_sweep: sweep result = {}",
        describe_sweep(&result)
    );

    let still_there = svc.get_file(&ctx, file_id).await;
    assert!(
        still_there.is_ok(),
        "expected the orphaned bare file to survive a real sweep pass with no compensation \
         invoked (no pending version was ever created to trigger the sweep's own orphan-parent \
         reclamation path) -- got: {still_there:?}"
    );
}

/// FS-01/F1 fix, end-to-end, against real PostgreSQL: mirrors exactly what
/// `api/rest/handlers.rs::create_file`'s multipart branch now does on an
/// initiate failure -- call `compensate_failed_multipart_initiate` with the
/// same `file_id`, then confirm the orphan is gone (no sweep needed at
/// all).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f1_capability_reject_with_compensation_reclaims_orphan() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let tmp = tempfile::tempdir().expect("tempdir");
    let backend: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("fs", tmp.keep()));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "fs").expect("registry");
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());
    let msvc = make_multipart_service(multipart_store, backends, 120);

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let file_id = svc
        .create_file_bare(&ctx, new_file())
        .await
        .expect("create_file_bare commits the bare file row");
    msvc.initiate_multipart_upload(
        &ctx,
        file_id,
        "application/octet-stream",
        20,
        Some(10),
        None,
        false,
    )
    .await
    .expect_err("local-fs backend does not advertise multipart_native");

    svc.compensate_failed_multipart_initiate(&ctx, file_id)
        .await;

    let gone = svc.get_file(&ctx, file_id).await;
    assert!(
        matches!(gone, Err(DomainError::FileNotFound { .. })),
        "FS-01/F1 fix: the compensating delete must reclaim the orphan file against real \
         PostgreSQL too, got: {gone:?}"
    );
}

/// A `StorageBackend` decorator whose `initiate_multipart` always fails,
/// even though `capabilities()` (delegated to the inner backend) genuinely
/// advertises `multipart_native: true` -- the *other* half of F1
/// (`tmp-review0.md`: "capability rejection... or backend-initiation
/// failure"), distinct from the capability-reject half above.
struct FailingInitiateBackend {
    inner: Arc<dyn StorageBackend>,
}

#[async_trait]
impl StorageBackend for FailingInitiateBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self) -> file_storage::infra::backend::BackendCapabilities {
        self.inner.capabilities()
    }
    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.inner.put(path, bytes).await
    }
    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.inner.get(path).await
    }
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<futures::stream::BoxStream<'_, std::io::Result<Bytes>>, DomainError> {
        self.inner.get_stream(path).await
    }
    async fn get_range(
        &self,
        path: &str,
        range: file_storage_sdk::ByteRange,
    ) -> Result<Bytes, DomainError> {
        self.inner.get_range(path, range).await
    }
    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }
    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }
    async fn initiate_multipart(&self, _path: &str) -> Result<String, DomainError> {
        Err(DomainError::database(
            "simulated backend-initiation failure (e.g. an S3 CreateMultipartUpload error)",
        ))
    }
    async fn upload_part(
        &self,
        path: &str,
        upload_handle: &str,
        part_number: u32,
        part_offset: u64,
        data: Bytes,
    ) -> Result<(String, Vec<u8>), DomainError> {
        self.inner
            .upload_part(path, upload_handle, part_number, part_offset, data)
            .await
    }
    async fn complete_multipart(
        &self,
        path: &str,
        upload_handle: &str,
        parts: &[file_storage::infra::backend::MultipartCompletionPart],
    ) -> Result<(file_storage::infra::content::hash_mode::Manifest, [u8; 32]), DomainError> {
        self.inner
            .complete_multipart(path, upload_handle, parts)
            .await
    }
    async fn abort_multipart(&self, path: &str, upload_handle: &str) -> Result<(), DomainError> {
        self.inner.abort_multipart(path, upload_handle).await
    }
    async fn list_paths(&self) -> Result<Vec<String>, DomainError> {
        self.inner.list_paths().await
    }
}

/// Backend-initiation-failure half of F1: the backend genuinely advertises
/// `multipart_native`, but its `initiate_multipart` call itself fails (a
/// transient S3-side error, say) -- same orphan outcome as the
/// capability-reject half, confirming F1 is not specific to the capability
/// check but to the absence of any rollback/compensation around
/// `create_file_bare` + initiate as a pair.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f1_backend_initiation_failure_orphan_survives_real_sweep() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let inner: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backend: Arc<dyn StorageBackend> = Arc::new(FailingInitiateBackend { inner });
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());
    let msvc = make_multipart_service(multipart_store, backends.clone(), 120);
    let engine = make_engine(store.clone(), backends, 0);

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let file_id = svc
        .create_file_bare(&ctx, new_file())
        .await
        .expect("create_file_bare");

    let err = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            20,
            Some(10),
            None,
            false,
        )
        .await
        .expect_err("the backend's initiate_multipart is rigged to fail");
    eprintln!("f1_backend_initiation_failure: initiate error = {err}");

    let result = engine.run_sweep().await;
    eprintln!(
        "f1_backend_initiation_failure_orphan_survives_real_sweep: sweep result = {}",
        describe_sweep(&result)
    );

    let still_there = svc.get_file(&ctx, file_id).await;
    assert!(
        still_there.is_ok(),
        "known defect FS-01/F1 (backend-initiation-failure half): expected the orphaned bare \
         file to survive a real sweep pass, got: {still_there:?}"
    );
}

// =========================================================================
// F2 / FS-02: completion is not owner-fenced end-to-end -- deterministic
// two-completer race via gated `MultipartStore` checkpoints
// =========================================================================

/// Which of the two concurrent completers (`A`, the stale winner-then-loser,
/// or `B`, the taker-over) a given [`GatedMultipartStore`] handle plays.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    A,
    B,
}

/// A `MultipartStore` decorator that pauses at two specific checkpoints via
/// `tokio::sync::Notify` gates, instead of `sleep`-based timing, to force
/// the *exact* interleaving `tmp-review0.md`'s F2 stranding counterexample
/// describes -- deterministically, not "reproduces in N/M trials" the way a
/// pure wall-clock race would (a real completer's assembly duration is not
/// something a test can pin down to the millisecond, and -- confirmed
/// empirically while building this harness -- even gating *when a call
/// starts* is not enough to pin down *which side's commit lands first*: an
/// already-running task and a freshly-woken one can race the scheduler in
/// either direction).
///
/// Two handles (one per role) share the same two `Notify`s and wrap the
/// *same* real, PostgreSQL-backed inner store:
///
/// - Role `B`'s **first** `get_version` call (the takeover fast-path check
///   in `assemble_and_finish_inner`) notifies `b_checked_pending` right
///   after it returns the (still-`pending`, since role `A` is gated below)
///   real value.
/// - Role `A`'s `finalize_version` call **waits** on `b_checked_pending`
///   before delegating -- guaranteeing `A`'s real finalize commit cannot
///   *start* before `B` has already observed the pre-finalize state, no
///   matter how much real wall-clock time separates `A`'s lease acquisition
///   (which must still genuinely expire before `B`'s own
///   `acquire_complete_lease` CAS will match) from `B`'s own start -- and
///   notifies `a_finalized` right after its own commit *returns*.
/// - Role `B`'s `finalize_version` call **waits** on `a_finalized` before
///   even starting its own (redundant, doomed-to-lose) attempt -- this is
///   the gate that makes A's *win* deterministic, not just A's *start*:
///   without it, B's task (already running, several steps into its own
///   reassembly) can race ahead of A's freshly-woken one and commit first,
///   which is a real, also-interesting outcome (the stale completer, not
///   the fresh one, ends up needing to converge instead) but not the
///   specific interleaving this test exists to pin down.
///
/// Post-fix (FS-02 remediation, see `docs/analysis/DB_BEHAVIOR_AUDIT.md`
/// §5), B's lost `finalize_version` no longer errors-and-releases the
/// lease -- it converges (re-derives the response and calls
/// `finish_session` directly, same as the takeover fast path) -- so there
/// is no third gate here anymore: nothing needs to hold A's own
/// `finish_session` call back, since the actual race that remains (both A
/// and B now separately racing to call `finish_session`) is exactly the
/// race `finish_session`'s own CAS-then-converge logic is designed to
/// resolve gracefully either way.
///
/// `tokio::sync::Notify::notify_one` buffers a permit if no waiter is
/// registered yet, so there is no lost-wakeup risk regardless of which side
/// reaches its gate first in wall-clock terms.
struct GatedMultipartStore {
    inner: Arc<dyn MultipartStore>,
    role: Role,
    b_checked_pending: Arc<Notify>,
    /// Notified by role A's `finalize_version` right after its real commit
    /// returns (success or not) -- role B's own `finalize_version` waits on
    /// this before even starting, so which side wins the real CAS is
    /// deterministic rather than left to the scheduler.
    a_finalized: Arc<Notify>,
    b_first_get_version_seen: Arc<AtomicBool>,
}

#[async_trait]
impl MultipartStore for GatedMultipartStore {
    async fn require_file(
        &self,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<file_storage_sdk::File, DomainError> {
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

    async fn create_multipart_upload(
        &self,
        upload_id: Uuid,
        file_id: Uuid,
        version_id: Uuid,
        backend_upload_handle: &str,
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
    ) -> Result<Option<file_storage::domain::multipart::MultipartUploadSession>, DomainError> {
        self.inner.get_multipart_upload(upload_id).await
    }

    async fn get_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
    ) -> Result<Option<file_storage_sdk::FileVersion>, DomainError> {
        let result = self.inner.get_version(file_id, version_id).await;
        if self.role == Role::B && !self.b_first_get_version_seen.swap(true, Ordering::SeqCst) {
            // This is B's takeover fast-path check -- notify A's gated
            // finalize_version that it may now proceed, only after this
            // (real, pre-finalize) read has already completed.
            self.b_checked_pending.notify_one();
        }
        result
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

    async fn finalize_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        hash_value: Vec<u8>,
        hash_mode: file_storage::infra::content::hash_mode::HashMode,
        part_count: Option<i32>,
        manifest: Option<String>,
        validated_mime: Option<String>,
        audit: file_storage::domain::audit::AuditEntry,
        auto_bind: Option<AutoBindOnFinalize>,
    ) -> Result<FinalizeVersionOutcome, DomainError> {
        if self.role == Role::A {
            self.b_checked_pending.notified().await;
        } else {
            // Role B: B's own (redundant, ultimately-losing) finalize attempt
            // must not even START until A's has definitely finished --
            // otherwise which of the two physically wins the real CAS
            // commit is at the mercy of tokio's scheduler (a freshly-woken
            // task vs. one that never yielded since triggering the wake),
            // which is not the specific interleaving this test exists to
            // pin down. Waiting here (rather than relying on A merely
            // having "started" its call) makes A's win deterministic.
            self.a_finalized.notified().await;
        }
        let result = self
            .inner
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
            .await;
        if self.role == Role::A {
            self.a_finalized.notify_one();
        }
        result
    }

    async fn complete_multipart_upload(
        &self,
        upload_id: Uuid,
        result_json: &str,
        audit: file_storage::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner
            .complete_multipart_upload(upload_id, result_json, audit)
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
        audit: file_storage::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.abort_multipart_upload(upload_id, audit).await
    }

    async fn delete_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        audit: file_storage::domain::audit::AuditEntry,
    ) -> Result<bool, DomainError> {
        self.inner.delete_version(file_id, version_id, audit).await
    }
}

/// FS-02/F2 fix verification -- the audit's single most severe finding, and
/// the one the coordinator specifically flagged for extra scrutiny ("the
/// earlier proposed one-line fix is insufficient"). This test used to
/// reproduce a genuine stranding (see `docs/analysis/DB_BEHAVIOR_AUDIT.md`
/// §5 for the full before/after and git history for the original repro);
/// after the fix (`multipart_service.rs::assemble_and_finish_inner`: a lost
/// finalize CAS now checks whether the version is `Available` -- i.e.
/// someone else's finalize already won -- and, if so, converges via the
/// same `replay_completed` + `finish_session` path the takeover fast-path
/// already used, instead of unconditionally erroring and releasing the
/// lease), the exact same deterministic interleaving now converges cleanly
/// instead. Mechanism:
///
/// 1. A acquires the completion lease (fresh) with a 1-second lease.
/// 2. The test waits >1s of real wall-clock time -- A's lease genuinely
///    expires (this is not simulated; `acquire_multipart_complete_lease`'s
///    CAS compares against real `OffsetDateTime::now_utc()`).
/// 3. B calls complete: takes over the (really) expired lease
///    (`takeover = true`), then hits its gated `get_version` takeover-check
///    -- the version is still `pending` (A is gated below, hasn't finalized
///    yet) -- so B does NOT take the "already finalized, just finish" fast
///    path; B proceeds through its own full (redundant) reassembly.
/// 4. A's gated `finalize_version` was released the instant B's check above
///    completed; A -- which has zero remaining `.await`s before that call --
///    wins the real finalize CAS: the version flips `pending -> available`
///    (+ bind, for an `auto_bind` session), for real, in PostgreSQL.
/// 5. B, having finished its own (redundant, slower) reassembly, calls its
///    own `finalize_version` -- sees `updated = false` (no longer `pending`)
///    -- **post-fix**, checks the version's real status, sees `Available`,
///    and converges: re-derives the response via `replay_completed` and
///    calls `finish_session` directly, same as a genuine takeover fast path.
/// 6. Both A and B now separately race to call `finish_session` (the
///    `state = 'completing' -> 'completed'` CAS); whichever gets there
///    first wins, and the other's own `finish_session` sees `finished =
///    false`, re-reads the session, finds `state == Completed`, and
///    converges silently too (this is the *pre-existing* convergence branch
///    `finish_session` already had for "someone else already finished it" --
///    it did not need to change).
///
/// End state: **both** A's and B's original callers get `Ok(Completed(...))`
/// -- no stranding, no spurious error, exactly once assembly's worth of
/// content, correctly available and bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f2_stale_completer_converges_instead_of_stranding_after_owner_fencing_fix() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let real_multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let file_id = svc
        .create_file_bare(&ctx, new_file())
        .await
        .expect("create_file_bare");
    let plan = {
        let msvc_setup =
            make_multipart_service(Arc::clone(&real_multipart_store), backends.clone(), 120);
        msvc_setup
            .initiate_multipart_upload(
                &ctx,
                file_id,
                "application/octet-stream",
                10 * 1024 * 1024,
                Some(5 * 1024 * 1024),
                None,
                true, // auto_bind
            )
            .await
            .expect("initiate_multipart_upload (in-memory backend supports multipart_native)")
    };
    simulate_all_parts(&real_multipart_store, &backend, &plan, file_id).await;

    let b_checked_pending = Arc::new(Notify::new());
    let a_finalized = Arc::new(Notify::new());
    let b_first_get_version_seen = Arc::new(AtomicBool::new(false));

    let store_a: Arc<dyn MultipartStore> = Arc::new(GatedMultipartStore {
        inner: Arc::clone(&real_multipart_store),
        role: Role::A,
        b_checked_pending: Arc::clone(&b_checked_pending),
        a_finalized: Arc::clone(&a_finalized),
        b_first_get_version_seen: Arc::clone(&b_first_get_version_seen),
    });
    let store_b: Arc<dyn MultipartStore> = Arc::new(GatedMultipartStore {
        inner: Arc::clone(&real_multipart_store),
        role: Role::B,
        b_checked_pending: Arc::clone(&b_checked_pending),
        a_finalized: Arc::clone(&a_finalized),
        b_first_get_version_seen: Arc::clone(&b_first_get_version_seen),
    });

    // A's lease is the shortest the code allows (`.max(1)`), so a real,
    // just-over-a-second wait genuinely expires it -- no simulated time.
    let msvc_a = make_multipart_service(store_a, backends.clone(), 1);
    let msvc_b = make_multipart_service(store_b, backends.clone(), 120);

    let ctx_a = ctx.clone();
    let upload_id = plan.upload_id;
    let task_a = tokio::spawn(async move {
        msvc_a
            .complete_multipart_upload(&ctx_a, file_id, upload_id, None)
            .await
    });

    // Real wall-clock wait for A's 1-second lease to actually expire --
    // `acquire_multipart_complete_lease`'s CAS compares against real
    // `OffsetDateTime::now_utc()`, so this cannot be faked with
    // `tokio::time::pause`.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let ctx_b = ctx.clone();
    let task_b = tokio::spawn(async move {
        msvc_b
            .complete_multipart_upload(&ctx_b, file_id, upload_id, None)
            .await
    });

    let (result_a, result_b) = tokio::join!(task_a, task_b);
    let result_a = result_a.expect("task A join");
    let result_b = result_b.expect("task B join");

    eprintln!(
        "f2_stale_completer_converges: A={} B={}",
        describe_result(&result_a),
        describe_result(&result_b),
    );
    if let Err(e) = &result_a {
        eprintln!("f2: A's error = {e}");
    }
    if let Err(e) = &result_b {
        eprintln!("f2: B's error = {e}");
    }

    assert!(
        result_a.is_ok() && result_b.is_ok(),
        "FS-02/F2 fix: both completers must now converge to Ok(Completed(...)) instead of \
         stranding the session -- A={result_a:?} B={result_b:?}"
    );
    let completed_a = result_a.expect("checked above").unwrap_completed();
    let completed_b = result_b.expect("checked above").unwrap_completed();
    assert_eq!(
        completed_a.version_id, completed_b.version_id,
        "both completers must agree on the same finalized version"
    );
    assert_eq!(
        completed_a.bind_state,
        BindState::Bound,
        "the auto-bind CAS must have won for the winner's caller"
    );

    let version = store
        .get_version(file_id, plan.version_id)
        .await
        .expect("get_version")
        .expect("version row must still exist");
    assert_eq!(
        version.status,
        VersionStatus::Available,
        "the version must be correctly finalized exactly once"
    );
    let file = svc.get_file(&ctx, file_id).await.expect("get_file");
    assert_eq!(
        file.content_id,
        Some(plan.version_id),
        "the auto-bind CAS won for real -- the file's content_id must point at this version"
    );
    let session = real_multipart_store
        .get_multipart_upload(upload_id)
        .await
        .expect("get_multipart_upload")
        .expect("session row must still exist");
    assert_eq!(
        session.state,
        file_storage::domain::multipart::MultipartUploadState::Completed,
        "FS-02/F2 fix: the session must reach Completed, not be stranded at in_progress"
    );
    assert!(
        session.lease_until.is_none(),
        "a completed session has no live lease -- got {}",
        session
            .lease_until
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string)
    );

    // A third, honest retry (the idempotent-replay path) must now succeed
    // too, replaying the same persisted result -- confirming the fix
    // didn't just avoid the immediate race, it left the session in a
    // normally-replayable terminal state.
    let msvc_c = make_multipart_service(Arc::clone(&real_multipart_store), backends.clone(), 120);
    let result_c = msvc_c
        .complete_multipart_upload(&ctx, file_id, upload_id, None)
        .await;
    eprintln!("f2: third retry result = {}", describe_result(&result_c));
    let completed_c = result_c
        .expect(
            "FS-02/F2 fix: a third retry against an already-Completed session must replay, \
                 not error",
        )
        .unwrap_completed();
    assert_eq!(
        completed_c.version_id, completed_a.version_id,
        "the replayed result must match the original completion"
    );
}

/// Directly flip `multipart_uploads.expires_at` on a row -- no public API
/// backdates an already-created session's expiry; mirrors
/// `tests/cleanup_test.rs`'s own direct-entity backdating pattern.
async fn backdate_multipart_expires_at(
    db: &Arc<DBProvider<DbError>>,
    upload_id: Uuid,
    expires_at: OffsetDateTime,
) {
    use file_storage::infra::storage::entity::multipart_upload::{
        Column as UploadColumn, Entity as UploadEntity,
    };
    let conn = db.conn().expect("conn");
    UploadEntity::update_many()
        .col_expr(UploadColumn::ExpiresAt, Expr::value(expires_at))
        .filter(UploadColumn::UploadId.eq(upload_id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate expires_at");
}

// =========================================================================
// F9 / FS-04: multipart auto-bind clobbers a rebind that happened before
// complete started, when no If-Match is supplied. Deterministic/sequential
// by construction (see the module doc) -- no barrier needed.
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f9_autobind_no_if_match_clobbers_prior_rebind() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());
    let msvc = make_multipart_service(multipart_store.clone(), backends.clone(), 120);
    let dp = file_storage::domain::data_plane::DataPlaneService::new(
        Arc::clone(&svc) as Arc<dyn DataPlanePort>
    );

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    // Bind an initial version so content_id is non-NULL going in.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"first content"),
    )
    .await
    .expect("put_content");
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind first content");

    // Multipart-initiate an auto_bind session for a NEW version (mirrors a
    // client that started uploading before anyone else touched the file).
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            None,
            true,
        )
        .await
        .expect("initiate_multipart_upload with auto_bind");
    simulate_all_parts(&multipart_store, &backend, &plan, ticket.file_id).await;

    // Someone else legitimately rebinds the file to a THIRD version while
    // the multipart upload above is still in flight -- the ordinary,
    // often-long user-driven gap between initiate and complete.
    let rebind_ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .expect("create_file (rebind source)");
    // Reuse the same file_id's version slot by binding a version created
    // against `ticket.file_id` instead -- simplest: create a second version
    // directly under the SAME file via the ordinary single-part path.
    let _ = rebind_ticket; // (kept for clarity of narration; unused directly)
    let second_ticket_version = svc
        .presign_version(&ctx, ticket.file_id)
        .await
        .expect("presign_version for the legitimate rebind");
    dp.put_content(
        &ctx,
        ticket.file_id,
        second_ticket_version.version_id,
        "text/plain",
        Bytes::from_static(b"legitimately rebound content"),
    )
    .await
    .expect("put_content (legitimate rebind)");
    svc.bind(
        &ctx,
        ticket.file_id,
        second_ticket_version.version_id,
        Some("*"),
    )
    .await
    .expect("legitimate rebind, unconditional CAS wildcard");

    // Now complete the earlier multipart upload with NO If-Match.
    let completed = msvc
        .complete_multipart_upload(&ctx, ticket.file_id, plan.upload_id, None)
        .await
        .expect("complete_multipart_upload (no If-Match)")
        .unwrap_completed();

    assert_eq!(
        completed.bind_state,
        BindState::Bound,
        "known defect FS-04/F9: the auto-bind must silently win, clobbering the legitimate \
         rebind that happened before this complete call started, because no If-Match was \
         supplied"
    );
    let file_after = svc.get_file(&ctx, ticket.file_id).await.expect("get_file");
    assert_eq!(
        file_after.content_id,
        Some(plan.version_id),
        "known defect FS-04/F9: content_id now points at the multipart upload's version, \
         silently discarding the legitimate rebind -- no client-supplied CAS token ever \
         validated this overwrite was intended"
    );
}

/// Negative control: the SAME scenario, but the caller supplies the correct
/// `If-Match` (the current ETag, captured right after initiating) -- the
/// precondition check at the top of `complete_multipart_upload` (which runs
/// BEFORE the auto-bind CAS, and against the file's CURRENT etag, not a
/// snapshot) correctly rejects it as stale, proving the detector doesn't
/// call every auto-bind completion "broken" -- only the no-If-Match case is
/// the actual gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn negative_control_f9_autobind_with_correct_if_match_rejects_stale_rebind() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());
    let msvc = make_multipart_service(multipart_store.clone(), backends.clone(), 120);
    let dp = file_storage::domain::data_plane::DataPlaneService::new(
        Arc::clone(&svc) as Arc<dyn DataPlanePort>
    );

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"first content"),
    )
    .await
    .expect("put_content");
    let bound_file = svc
        .bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind first content");
    let etag_at_initiate_time = file_storage::domain::etag::etag_for(&bound_file);

    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            ticket.file_id,
            "application/octet-stream",
            5,
            None,
            None,
            true,
        )
        .await
        .expect("initiate_multipart_upload with auto_bind");
    simulate_all_parts(&multipart_store, &backend, &plan, ticket.file_id).await;

    let second_ticket_version = svc
        .presign_version(&ctx, ticket.file_id)
        .await
        .expect("presign_version for the legitimate rebind");
    dp.put_content(
        &ctx,
        ticket.file_id,
        second_ticket_version.version_id,
        "text/plain",
        Bytes::from_static(b"legitimately rebound content"),
    )
    .await
    .expect("put_content (legitimate rebind)");
    svc.bind(
        &ctx,
        ticket.file_id,
        second_ticket_version.version_id,
        Some("*"),
    )
    .await
    .expect("legitimate rebind");

    // This time, supply the ETag observed at initiate time as If-Match.
    let err = msvc
        .complete_multipart_upload(
            &ctx,
            ticket.file_id,
            plan.upload_id,
            etag_at_initiate_time.as_deref(),
        )
        .await
        .expect_err("a stale If-Match must be rejected, not silently overwritten");
    assert!(
        matches!(err, DomainError::PreconditionFailed { .. }),
        "negative control: supplying the correct (now-stale) If-Match must turn F9's silent \
         clobber into a clean PreconditionFailed, got: {err}"
    );

    let file_after = svc.get_file(&ctx, ticket.file_id).await.expect("get_file");
    assert_eq!(
        file_after.content_id,
        Some(second_ticket_version.version_id),
        "the legitimate rebind must survive when If-Match correctly protects it"
    );
}

// =========================================================================
// F10 / FS-05: expired multipart session blocks parent cleanup on THIS
// sweep pass, but no later pass ever revisits it either ("second path into
// F1"). Deterministic by construction -- no barrier needed.
// =========================================================================

async fn backdate_version_created_at(
    db: &Arc<DBProvider<DbError>>,
    version_id: Uuid,
    created_at: OffsetDateTime,
) {
    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };
    let conn = db.conn().expect("conn");
    FileVersionEntity::update_many()
        .col_expr(FileVersionColumn::CreatedAt, Expr::value(created_at))
        .filter(FileVersionColumn::VersionId.eq(version_id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("backdate version created_at");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f10_expired_session_orphans_parent_permanently_across_sweeps() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = make_file_service(store.clone(), backends.clone());
    let msvc = make_multipart_service(multipart_store.clone(), backends.clone(), 120);
    // grace = 1 hour so only the deliberately-backdated version below is
    // sweep-eligible (mirrors cleanup_test.rs's own convention).
    let engine = make_engine(store.clone(), backends, 3600);

    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);
    let file_id = svc
        .create_file_bare(&ctx, new_file())
        .await
        .expect("create_file_bare");
    let plan = msvc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            "application/octet-stream",
            1024,
            None,
            None,
            false,
        )
        .await
        .expect("initiate_multipart_upload");

    let now = OffsetDateTime::now_utc();
    backdate_version_created_at(&db, plan.version_id, now - time::Duration::hours(2)).await;
    backdate_multipart_expires_at(&db, plan.upload_id, now - time::Duration::seconds(10)).await;

    let result = engine.run_sweep().await;
    eprintln!("f10: first sweep result = {}", describe_sweep(&result));
    assert_eq!(
        result.abandoned_pending_deleted, 1,
        "the abandoned pending version must be reclaimed in this same pass"
    );
    assert_eq!(
        result.expired_multipart_aborted, 1,
        "the expired session must also be aborted in this same pass"
    );
    assert_eq!(
        result.abandoned_files_deleted, 0,
        "known defect FS-05/F10: the parent file must NOT be reclaimed in this pass -- \
         has_in_progress_for_file still saw the session as in_progress when step 1 (version \
         reclamation) ran, since step 2 (session abort) had not run yet"
    );

    let version_after = store
        .get_version(file_id, plan.version_id)
        .await
        .expect("get_version");
    assert!(
        version_after.is_none(),
        "the pending version row must be gone -- step 1 deletes it regardless of the \
         now-stale has_in_progress_for_file snapshot"
    );
    let file_after_pass_1 = svc.get_file(&ctx, file_id).await;
    assert!(
        file_after_pass_1.is_ok(),
        "known defect FS-05/F10: the file must survive this sweep pass as a version-less \
         orphan -- got: {file_after_pass_1:?}"
    );

    // The decisive assertion: a SECOND sweep pass never revisits this file
    // either, because the orphan-file check only ever runs as a side effect
    // of `delete_abandoned_pending_version`, and there is no version left to
    // trigger it a second time.
    let result_2 = engine.run_sweep().await;
    eprintln!("f10: second sweep result = {}", describe_sweep(&result_2));
    assert_eq!(
        result_2.abandoned_files_deleted, 0,
        "known defect FS-05/F10: a second sweep pass must not reclaim the orphan either -- \
         there is no pending version left anywhere to trigger the orphan-file check again"
    );
    let file_after_pass_2 = svc.get_file(&ctx, file_id).await;
    assert!(
        file_after_pass_2.is_ok(),
        "known defect FS-05/F10: the file remains a permanent version-less orphan across \
         repeated sweep passes -- got: {file_after_pass_2:?}"
    );
}

// =========================================================================
// Post-state invariant checker
// =========================================================================

/// Every `files.content_id` that is `Some` must point at a version that (a)
/// exists and (b) is `Available`. Unlike the version-less-orphan check
/// below, this invariant is expected to hold **unconditionally** --
/// `bind_content_cas` only ever points at a version finalized `available` in
/// the same transaction as the bind (see `finalize_version`'s `auto_bind`
/// doc) -- so this function returns the list of violations found (empty is
/// the healthy, expected result in every scenario in this file, including
/// the known-defect ones: F2/F9/F10 all leave `content_id` pointing at a
/// perfectly valid, available version; the defects they demonstrate are
/// elsewhere).
async fn find_content_id_violations(db: &Arc<DBProvider<DbError>>, file_id: Uuid) -> Vec<String> {
    use file_storage::infra::storage::entity::file::{Column as FileColumn, Entity as FileEntity};
    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let mut violations = Vec::new();
    let Some(file) = FileEntity::find()
        .filter(FileColumn::FileId.eq(file_id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query files")
    else {
        return violations;
    };
    if let Some(content_id) = file.content_id {
        let version = FileVersionEntity::find()
            .filter(FileVersionColumn::VersionId.eq(content_id))
            .secure()
            .scope_with(&scope)
            .one(&conn)
            .await
            .expect("query file_versions");
        match version {
            None => violations.push(format!(
                "file {file_id} content_id={content_id} points at a version row that does not exist"
            )),
            Some(v) if v.status != "available" => violations.push(format!(
                "file {file_id} content_id={content_id} points at a version whose status is \
                 {}, not available",
                v.status
            )),
            Some(_) => {}
        }
    }
    violations
}

/// Whether `file_id` is currently a version-less orphan (zero `file_versions`
/// rows and `content_id IS NULL`) -- the shape F1/F10 leave behind. Returns
/// `false` for a file that still has any version or bound content.
async fn is_versionless_orphan(db: &Arc<DBProvider<DbError>>, file_id: Uuid) -> bool {
    use file_storage::infra::storage::entity::file::{Column as FileColumn, Entity as FileEntity};
    use file_storage::infra::storage::entity::file_version::{
        Column as FileVersionColumn, Entity as FileVersionEntity,
    };
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let Some(file) = FileEntity::find()
        .filter(FileColumn::FileId.eq(file_id))
        .secure()
        .scope_with(&scope)
        .one(&conn)
        .await
        .expect("query files")
    else {
        return false;
    };
    if file.content_id.is_some() {
        return false;
    }
    let version_count = FileVersionEntity::find()
        .filter(FileVersionColumn::FileId.eq(file_id))
        .secure()
        .scope_with(&scope)
        .count(&conn)
        .await
        .expect("count file_versions");
    version_count == 0
}

/// The post-state invariant checker's own dedicated test: a healthy file
/// (create + upload + bind) must show zero content_id violations and must
/// NOT be a version-less orphan; a file put through F1's exact reproduction
/// above IS correctly flagged as a version-less orphan by the same checker
/// -- proving the checker actually distinguishes the two, not just always
/// reporting "clean" or always reporting "violation".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invariant_checker_distinguishes_healthy_file_from_known_orphan() {
    let (db, _pg_guard) = pg_db_or_skip!();
    let store = Store::new(Arc::clone(&db));
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let backends = BackendRegistry::new(vec![Arc::clone(&backend)], "mem").expect("registry");
    let svc = make_file_service(store.clone(), backends.clone());
    let dp = file_storage::domain::data_plane::DataPlaneService::new(
        Arc::clone(&svc) as Arc<dyn DataPlanePort>
    );
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    // Healthy file.
    let ticket = svc
        .create_file(&ctx, new_file(), None, false)
        .await
        .expect("create_file");
    dp.put_content(
        &ctx,
        ticket.file_id,
        ticket.version_id,
        "text/plain",
        Bytes::from_static(b"healthy"),
    )
    .await
    .expect("put_content");
    svc.bind(&ctx, ticket.file_id, ticket.version_id, None)
        .await
        .expect("bind");

    let violations = find_content_id_violations(&db, ticket.file_id).await;
    assert!(
        violations.is_empty(),
        "a healthy, correctly-bound file must have zero content_id invariant violations, got: \
         {violations:?}"
    );
    assert!(
        !is_versionless_orphan(&db, ticket.file_id).await,
        "a healthy, bound file must not be flagged as a version-less orphan"
    );

    // F1's exact reproduction: bare file, failed initiate, no cleanup path
    // ever triggered.
    let tmp = tempfile::tempdir().expect("tempdir");
    let local_backend: Arc<dyn StorageBackend> = Arc::new(LocalFsBackend::new("fs", tmp.keep()));
    let local_backends =
        BackendRegistry::new(vec![Arc::clone(&local_backend)], "fs").expect("registry");
    let svc_local = make_file_service(store.clone(), local_backends.clone());
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let msvc_local = make_multipart_service(multipart_store, local_backends, 120);
    let orphan_file_id = svc_local
        .create_file_bare(&ctx, new_file())
        .await
        .expect("create_file_bare");
    msvc_local
        .initiate_multipart_upload(
            &ctx,
            orphan_file_id,
            "application/octet-stream",
            20,
            Some(10),
            None,
            false,
        )
        .await
        .expect_err("capability reject");

    assert!(
        is_versionless_orphan(&db, orphan_file_id).await,
        "known defect FS-01/F1: the invariant checker must flag this file as a version-less \
         orphan -- if this starts failing, either F1 was fixed (update the report) or the \
         checker itself regressed"
    );
    // A version-less orphan trivially has no content_id, so it is not a
    // *content_id* violation by this checker's own definition (there is
    // nothing to point anywhere yet) -- the orphan-ness is the finding, not
    // a dangling pointer. Document that boundary directly rather than
    // asserting something the checker was never designed to catch.
    let orphan_violations = find_content_id_violations(&db, orphan_file_id).await;
    assert!(
        orphan_violations.is_empty(),
        "a version-less orphan has no content_id to be invalid -- the orphan-ness itself is \
         is_versionless_orphan's finding, not find_content_id_violations'; got unexpected \
         content_id violations: {orphan_violations:?}"
    );
}
