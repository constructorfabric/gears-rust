//! Upload-flow redesign: the completion-lease state machine, an
//! initiate-time `auto_bind` flag, session-persisted `backend_id`/
//! `backend_path`, and the index hardening that leans on those same new
//! columns — in one migration, for both dialects.
//!
//! # 1. `multipart_uploads` columns and the widened `state` CHECK
//!
//! `POST /files` can open a multipart session directly (merged create+plan)
//! with `bind: "auto"` (the default), in which case `complete_multipart_upload`
//! performs the content bind itself — in the same transaction as the version
//! finalize, under the same CAS as a manual `POST /files/{id}/bind` — instead
//! of requiring a separate client `bind` request. The chosen mode is fixed at
//! session creation, so it is persisted on the session row (`auto_bind`);
//! `complete` reads it back rather than trusting any per-request input.
//! Existing rows (and sessions opened via the still-supported standalone
//! `POST /files/{id}/multipart`) default to `FALSE` — staged behaviour
//! (complete never binds; the client binds manually).
//!
//! Also adds the completion-lease state-machine columns: `complete`
//! transitions `in_progress → completing(lease_owner, lease_until) →
//! completed(complete_result)` via single conditional UPDATEs — no DB
//! transaction is held across the backend assembly I/O — and the persisted
//! `complete_result` JSON makes re-complete idempotent. The `state` CHECK is
//! widened to admit `completing`.
//!
//! Also adds `backend_id`/`backend_path` (both nullable text): the backend
//! and object path a session's upload actually targets, fixed at initiate
//! time. Before these columns existed, the expired-multipart-session cleanup
//! (`CleanupEngine::cleanup_expired_session_version_with_file`) had no way to
//! recover that pair once the `file_versions` row it normally reads them from
//! was already reclaimed (a real race: step 1 of the same sweep can delete
//! the pending version before step 2 ever looks at the session) — it fell
//! back to the *default* backend and a freshly recomputed deterministic path,
//! which silently aborts on the wrong backend for any session whose upload
//! was never on the default backend, leaking the real backend-side
//! multipart handle. Persisting the pair directly on the session row removes
//! the need to reconstruct it from a row that may no longer exist. Existing
//! rows are backfilled from their matching `file_versions` row (`(file_id,
//! version_id)`, the same pair the session was created with) — a row whose
//! version has already been reclaimed (or that was never given a version, an
//! edge case only the abandoned-standalone-initiate path can leave behind) is
//! left `NULL`, the legacy case cleanup's fallback still covers.
//!
//! On `SQLite`, which cannot alter or drop a `CHECK` constraint, this part
//! rebuilds `multipart_uploads` (create-copy-drop-rename) rather than
//! altering it in place. `multipart_upload_parts.upload_id` carries
//! `REFERENCES multipart_uploads (upload_id) ON DELETE CASCADE`, and sqlx
//! enables `PRAGMA foreign_keys` by default, so the naive rebuild's `DROP
//! TABLE multipart_uploads` would cascade-delete every `multipart_upload_parts`
//! row before the parent table is even gone (`PRAGMA foreign_keys` cannot be
//! toggled off mid-transaction). The rebuild therefore evacuates the child
//! rows to an unconstrained holding table first and reinserts them once the
//! parent is back in place under the same `upload_id` values, and re-creates
//! the two indexes that lived on the old table (`multipart_uploads_file_idx`,
//! `multipart_uploads_expired_idx`) that dropping it would otherwise lose.
//!
//! # 2. Index hardening
//!
//! Covers hot predicates that otherwise force a full table scan, plus one
//! tie-breaker fix on an existing covering index. Applied after part 1 above
//! because two of these indexes cover columns/values part 1 introduces
//! (`multipart_uploads.lease_until` and the `completing` state):
//!
//! - `idempotency_keys_file_idx` on `idempotency_keys (file_id)`: covers the
//!   `ON DELETE CASCADE` back to `files` — without it, every `DELETE FROM
//!   files` seq-scans the whole table for cascade victims while already
//!   holding the row lock(s) on `files`.
//! - `multipart_uploads_sweep_idx` on `multipart_uploads (state, expires_at,
//!   lease_until)`: the orphan-reconciliation sweep filters `expires_at < now
//!   AND (state = 'in_progress' OR (state = 'completing' AND lease_until <
//!   now))`; the existing `multipart_uploads_expired_idx` only serves the
//!   `in_progress` branch (partial on Postgres, plain on `SQLite`), so this new,
//!   deliberately non-partial index leads with `state` to cover both branches
//!   of the OR.
//! - `files_versionless_sweep_idx` on `files (created_at, file_id) WHERE
//!   content_id IS NULL`: covers the versionless-orphan-file cleanup sweep's
//!   `content_id IS NULL AND created_at < cutoff` scan, ordered by
//!   `(created_at, file_id)` — `files`'s existing indexes are all
//!   owner/tenant-oriented and do not serve this predicate.
//! - `file_versions_file_created_idx` on `file_versions (file_id, created_at,
//!   version_id)`: covers `VersionRepo::list_by_file`'s `file_id = ?` filter
//!   plus its `created_at DESC` sort; the composite PK `(file_id, version_id)`
//!   serves the filter but not the sort, and versions are never pruned in
//!   P1/P2, so a long-lived file's version count grows unbounded.
//! - `files_owner_listing_v2_idx` on `files (tenant_id, owner_kind, owner_id,
//!   created_at DESC, file_id DESC)`, replacing `files_owner_listing_idx
//!   (tenant_id, owner_kind, owner_id, created_at DESC)` from the already-
//!   released `m20260624_000001_p1_initial` (left untouched, dropped here
//!   instead): `FileRepo::list` sorts `ORDER BY created_at DESC, file_id
//!   DESC`, and the old index's missing `file_id` tie-break left that half of
//!   the sort to an extra in-memory pass over every row sharing a
//!   `created_at` instant.
//!
//! # `down()`
//!
//! Rolls both parts back, in reverse order: first the five new indexes
//! (dropped) with `files_owner_listing_idx` recreated, then the
//! `multipart_uploads` columns/CHECK (and, on `SQLite`, the table itself)
//! restored to their pre-migration shape. A `completing` row cannot satisfy
//! the narrowed CHECK (that lease state did not exist before this migration),
//! so any live `completing` row is folded into `aborted` first, the same
//! outcome an expired lease would eventually produce on its own —
//! `backend_id`/`backend_path` are dropped without an inverse backfill (the
//! round trip is schema-equivalence only for those two columns, not data
//! preservation).
//!
//! This migration merges two migrations from this branch into one, since
//! neither had shipped in a release.

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
-- Backfill existing rows (created by a pre-this-migration server) from their
-- matching file_versions row. Only touches rows this migration itself just
-- added the (NULL) columns to -- an already-populated row (a re-run, or a
-- row this migration's own INSERT path already filled) is left untouched.
UPDATE multipart_uploads
SET backend_id = fv.backend_id,
    backend_path = fv.backend_path
FROM file_versions fv
WHERE fv.file_id = multipart_uploads.file_id
  AND fv.version_id = multipart_uploads.version_id
  AND multipart_uploads.backend_id IS NULL;

-- Index hardening (part 2 of this migration -- see the module doc). Placed
-- after the columns above because multipart_uploads_sweep_idx covers
-- lease_until and the widened state domain.
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_sweep_idx
    ON multipart_uploads (state, expires_at, lease_until);
CREATE INDEX IF NOT EXISTS files_versionless_sweep_idx
    ON files (created_at, file_id) WHERE content_id IS NULL;
CREATE INDEX IF NOT EXISTS file_versions_file_created_idx
    ON file_versions (file_id, created_at, version_id);
CREATE INDEX IF NOT EXISTS files_owner_listing_v2_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC, file_id DESC);
DROP INDEX IF EXISTS files_owner_listing_idx;
";

// SQLite cannot alter or drop a CHECK constraint -- rebuild the table with the
// widened state CHECK (rebuild-and-rename pattern, no data loss; sessions are
// short-lived rows so the copy is trivially small).
//
// `multipart_upload_parts.upload_id` is declared `REFERENCES multipart_uploads
// (upload_id) ON DELETE CASCADE` (`m20260701_000001_p2_initial`), and sqlx
// enables `PRAGMA foreign_keys` by default -- so the naive rebuild (create the
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
// `multipart_uploads_expired_idx`) -- dropping the table drops them too, and
// the index-hardening indexes appended after the rebuild do not cover them.
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
-- Backfill backend_id/backend_path from the matching file_versions row (see
-- the module doc) via a LEFT JOIN in the same copy -- a row with no matching
-- version (already reclaimed, or never given one) simply gets NULLs from the
-- unmatched join side, same as the Postgres backfill's fallback.
INSERT INTO multipart_uploads_new (
    upload_id, file_id, version_id, backend_upload_handle, state,
    declared_mime, mime_validated, declared_size, part_size,
    backend_id, backend_path,
    created_at, expires_at
)
SELECT mu.upload_id, mu.file_id, mu.version_id, mu.backend_upload_handle, mu.state,
       mu.declared_mime, mu.mime_validated, mu.declared_size, mu.part_size,
       fv.backend_id, fv.backend_path,
       mu.created_at, mu.expires_at
FROM multipart_uploads mu
LEFT JOIN file_versions fv
    ON fv.file_id = mu.file_id AND fv.version_id = mu.version_id;
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

-- Index hardening (part 2 of this migration -- see the module doc). Placed
-- after the rebuild above because multipart_uploads_sweep_idx covers
-- lease_until and the widened state domain.
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_sweep_idx
    ON multipart_uploads (state, expires_at, lease_until);
CREATE INDEX IF NOT EXISTS files_versionless_sweep_idx
    ON files (created_at, file_id) WHERE content_id IS NULL;
CREATE INDEX IF NOT EXISTS file_versions_file_created_idx
    ON file_versions (file_id, created_at, version_id);
CREATE INDEX IF NOT EXISTS files_owner_listing_v2_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC, file_id DESC);
DROP INDEX IF EXISTS files_owner_listing_idx;
";

// PostgreSQL down: reverse of POSTGRES_UP, in reverse order -- drop the five
// new indexes and restore files_owner_listing_idx first, then drop the four
// new multipart_uploads columns and restore the original narrow state CHECK.
// A `completing` row cannot satisfy the narrow CHECK (that lease state did
// not exist before this migration) -- a real rollback can only have live
// `completing` rows if a completer is lease-holding mid-flight, so treat
// them the same way an expired lease eventually would and fold them into
// `aborted` before the CHECK is narrowed, rather than leaving the rollback
// to fail outright on an active deployment.
const POSTGRES_DOWN: &str = r"
DROP INDEX IF EXISTS files_owner_listing_v2_idx;
CREATE INDEX IF NOT EXISTS files_owner_listing_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC);
DROP INDEX IF EXISTS file_versions_file_created_idx;
DROP INDEX IF EXISTS files_versionless_sweep_idx;
DROP INDEX IF EXISTS multipart_uploads_sweep_idx;
DROP INDEX IF EXISTS idempotency_keys_file_idx;

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

// SQLite down: same index revert as POSTGRES_DOWN, then the same child-safe
// rebuild-and-rename pattern as SQLITE_UP, mirrored to restore the table
// shape without the six new columns and with the narrow state CHECK. The
// `completing` -> `aborted` fold-in (see POSTGRES_DOWN's comment for why)
// happens inline in the copy's SELECT here, since SQLite's CHECK is enforced
// at INSERT into the new table.
const SQLITE_DOWN: &str = r"
DROP INDEX IF EXISTS files_owner_listing_v2_idx;
CREATE INDEX IF NOT EXISTS files_owner_listing_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC);
DROP INDEX IF EXISTS file_versions_file_created_idx;
DROP INDEX IF EXISTS files_versionless_sweep_idx;
DROP INDEX IF EXISTS multipart_uploads_sweep_idx;
DROP INDEX IF EXISTS idempotency_keys_file_idx;

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
