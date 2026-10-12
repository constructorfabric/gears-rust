//! Content-hash modes: `file_versions.hash_mode` (default `'whole-sha256'`, so
//! existing rows need no re-hash) and `part_count` (required only for
//! `'multipart-composite-sha256'`, via a cross-column `CHECK`), a unique index on
//! `file_versions (version_id)` so `version_hash_manifest` can reference it with a
//! single-column FK, and the `version_hash_manifest` table itself.

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
                // Columns are left in place on rollback: `SQLite` may lack `DROP COLUMN`
                // and they are backwards-compatible.
                Ok(())
            }
            _ => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
