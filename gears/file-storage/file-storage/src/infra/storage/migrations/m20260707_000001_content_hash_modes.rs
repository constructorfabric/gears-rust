//! ADR-0006 content-hash modes: `hash_mode`/`part_count` on `file_versions`,
//! plus the new `version_hash_manifest` table.
//!
//! Adds:
//! - `file_versions.hash_mode text NOT NULL DEFAULT 'whole-sha256' CHECK (...)`
//!   — which of the two shipped hash modes produced this version's
//!   `hash_value`. Every pre-existing row backfills to `'whole-sha256'` via
//!   the column default (correct: every extant row is a P1 single-part
//!   SHA-256 upload, requiring no re-hash).
//! - `file_versions.part_count integer` — `NOT NULL` only for
//!   `hash_mode = 'multipart-composite-sha256'`, enforced by a cross-column
//!   presence `CHECK`. Pre-existing rows backfill to `NULL`.
//! - A unique index on `file_versions (version_id)` alone, so
//!   `version_hash_manifest` can carry a single-column FK into it (the
//!   table's actual PK is the composite `(file_id, version_id)` — see
//!   `entity/file_version.rs`'s doc comment: `version_id` is already
//!   globally unique in practice, this index makes that a DB-enforced fact).
//! - `version_hash_manifest (version_id PK/FK, manifest, created_at)` — one
//!   row per `multipart-composite-sha256` version (§4/§5 of
//!   `content-hash-modes.md`). No row for pre-existing/whole-sha256 versions.
//!
//! `hash_algorithm`'s existing `CHECK (hash_algorithm = 'SHA-256')` is left
//! **untouched** — both modes use SHA-256 as their only underlying
//! primitive, so there is nothing to widen.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE file_versions
    ADD COLUMN IF NOT EXISTS hash_mode text NOT NULL DEFAULT 'whole-sha256'
        CHECK (hash_mode IN ('whole-sha256', 'multipart-composite-sha256')),
    ADD COLUMN IF NOT EXISTS part_count integer;

ALTER TABLE file_versions
    ADD CONSTRAINT file_versions_part_count_presence_check
        CHECK ((hash_mode = 'multipart-composite-sha256') = (part_count IS NOT NULL));

CREATE UNIQUE INDEX IF NOT EXISTS file_versions_version_id_unique_idx
    ON file_versions (version_id);

CREATE TABLE IF NOT EXISTS version_hash_manifest (
    version_id  uuid         NOT NULL PRIMARY KEY
                             REFERENCES file_versions (version_id) ON DELETE CASCADE,
    manifest    text         NOT NULL,
    created_at  timestamptz  NOT NULL  DEFAULT now()
);
";

const SQLITE_UP: &str = r"
-- SQLite does not support multi-column ADD COLUMN in one statement.
ALTER TABLE file_versions ADD COLUMN hash_mode TEXT NOT NULL DEFAULT 'whole-sha256'
    CHECK (hash_mode IN ('whole-sha256', 'multipart-composite-sha256'));
ALTER TABLE file_versions ADD COLUMN part_count INTEGER
    CHECK ((hash_mode = 'multipart-composite-sha256') = (part_count IS NOT NULL));

CREATE UNIQUE INDEX IF NOT EXISTS file_versions_version_id_unique_idx
    ON file_versions (version_id);

CREATE TABLE IF NOT EXISTS version_hash_manifest (
    version_id  TEXT  NOT NULL PRIMARY KEY
                      REFERENCES file_versions (version_id) ON DELETE CASCADE,
    manifest    TEXT  NOT NULL,
    created_at  TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP
);
";

const DOWN: &str = r"
DROP TABLE IF EXISTS version_hash_manifest;
DROP INDEX IF EXISTS file_versions_version_id_unique_idx;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
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
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                conn.execute_unprepared(DOWN).await?;
                // `hash_mode`/`part_count` columns are intentionally left in
                // place on rollback (mirrors `m20260701_000002`'s policy):
                // SQLite cannot `DROP COLUMN` in older versions, the columns
                // are backwards-compatible (a NOT NULL default + a nullable
                // column), and a Postgres production rollback of the columns
                // themselves would need a dedicated follow-up migration.
                Ok(())
            }
            _ => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
