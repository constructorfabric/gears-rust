// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-schema:p1
//! `audit_records` gains `idx_audit_change_ref`, the partial index on the
//! `change_ref` column `m20261031_000001_activation_state` adds, through which
//! the history attaches an `activation` record to the change it answers
//! (DESIGN-activation §4.7).
//!
//! It is a migration of its own for the lock it is built under. The toolkit
//! runner applies each migration's `up()` and its history record in one
//! transaction, so an index built inside the activation migration would be
//! built while that migration's `ACCESS EXCLUSIVE` lock on `audit_records` is
//! held, blocking every reader and writer for the length of the build. Here
//! `CREATE INDEX` takes only the `SHARE` lock it takes alone: readers — the
//! history read among them — proceed, and writers to `audit_records`, which
//! every settings write is, wait for one sequential scan of the table. The
//! index it produces is empty, since the column was added by the preceding
//! migration and no row carries a `change_ref` yet, so the scan is the whole
//! cost. `CREATE INDEX CONCURRENTLY` would not block writers at all, but
//! `PostgreSQL` refuses it inside a transaction block, and the runner gives
//! a migration no other place to run.
//!
//! `IF NOT EXISTS` keeps the step a no-op on a database that built the index
//! under the earlier, single-migration spelling of the feature.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Both backends take a partial index in the same spelling.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_audit_change_ref
                     ON audit_records (change_ref)
                     WHERE change_ref IS NOT NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_audit_change_ref;")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20261031_000002_audit_change_ref_index_tests.rs"]
mod m20261031_000002_audit_change_ref_index_tests;
