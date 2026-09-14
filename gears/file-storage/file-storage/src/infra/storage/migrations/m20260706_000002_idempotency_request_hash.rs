//! Bind idempotency keys to a canonicalized hash of the request body (P2
//! remediation 2.1).
//!
//! `idempotency_keys` previously stored no fingerprint of the request that
//! created it — `FileService::create_file`'s replay path returned the stored
//! ticket unconditionally once a live record was found for the composite key
//! `(tenant_id, owner_kind, owner_id, idempotency_key)`. A caller could reuse
//! the same `idempotency_key` with a materially different body (`name`,
//! `gts_file_type`, `mime_type`, `custom_metadata`) and silently receive the
//! *original* ticket instead of an error, masking a client bug or a stale
//! retry.
//!
//! This migration adds a `request_hash` column recording a SHA-256 over a
//! canonicalized, length-prefixed encoding of the identity-relevant request
//! fields at insert time (see `domain::idempotency::compute_request_hash`).
//! The domain layer (`FileService::create_file`) recomputes this hash from
//! the *current* request on replay and rejects a mismatch with `409
//! Conflict`, alongside (not instead of) the `subject_id` check added by P2
//! remediation 0.10.
//!
//! Deliberately sequenced **after** `m20260706_000001_idempotency_subject_id`
//! — that migration's own header notes 2.1 was not yet landed on this branch
//! when it was written, so this is the next migration in the numbered
//! sequence rather than colliding with it.
//!
//! Existing (pre-migration) rows have no real hash on file; they default to
//! an empty blob, which can never equal a freshly computed 32-byte SHA-256
//! digest, so any in-flight replay of a pre-migration key is correctly
//! treated as a body mismatch (`Conflict`) rather than being silently
//! trusted. In practice this table's rows expire within
//! `idempotency_ttl_secs` (default 86400s), so there are no long-lived rows
//! to backfill.
//!
//! @cpt-cf-file-storage-fr-upload-idempotency

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
