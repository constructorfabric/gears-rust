// Created: 2026-07-27 by Constructor Tech
// Not every test binary that declares `mod common;` uses every helper below
// (e.g. `query_recorder_test.rs` only needs `test_db_with_recorder`) --
// clippy's dead-code analysis runs per test *binary*, so a helper unused by
// one binary but used by another (`db_behavior_audit_test.rs`,
// `pg_concurrency_test.rs`) still needs this allow. Same shared-module shape
// as resource-group's own `tests/common/mod.rs`.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
//! Shared test helpers for the file-storage DB-behavior audit (Step 4 of the
//! DB-behavior audit program -- see
//! `docs/toolkit_unified_system/14_db_behavior_testing.md`, methodology
//! validated against `resource-group`'s own audit).
//!
//! Mirrors `resource_group`'s `tests/common/mod.rs::test_db_with_recorder()`
//! (branch `audit/rg-db-behavior`): the recorder's callback must be attached
//! *before* the connection is wrapped into a `DBProvider` (`SeaORM` captures
//! the callback by value at query time; `Db`/`DBProvider` never expose the
//! raw connection afterward, by the `toolkit-db` security model).
//!
//! Uses `sqlite::memory:` (unlike this crate's other integration-test files,
//! which use a file-backed temp DB so a *second*, independent raw connection
//! can inspect/tamper with rows for idempotency-replay tests) -- the
//! DB-behavior tests here only ever go through the one `DBProvider` the
//! service itself was built with, so there's no need for a second connection
//! to see the same data, and `:memory:` is faster and leaves nothing on disk.

use std::sync::Arc;

use bytes::Bytes;
use file_storage::domain::authz::TenantOnlyAuthorizer;
use file_storage::domain::multipart::MultipartPlan;
use file_storage::domain::multipart_service::MultipartService;
use file_storage::domain::ports::MultipartStore;
use file_storage::domain::service::{FileService, ServiceConfig};
use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
use file_storage::infra::signed_url::Issuer;
use file_storage::infra::storage::Store;
use file_storage::infra::storage::migrations::Migrator;
use file_storage_sdk::{NewFile, OwnerKind};
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

pub mod query_recorder;
use query_recorder::QueryRecorder;

/// GTS file type used throughout these tests -- an arbitrary, valid type
/// path; no policy/type-registry lookup depends on its exact value here.
pub const GTS: &str = gts_id!("cf.fstorage.file.type.v1~x.test.file.type.v1~");

/// Plain migrated in-memory `DBProvider`, no recorder attached.
pub async fn test_db() -> Arc<DBProvider<DbError>> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect to in-memory SQLite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");
    Arc::new(DBProvider::new(db))
}

/// Migrated in-memory `DBProvider` with a `QueryRecorder` attached to its
/// single pooled connection. The recorder captures migrations' own DDL
/// too, so it's cleared once migrations finish -- callers see a trace that
/// starts empty.
pub async fn test_db_with_recorder() -> (Arc<DBProvider<DbError>>, QueryRecorder) {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let (recorder, callback) = QueryRecorder::attach();
    let db = toolkit_db::connect_db_with_metric_callback("sqlite::memory:", opts, callback)
        .await
        .expect("connect to in-memory SQLite with recorder");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");
    recorder.clear();
    (Arc::new(DBProvider::new(db)), recorder)
}

/// A `SecurityContext` for a fresh random subject in the given tenant.
#[must_use]
pub fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

/// A `NewFile` request body with the shared test GTS type.
#[must_use]
pub fn new_file() -> NewFile {
    NewFile {
        owner_kind: OwnerKind::User,
        owner_id: Uuid::now_v7(),
        name: "audit.bin".to_owned(),
        gts_file_type: GTS.to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        custom_metadata: vec![],
    }
}

/// Everything a DB-behavior test typically needs: the two domain services
/// (sharing one `Store`), the default backend (for driving
/// `backend.upload_part`/`upload_bytes` directly, sidecar-style), and the
/// `MultipartStore` port (for `get_multipart_upload`/`upsert_multipart_part`,
/// also sidecar-style -- see [`simulate_sidecar_put_part`]).
pub struct Services {
    pub svc: Arc<FileService>,
    pub msvc: Arc<MultipartService>,
    pub backend: Arc<dyn StorageBackend>,
    pub multipart_store: Arc<dyn MultipartStore>,
}

/// Build `FileService` + `MultipartService` sharing one `Store` and one
/// `InMemoryBackend` (advertises `multipart_native: true`, so both
/// single-part and multipart flows work against it without needing a real
/// backend). Mirrors `tests/multipart_test.rs::build_service_with_config`,
/// trimmed to what the audit tests need.
///
/// Not `async` (nothing inside awaits -- `db` is already a connected
/// `DBProvider`, and `Store`/`FileService`/`MultipartService::new` are all
/// plain constructors); callers built as part of an async setup sequence
/// just call it directly, no `.await` needed.
pub fn make_services(db: &Arc<DBProvider<DbError>>) -> (Arc<FileService>, Arc<MultipartService>) {
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    let s = make_services_with_backends(db, vec![backend], "mem");
    (s.svc, s.msvc)
}

/// Full [`Services`] bundle (backend + multipart-store port included), for
/// tests that need to simulate the sidecar's part-upload dance.
pub fn make_services_full(db: &Arc<DBProvider<DbError>>) -> Services {
    let backend: Arc<dyn StorageBackend> = Arc::new(InMemoryBackend::new("mem"));
    make_services_with_backends(db, vec![backend], "mem")
}

/// Like [`make_services_full`], but the `BackendRegistry` is built from a
/// caller-supplied list of backends (e.g. a `LocalFsBackend` to exercise the
/// `multipart_native == false` capability-reject path, or a decorator
/// wrapping `InMemoryBackend` to inject a deterministic backend failure).
pub fn make_services_with_backends(
    db: &Arc<DBProvider<DbError>>,
    backends: Vec<Arc<dyn StorageBackend>>,
    default_id: &str,
) -> Services {
    let backend = backends
        .iter()
        .find(|b| b.id() == default_id)
        .cloned()
        .unwrap_or_else(|| backends.first().cloned().expect("at least one backend"));
    let backends_reg = BackendRegistry::new(backends, default_id).expect("registry");
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
    let store = Store::new(Arc::clone(db));
    let multipart_store: Arc<dyn MultipartStore> = Arc::new(store.clone());
    let svc = Arc::new(FileService::new(
        store,
        backends_reg.clone(),
        Arc::clone(&issuer),
        Arc::clone(&authorizer),
        cfg,
        None,
        None,
    ));
    let msvc = Arc::new(MultipartService::new(
        Arc::clone(&multipart_store),
        backends_reg,
        Arc::clone(&authorizer),
        None,
        issuer,
        "http://sidecar.test".to_owned(),
        3600,
    ));
    Services {
        svc,
        msvc,
        backend,
        multipart_store,
    }
}

/// Simulate the sidecar writing one part of a multipart upload: writes
/// `data` through the backend's native multipart path
/// (`backend.upload_part`), then persists the part row via
/// `MultipartStore::upsert_multipart_part` -- exactly the two steps a real
/// sidecar performs (`backend.upload_part` then an SDK callback), mirroring
/// `tests/multipart_test.rs::simulate_sidecar_put_part`.
pub async fn simulate_sidecar_put_part(
    multipart_store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &MultipartPlan,
    file_id: Uuid,
    part_number: u32,
    data: Bytes,
) {
    let part = plan
        .parts
        .iter()
        .find(|p| p.part_number == part_number)
        .unwrap_or_else(|| panic!("part {part_number} not in plan"));
    let session = multipart_store
        .get_multipart_upload(plan.upload_id)
        .await
        .expect("get_multipart_upload")
        .expect("session must exist");
    let backend_path = format!("/{file_id}/{}", plan.version_id);

    let (backend_etag, part_hash) = backend
        .upload_part(
            &backend_path,
            &session.backend_upload_handle,
            part_number,
            part.offset,
            data,
        )
        .await
        .expect("backend upload_part");

    let size = i64::try_from(part.size).expect("part size fits in i64");
    let now = time::OffsetDateTime::now_utc();
    let part_number_i32 = i32::try_from(part_number).expect("part_number fits in i32");
    multipart_store
        .upsert_multipart_part(
            plan.upload_id,
            part_number_i32,
            &backend_etag,
            part_hash,
            size,
            now,
        )
        .await
        .expect("upsert_multipart_part");
}

/// Drive every part in `plan` through [`simulate_sidecar_put_part`] with
/// `part.size`-length, all-zero filler bytes -- for tests that only care
/// that a complete assembles, not about specific byte content.
pub async fn simulate_all_parts(
    multipart_store: &Arc<dyn MultipartStore>,
    backend: &Arc<dyn StorageBackend>,
    plan: &MultipartPlan,
    file_id: Uuid,
) {
    for part in plan.parts.clone() {
        let size = usize::try_from(part.size).expect("part size fits in usize");
        let data = Bytes::from(vec![0u8; size]);
        simulate_sidecar_put_part(
            multipart_store,
            backend,
            plan,
            file_id,
            part.part_number,
            data,
        )
        .await;
    }
}
