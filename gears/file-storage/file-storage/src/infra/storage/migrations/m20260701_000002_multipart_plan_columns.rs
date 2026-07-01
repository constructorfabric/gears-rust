//! Server-authoritative multipart-coordinator schema delta.
//!
//! Adds the plan columns to `multipart_uploads` that the server-authoritative
//! multipart model requires (FEATURE `multipart-coordinator`, §6):
//!
//! - `declared_size bigint NOT NULL` — the gated total; allows `complete` and
//!   resume to verify actual-vs-declared without re-summing parts.
//! - `part_size bigint NOT NULL` — the server-chosen plan unit; together with
//!   `declared_size` this reconstitutes the full plan for resume without a
//!   per-part plan table.
//!
//! `version_id` was already present in the P2-initial migration
//! (`m20260701_000001_p2_initial`).
//!
//! @cpt-cf-file-storage-fr-multipart-upload

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE multipart_uploads
    ADD COLUMN IF NOT EXISTS declared_size bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS part_size     bigint NOT NULL DEFAULT 0;
";

const SQLITE_UP: &str = r"
-- SQLite does not support multi-column ADD COLUMN in one statement.
ALTER TABLE multipart_uploads ADD COLUMN declared_size INTEGER NOT NULL DEFAULT 0;
ALTER TABLE multipart_uploads ADD COLUMN part_size     INTEGER NOT NULL DEFAULT 0;
";

const DOWN: &str = r"
-- Down is intentionally a no-op: SQLite does not support DROP COLUMN in older
-- versions and the columns are backwards-compatible (default 0).  For Postgres
-- a production rollback would need a follow-up migration; for test environments
-- the whole DB is dropped anyway.
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
