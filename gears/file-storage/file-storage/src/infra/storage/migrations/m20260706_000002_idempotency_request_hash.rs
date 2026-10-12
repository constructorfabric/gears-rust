//! Bind idempotency keys to a hash of the creating request.
//!
//! Adds `request_hash` (SHA-256 of the identity-relevant request fields, see
//! `domain::idempotency::compute_request_hash`). A replay with a different body is
//! rejected with `409 Conflict` instead of returning the original ticket.
//! Pre-existing rows default to an empty blob, which never equals a 32-byte digest,
//! so their replays are rejected (rows expire within `idempotency_ttl_secs` anyway).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
ALTER TABLE idempotency_keys
    ADD COLUMN IF NOT EXISTS request_hash bytea NOT NULL DEFAULT '\x';
";

const SQLITE_UP: &str = r"
ALTER TABLE idempotency_keys ADD COLUMN request_hash BLOB NOT NULL DEFAULT x'';
";

const DOWN: &str = r"
-- Down is intentionally a no-op: SQLite does not support DROP COLUMN in older
-- versions, and the column is backwards-compatible (defaults to an empty
-- blob). A production rollback would need a follow-up migration; for test
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
