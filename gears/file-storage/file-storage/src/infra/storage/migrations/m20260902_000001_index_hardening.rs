//! Index hardening: cover two hot predicates that currently force a full
//! table scan.
//!
//! 1. `idempotency_keys_file_idx` on `idempotency_keys (file_id)`.
//!    `idempotency_keys.file_id` carries `REFERENCES files (file_id) ON
//!    DELETE CASCADE` (`m20260701_000001_p2_initial`), but the table's only
//!    index is `idempotency_keys_expired_idx (expires_at)` — the primary key
//!    is the four-column `(tenant_id, owner_kind, owner_id,
//!    idempotency_key)`, which does not help a lookup keyed by `file_id`.
//!    Every `DELETE FROM files` therefore makes SQLite/Postgres seq-scan the
//!    whole `idempotency_keys` table to find the cascade victims, while
//!    already holding the row lock(s) on `files` inside the deleting
//!    transaction.
//!
//! 2. `multipart_uploads_sweep_idx` on `multipart_uploads (state,
//!    expires_at, lease_until)`. The orphan-reconciliation sweep
//!    (`MultipartRepo::list_expired`) filters on `expires_at < now AND
//!    (state = 'in_progress' OR (state = 'completing' AND lease_until <
//!    now))`. The existing `multipart_uploads_expired_idx` is a *partial*
//!    index restricted to `state = 'in_progress'` on Postgres (plain
//!    `(expires_at, state)` on `SQLite`, where `p2_initial` simply did not
//!    make it partial (`SQLite` has supported partial indexes since 3.8.0) —
//!    either way it does not serve the `completing AND lease_until < now`
//!    half of the OR, so that branch falls back to a full scan. The
//!    new index is deliberately non-partial and leads with `state` so both
//!    branches of the OR can use it.
//!
//! `down()` drops both indexes on both dialects.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_sweep_idx
    ON multipart_uploads (state, expires_at, lease_until);
";

const SQLITE_UP: &str = r"
CREATE INDEX IF NOT EXISTS idempotency_keys_file_idx
    ON idempotency_keys (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_sweep_idx
    ON multipart_uploads (state, expires_at, lease_until);
";

const DOWN: &str = r"
DROP INDEX IF EXISTS multipart_uploads_sweep_idx;
DROP INDEX IF EXISTS idempotency_keys_file_idx;
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
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                conn.execute_unprepared(DOWN).await?;
                Ok(())
            }
            // See `up()`'s matching arm.
            _ => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
