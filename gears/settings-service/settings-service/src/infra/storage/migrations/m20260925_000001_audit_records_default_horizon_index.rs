// Created: 2026-09-25 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-table:p1
//! `audit_records` gains the index the retention pass prunes by.
//!
//! Nearly every record carries no `retain_until` and leaves once `occurred_at`
//! is past the configured retention. The table had an index for the explicit
//! hold only (`idx_audit_retention`) and one led by the setting key
//! (`idx_audit_scoped`), so the daily pass read the whole table to find a
//! day's worth of rows — at the declared bound, tens of millions read for
//! about a hundred thousand deleted. A partial index on `occurred_at` for the
//! rows without a hold serves exactly that predicate, in the order the pass
//! takes them.

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
                "CREATE INDEX IF NOT EXISTS idx_audit_default_horizon
                     ON audit_records (occurred_at)
                     WHERE retain_until IS NULL;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_audit_default_horizon;")
            .await?;
        Ok(())
    }
}
