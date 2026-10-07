// Created: 2026-09-25 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-table:p1
//! The audit retention policy row exists from the start.
//!
//! The row the trigger reads was written by the first retention pass, which
//! updated it or, finding none, inserted it. Two replicas' first passes could
//! both find none and both insert, and the loser's whole pass failed on the
//! primary key. Seeding the row here, at the platform minimum, leaves the pass
//! one statement — an update of the row that is always there — so there is
//! nothing to race for. The gear overwrites the value with its configured
//! retention before every pass. Rolling this migration back leaves the row:
//! it is configuration, and the writer that stays live needs it there.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

// The minimum (`365` below) is written out rather than taken from
// `crate::audit::MIN_RETENTION_DAYS`, as in the migrations before; a test
// pins the two together.

#[derive(DeriveMigrationName)]
pub struct Migration;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let now = if manager.get_database_backend() == DatabaseBackend::Postgres {
            "now()"
        } else {
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
        };
        // Idempotent either way: a row already there is left as it is.
        db.execute_unprepared(&format!(
            "INSERT INTO settings_audit_policy (id, retention_days, updated_at)
             SELECT 1, 365, {now}
             WHERE NOT EXISTS (SELECT 1 FROM settings_audit_policy WHERE id = 1);"
        ))
        .await?;
        Ok(())
    }

    /// Nothing: the row is configuration the gear has written, not schema.
    /// Deleting it would stop a live instance's retention pass — the writer
    /// only updates the row — and discard the operator's configured horizon.
    /// The table itself goes with the migration that made it.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20260925_000003_audit_retention_policy_seed_tests.rs"]
mod m20260925_000003_audit_retention_policy_seed_tests;
