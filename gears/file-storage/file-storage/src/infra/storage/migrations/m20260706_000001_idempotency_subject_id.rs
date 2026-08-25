//! Bind idempotency keys to the authenticated subject (P2 remediation 0.10).
//!
//! `idempotency_keys` previously carried no notion of *who* created a given
//! key — the composite key `(tenant_id, owner_kind, owner_id, idempotency_key)`
//! is derived entirely from the request body, so one caller could guess/reuse
//! another caller's `(owner_id, key)` tuple and, together with the ordering
//! bug fixed alongside this migration, receive a live upload URL for a file
//! they never created and are not authorized to write.
//!
//! This migration adds a `subject_id` column recording `ctx.subject_id()` at
//! insert time. It is **not** added to the primary key — the domain layer
//! (`FileService::create_file`) fetches the record via the existing composite
//! key and then verifies `record.subject_id == ctx.subject_id()`, treating a
//! mismatch as `Forbidden` rather than silently falling through to a fresh
//! create (which would otherwise race the still-live row on insert).
//!
//! Deliberately **not** coupled to 2.1's `request_hash` column: 2.1 is not
//! landed on this branch, and coupling an unrelated, already-necessary Tier 0
//! fix to a not-yet-designed follow-up would only add churn risk here.
//!
//! Existing (pre-migration) rows have no real subject on file; they are
//! backfilled with the nil UUID, which can never equal a real
//! `ctx.subject_id()`, so any in-flight replay of a pre-migration key is
//! correctly treated as a subject mismatch (`Forbidden`) rather than being
//! silently trusted.
//!
//! @cpt-cf-file-storage-fr-upload-idempotency

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
