//! Bind idempotency keys to the authenticated subject.
//!
//! Adds `subject_id` (`ctx.subject_id()` at insert time), not part of the primary
//! key: the domain layer verifies it on replay and answers `Forbidden` on mismatch,
//! so one caller cannot reuse another's `(owner_id, key)` tuple.
//! Pre-existing rows are backfilled with the nil UUID, which never matches a real
//! subject, so their replays are rejected.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE idempotency_keys
    ADD COLUMN IF NOT EXISTS subject_id uuid NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
";

const SQLITE_UP: &str = r"
ALTER TABLE idempotency_keys ADD COLUMN subject_id TEXT NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
";

const DOWN: &str = r"
-- Down is intentionally a no-op: SQLite does not support DROP COLUMN in older
-- versions, and the column is backwards-compatible (defaults to the nil
-- UUID). A production rollback would need a follow-up migration; for test
-- environments the whole DB is dropped anyway.
SELECT 1;
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
