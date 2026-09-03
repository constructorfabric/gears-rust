//! Add `auto_bind` to `multipart_uploads`.
//!
//! `POST /files` can open a multipart session directly (merged create+plan)
//! with `bind: "auto"` (the default), in which case `complete_multipart_upload`
//! performs the content bind itself — in the same transaction as the version
//! finalize, under the same CAS as a manual `POST /files/{id}/bind` — instead
//! of requiring a separate client `bind` request. The chosen mode is fixed at
//! session creation, so it is persisted on the session row; `complete` reads
//! it back rather than trusting any per-request input.
//!
//! Existing rows (and sessions opened via the still-supported standalone
//! `POST /files/{id}/multipart`) default to `FALSE` — staged behaviour
//! (complete never binds; the client binds manually).
//!
//! Also adds the completion-lease state-machine columns: `complete`
//! transitions `in_progress → completing(lease_owner, lease_until) →
//! completed(complete_result)` via single conditional UPDATEs — no DB
//! transaction is held across the backend assembly I/O — and the persisted
//! `complete_result` JSON makes re-complete idempotent.
//!
//! `down()` performs a real rollback on both dialects: it drops the four new
//! columns and restores the original narrow `state` CHECK (folding any live
//! `completing` row into `aborted` first, since that lease state cannot
//! satisfy the narrow CHECK). On `SQLite` the rollback rebuilds
//! `multipart_uploads` the same child-safe way `up()` does, so
//! `multipart_upload_parts` rows and the `multipart_uploads_file_idx` /
//! `multipart_uploads_expired_idx` indexes survive the round trip.

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
// widened state CHECK (rebuild-and-rename pattern, no data loss; sessions are
// short-lived rows so the copy is trivially small).
//
// `multipart_upload_parts.upload_id` is declared `REFERENCES multipart_uploads
// (upload_id) ON DELETE CASCADE` (`m20260701_000001_p2_initial`), and sqlx
// enables `PRAGMA foreign_keys` by default — so the naive rebuild (create the
// new table, copy the parent rows, `DROP TABLE multipart_uploads`, rename)
// makes that `DROP TABLE` perform an implicit cascading delete of *every*
// `multipart_upload_parts` row, parent-row-by-parent-row, before the table is
// even gone. `PRAGMA foreign_keys` cannot be toggled off mid-transaction
// (SQLite treats it as a no-op there), so the only way to keep the children
// is to evacuate them to an unconstrained holding table first and reinsert
// them once the parent has been recreated with the same `upload_id` values:
// the reinsert's FK check then finds the parent row already back in place.
//
// The rebuild also has to recreate the two indexes that lived on the old
// `multipart_uploads` table (`multipart_uploads_file_idx`,
// `multipart_uploads_expired_idx`) — dropping the table drops them too, and
// nothing else in this migration re-adds them.
const SQLITE_UP: &str = r"
CREATE TABLE multipart_upload_parts_backup AS SELECT * FROM multipart_upload_parts;

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

INSERT INTO multipart_upload_parts (
    upload_id, part_number, backend_etag, part_hash, size, uploaded_at
)
SELECT upload_id, part_number, backend_etag, part_hash, size, uploaded_at
FROM multipart_upload_parts_backup;
DROP TABLE multipart_upload_parts_backup;

CREATE INDEX IF NOT EXISTS multipart_uploads_file_idx
    ON multipart_uploads (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_expired_idx
    ON multipart_uploads (expires_at, state);
";

// PostgreSQL down: drop the four new columns and restore the original narrow
// state CHECK. A `completing` row cannot satisfy the narrow CHECK (that
// lease state did not exist before this migration) — a real rollback can
// only have live `completing` rows if a completer is lease-holding
// mid-flight, so treat them the same way an expired lease eventually would
// and fold them into `aborted` before the CHECK is narrowed, rather than
// leaving the rollback to fail outright on an active deployment.
const POSTGRES_DOWN: &str = r"
UPDATE multipart_uploads SET state = 'aborted' WHERE state = 'completing';
ALTER TABLE multipart_uploads DROP CONSTRAINT IF EXISTS multipart_uploads_state_check;
ALTER TABLE multipart_uploads
    ADD CONSTRAINT multipart_uploads_state_check
    CHECK (state IN ('in_progress', 'completed', 'aborted'));
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS auto_bind;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS lease_until;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS lease_owner;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS complete_result;
";

// SQLite down: same child-safe rebuild-and-rename pattern as `SQLITE_UP`,
// mirrored to restore the table shape without the four new columns and with
// the narrow state CHECK. The `completing` -> `aborted` fold-in (see
// `POSTGRES_DOWN`'s comment for why) happens inline in the copy's `SELECT`
// here, since SQLite's CHECK is enforced at INSERT into the new table.
const SQLITE_DOWN: &str = r"
CREATE TABLE multipart_upload_parts_backup AS SELECT * FROM multipart_upload_parts;

CREATE TABLE multipart_uploads_old (
    upload_id              TEXT  PRIMARY KEY NOT NULL,
    file_id                TEXT  NOT NULL
                                 REFERENCES files (file_id) ON DELETE CASCADE,
    version_id             TEXT  NOT NULL,
    backend_upload_handle  TEXT  NOT NULL,
    state                  TEXT  NOT NULL  DEFAULT 'in_progress'
                                 CHECK (state IN ('in_progress', 'completed', 'aborted')),
    declared_mime          TEXT  NOT NULL,
    mime_validated         INTEGER NOT NULL DEFAULT 0,
    declared_size          INTEGER NOT NULL DEFAULT 0,
    part_size              INTEGER NOT NULL DEFAULT 0,
    created_at             TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    expires_at             TEXT  NOT NULL
);
INSERT INTO multipart_uploads_old (
    upload_id, file_id, version_id, backend_upload_handle, state,
    declared_mime, mime_validated, declared_size, part_size,
    created_at, expires_at
)
SELECT upload_id, file_id, version_id, backend_upload_handle,
       CASE WHEN state = 'completing' THEN 'aborted' ELSE state END,
       declared_mime, mime_validated, declared_size, part_size,
       created_at, expires_at
FROM multipart_uploads;
DROP TABLE multipart_uploads;
ALTER TABLE multipart_uploads_old RENAME TO multipart_uploads;

INSERT INTO multipart_upload_parts (
    upload_id, part_number, backend_etag, part_hash, size, uploaded_at
)
SELECT upload_id, part_number, backend_etag, part_hash, size, uploaded_at
FROM multipart_upload_parts_backup;
DROP TABLE multipart_upload_parts_backup;

CREATE INDEX IF NOT EXISTS multipart_uploads_file_idx
    ON multipart_uploads (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_expired_idx
    ON multipart_uploads (expires_at, state);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            // MySQL and any backend a future `sea_orm` adds to the
            // `#[non_exhaustive]` `DatabaseBackend` enum are refused
            // explicitly here, rather than left to a panic on an
            // uncovered pattern.
            _ => {
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
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_DOWN,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN,
            // See `up()`'s matching arm.
            _ => {
                return Err(DbErr::Custom(
                    "file-storage migrations support Postgres and SQLite only".to_owned(),
                ));
            }
        };
        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}
