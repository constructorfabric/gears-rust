//! Shared test helpers for FileStorage integration tests.
//!
//! Per `docs/modkit_unified_system/12_unit_testing.md`:
//! - SQLite `:memory:` for fast, isolated test DB (~1 ms per call)
//! - No `sleep`, no `timeout`, no shared state between tests
//! - Each test creates its own DB and its own data
//!
//! These are not E2E tests in the strict sense (no running server, no
//! real PostgreSQL). They are integration tests that drive the full
//! repository surface against a real database to verify the
//! conditional-UPDATE / race-detection semantics that pure-logic
//! unit tests cannot exercise.

use std::sync::Arc;

use file_storage::domain::repo::{FilesRepo, InsertPendingArgs};
use file_storage::infra::storage::migrations::Migrator;
use file_storage::infra::storage::sea_orm_repo::SeaOrmFilesRepository;
use file_storage_sdk::FileInfo;
use modkit_db::ConnectOpts;
use modkit_db::connect_db;
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::secure::Db;
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use uuid::Uuid;

/// SQLite `:memory:` DB with FileStorage migrations applied. ~1 ms.
///
/// Each call returns a fresh isolated DB.
pub async fn test_db() -> Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite::memory:");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run file_storage migrations");
    db
}

/// Repository over the test DB.
pub fn repo() -> Arc<SeaOrmFilesRepository> {
    Arc::new(SeaOrmFilesRepository::new())
}

/// Fixture: insert a fresh `pending_upload` row and return its `FileInfo`.
pub async fn insert_pending_row(
    db: &Db,
    repo: &SeaOrmFilesRepository,
    tenant_id: Uuid,
) -> FileInfo {
    let conn = db.conn().expect("conn");
    let now = OffsetDateTime::now_utc();
    let file_id = Uuid::new_v4();
    repo.insert_pending(
        &conn,
        InsertPendingArgs {
            file_id,
            tenant_id,
            backend_id: Uuid::new_v4(),
            file_path: format!("f/{}", file_id.simple()),
            owner_id: Uuid::new_v4(),
            name: "doc.pdf".into(),
            gts_file_type: "gts.cf.fstorage.file.type.v1~document".into(),
            mime_type: "application/pdf".into(),
            etag_pinned: String::new(),
            upload_expires_at: Some(now + time::Duration::hours(1)),
            custom_metadata_json: "{}".into(),
            now,
        },
    )
    .await
    .expect("insert_pending")
}

/// Drive the row to `uploaded` state with the given `(etag, version_id, size_bytes)`.
pub async fn finalize_upload(
    db: &Db,
    repo: &SeaOrmFilesRepository,
    tenant_id: Uuid,
    file_id: Uuid,
    etag: &str,
    version_id: Option<&str>,
    size_bytes: u64,
) -> FileInfo {
    let conn = db.conn().expect("conn");
    repo.begin_complete_upload(&conn, tenant_id, file_id, OffsetDateTime::now_utc())
        .await
        .expect("begin_complete_upload");
    match repo
        .finish_complete_upload(
            &conn,
            tenant_id,
            file_id,
            etag,
            version_id,
            size_bytes,
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("finish_complete_upload")
    {
        file_storage::domain::repo::ChangeStatusOutcome::Applied(info) => info,
        other => panic!("finish_complete_upload: unexpected outcome {other:?}"),
    }
}
