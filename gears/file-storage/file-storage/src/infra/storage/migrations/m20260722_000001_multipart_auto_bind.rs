//! Add `auto_bind` to `multipart_uploads` (upload-flow redesign).
//!
//! `POST /files` can now open a multipart session directly (merged
//! create+plan) with `bind: "auto"` (the default), in which case
//! `complete_multipart_upload` performs the content bind itself — in the same
//! transaction as the version finalize, under the same CAS as a manual
//! `POST /files/{id}/bind` — instead of requiring a separate client `bind`
//! request. The chosen mode is fixed at session creation, so it is persisted
//! on the session row; `complete` reads it back rather than trusting any
//! per-request input.
//!
//! Existing rows (and sessions opened via the still-supported standalone
//! `POST /files/{id}/multipart`) default to `FALSE` — the pre-redesign
//! staged behaviour (complete never binds; the client binds manually).
//!
//! Also adds the completion-lease state-machine columns (same redesign):
//! `complete` transitions `in_progress → completing(lease_owner,
//! lease_until) → completed(complete_result)` via single conditional
//! UPDATEs — no DB transaction is held across the backend assembly I/O — and
//! the persisted `complete_result` JSON makes re-complete idempotent.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS auto_bind BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS lease_until timestamptz NULL;
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS lease_owner text NULL;
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS complete_result text NULL;
-- Widen the state CHECK to admit the new 'completing' lease state. The
-- original inline CHECK gets the auto-generated name below on Postgres.
ALTER TABLE multipart_uploads DROP CONSTRAINT IF EXISTS multipart_uploads_state_check;
ALTER TABLE multipart_uploads
    ADD CONSTRAINT multipart_uploads_state_check
    CHECK (state IN ('in_progress', 'completing', 'completed', 'aborted'));
";

// SQLite cannot alter or drop a CHECK constraint — rebuild the table with the
// widened state CHECK (12-step ALTER pattern, no data loss; sessions are
// short-lived rows so the copy is trivially small).
const SQLITE_UP: &str = r"
CREATE TABLE multipart_uploads_new (
    upload_id              TEXT  PRIMARY KEY NOT NULL,
    file_id                TEXT  NOT NULL
                                 REFERENCES files (file_id) ON DELETE CASCADE,
    version_id             TEXT  NOT NULL,
    backend_upload_handle  TEXT  NOT NULL,
    state                  TEXT  NOT NULL  DEFAULT 'in_progress'
                                 CHECK (state IN ('in_progress', 'completing', 'completed', 'aborted')),
    declared_mime          TEXT  NOT NULL,
    mime_validated         INTEGER NOT NULL DEFAULT 0,
    declared_size          INTEGER NOT NULL DEFAULT 0,
    part_size              INTEGER NOT NULL DEFAULT 0,
    auto_bind              BOOLEAN NOT NULL DEFAULT FALSE,
    lease_until            TIMESTAMP NULL,
    lease_owner            TEXT NULL,
    complete_result        TEXT NULL,
    created_at             TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    expires_at             TEXT  NOT NULL
);
INSERT INTO multipart_uploads_new (
    upload_id, file_id, version_id, backend_upload_handle, state,
    declared_mime, mime_validated, declared_size, part_size,
    created_at, expires_at
)
SELECT upload_id, file_id, version_id, backend_upload_handle, state,
       declared_mime, mime_validated, declared_size, part_size,
       created_at, expires_at
FROM multipart_uploads;
DROP TABLE multipart_uploads;
ALTER TABLE multipart_uploads_new RENAME TO multipart_uploads;
";

const DOWN: &str = r"
-- Down is intentionally a no-op: SQLite does not support DROP COLUMN in older
-- versions, and the column is backwards-compatible (defaults to FALSE, the
-- pre-redesign behaviour). A production rollback would need a follow-up
-- migration; for test environments the whole DB is dropped anyway.
SELECT 1;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(
                    "file-storage migrations support Postgres and SQLite only".to_owned(),
                ));
            }
        };
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                conn.execute_unprepared(DOWN).await?;
                Ok(())
            }
            sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
