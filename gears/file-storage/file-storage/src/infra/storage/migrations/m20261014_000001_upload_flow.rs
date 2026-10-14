//! Upload flow, one migration for both dialects:
//!
//! 1. `multipart_uploads`: `auto_bind`, completion-lease columns (`lease_owner`, `lease_until`,
//!    `complete_result`; `state` CHECK widened to admit `completing`), and
//!    `backend_id`/`backend_path` so expired-session cleanup does not need a reclaimed
//!    `file_versions` row. `SQLite` cannot alter a CHECK, so the table is rebuilt; child
//!    `multipart_upload_parts` rows are evacuated first (`DROP TABLE` would cascade them;
//!    `PRAGMA foreign_keys` cannot be toggled mid-transaction) and the two indexes recreated.
//! 2. Index `idempotency_keys_file_idx` (FK-cascade index for replay lookup and cascade delete).
//! 3. `file_versions.bound_on_finalize`: lets an idempotent finalize retry report the bind
//!    outcome without re-reading `files.content_id`.
//!
//! `down()` reverses all three; `completing` rows are folded into `aborted` first (the narrow
//! CHECK cannot hold them); `backend_id`/`backend_path` are dropped without backfill.

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

-- The backend/path a session's upload actually targets (see the module doc
-- for why cleanup needs this persisted rather than always reconstructed).
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS backend_id text NULL;
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS backend_path text NULL;

-- Part 2: FK-cascade index (see the module doc).
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);

-- Part 3: persisted finalize-time bind decision (see the module doc).
ALTER TABLE file_versions
    ADD COLUMN IF NOT EXISTS bound_on_finalize boolean NOT NULL DEFAULT false;
";

// SQLite rebuild (create-copy-drop-rename); see the module doc.
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
    backend_id             TEXT NULL,
    backend_path           TEXT NULL,
    created_at             TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    expires_at             TEXT  NOT NULL
);
INSERT INTO multipart_uploads_new (
    upload_id, file_id, version_id, backend_upload_handle, state,
    declared_mime, mime_validated, declared_size, part_size,
    created_at, expires_at
)
SELECT mu.upload_id, mu.file_id, mu.version_id, mu.backend_upload_handle, mu.state,
       mu.declared_mime, mu.mime_validated, mu.declared_size, mu.part_size,
       mu.created_at, mu.expires_at
FROM multipart_uploads mu;
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

-- Part 2: FK-cascade index (see the module doc).
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);

-- Part 3: persisted finalize-time bind decision (see the module doc).
-- SQLite's `ADD COLUMN` has no `IF NOT EXISTS` clause, unlike Postgres above.
ALTER TABLE file_versions ADD COLUMN bound_on_finalize BOOLEAN NOT NULL DEFAULT FALSE;
";

// Reverse of POSTGRES_UP; `completing` rows are folded into `aborted` before the CHECK narrows.
const POSTGRES_DOWN: &str = r"
DROP INDEX IF EXISTS idempotency_keys_file_idx;

ALTER TABLE file_versions DROP COLUMN IF EXISTS bound_on_finalize;

UPDATE multipart_uploads SET state = 'aborted' WHERE state = 'completing';
ALTER TABLE multipart_uploads DROP CONSTRAINT IF EXISTS multipart_uploads_state_check;
ALTER TABLE multipart_uploads
    ADD CONSTRAINT multipart_uploads_state_check
    CHECK (state IN ('in_progress', 'completed', 'aborted'));
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS auto_bind;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS lease_until;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS lease_owner;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS complete_result;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS backend_id;
ALTER TABLE multipart_uploads DROP COLUMN IF EXISTS backend_path;
";

// SQLite down: same reverts, rebuild mirrored; the `completing` -> `aborted` fold happens in the copy's SELECT.
const SQLITE_DOWN: &str = r"
DROP INDEX IF EXISTS idempotency_keys_file_idx;

-- SQLite's `DROP COLUMN` has no `IF EXISTS` clause, unlike Postgres's equivalent statement.
ALTER TABLE file_versions DROP COLUMN bound_on_finalize;

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
-- backend_id/backend_path are dropped along with the other four columns this
-- migration added -- up()'s copy is not mirrored in reverse (no rebuild reads
-- them back out of file_versions); the round trip is schema-equivalence
-- only, not data preservation of these two columns.
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
            // MySQL and any future `#[non_exhaustive]` backend are refused explicitly.
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
