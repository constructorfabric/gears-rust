//! Keyset indexes for the cursor-paginated listings (files, versions, retention rules); the
//! id tie-breaker is each index's last column:
//! - `file_versions (file_id, created_at, version_id)` for the versions listing;
//! - `files_owner_listing_v2_idx` replaces `files_owner_listing_idx` with `file_id DESC` appended;
//! - `retention_rules (tenant_id, created_at DESC, rule_id DESC)`.
//!
//! `down` restores the previous `files_owner_listing_idx` and drops the rest.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE INDEX IF NOT EXISTS file_versions_file_created_idx
    ON file_versions (file_id, created_at, version_id);
CREATE INDEX IF NOT EXISTS files_owner_listing_v2_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC, file_id DESC);
DROP INDEX IF EXISTS files_owner_listing_idx;
CREATE INDEX IF NOT EXISTS retention_rules_tenant_listing_idx
    ON retention_rules (tenant_id, created_at DESC, rule_id DESC);
";

const SQLITE_UP: &str = r"
CREATE INDEX IF NOT EXISTS file_versions_file_created_idx
    ON file_versions (file_id, created_at, version_id);
CREATE INDEX IF NOT EXISTS files_owner_listing_v2_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC, file_id DESC);
DROP INDEX IF EXISTS files_owner_listing_idx;
CREATE INDEX IF NOT EXISTS retention_rules_tenant_listing_idx
    ON retention_rules (tenant_id, created_at DESC, rule_id DESC);
";

const DOWN: &str = r"
DROP INDEX IF EXISTS retention_rules_tenant_listing_idx;
DROP INDEX IF EXISTS files_owner_listing_v2_idx;
CREATE INDEX IF NOT EXISTS files_owner_listing_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC);
DROP INDEX IF EXISTS file_versions_file_created_idx;
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
                Ok(())
            }
            _ => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
