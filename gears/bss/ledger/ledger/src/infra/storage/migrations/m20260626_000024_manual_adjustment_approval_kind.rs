//! Forward-fix: add `MANUAL_ADJUSTMENT` to the dual-control `ledger_approval.kind`
//! CHECK (Slice 3 Group 5 / Phase 3 governance, VHP-1856) on clusters where
//! `m20260624_000012` was ALREADY applied with its prior kind set.
//!
//! `m20260624_000012` was amended in place to add `MANUAL_ADJUSTMENT` to the
//! `chk_ledger_approval_kind` CHECK (so an over-threshold governed manual
//! adjustment can be parked PENDING). In-place edits to an already-run migration do
//! NOT re-apply (migrations run once, by name), so a cluster that ran the prior
//! `000012` keeps the OLD CHECK — and the new manual-adjustment gate's
//! `INSERT … kind = 'MANUAL_ADJUSTMENT'` then trips that CHECK (a 500). This
//! migration brings such a cluster up to date; it is **idempotent** (DROP
//! CONSTRAINT IF EXISTS + re-create), so on a FRESH cluster — where `000012`'s
//! amended form already created the final shape — it is a harmless no-op
//! re-statement.
//!
//! **Postgres-only.** `SQLite` is non-production and always migrates fresh from the
//! amended `000012` (which already carries `MANUAL_ADJUSTMENT` in the CHECK), so the
//! `SQLite` branch here is intentionally empty. (Re-stating a table-level CHECK on
//! `SQLite` would require a full table rebuild; it is unnecessary for the fresh-only
//! `SQLite` path, mirroring `m20260626_000023`.)

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    // Add the `MANUAL_ADJUSTMENT` kind (Group 5 / Phase 3). DROP IF EXISTS first so a
    // fresh cluster (where amended 000012 already created the final CHECK) re-creates
    // cleanly.
    "ALTER TABLE bss.ledger_approval DROP CONSTRAINT IF EXISTS chk_ledger_approval_kind",
    "ALTER TABLE bss.ledger_approval ADD CONSTRAINT chk_ledger_approval_kind CHECK (kind IN
        ('REVERSE','MATERIAL_BACKDATING','CREDIT_GRANT','CHARGEBACK_LOSS','PAYER_CLOSURE','PERIOD_REOPEN','RECOGNITION_SCHEDULE_CHANGE','REFUND','MANUAL_ADJUSTMENT'))",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_approval DROP CONSTRAINT IF EXISTS chk_ledger_approval_kind",
    "ALTER TABLE bss.ledger_approval ADD CONSTRAINT chk_ledger_approval_kind CHECK (kind IN
        ('REVERSE','MATERIAL_BACKDATING','CREDIT_GRANT','CHARGEBACK_LOSS','PAYER_CLOSURE','PERIOD_REOPEN','RECOGNITION_SCHEDULE_CHANGE','REFUND'))",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        // Postgres-only: SQLite migrates fresh from the amended 000012.
        if backend != sea_orm::DatabaseBackend::Postgres {
            return Ok(());
        }
        for sql in PG_UP_STATEMENTS {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        if backend != sea_orm::DatabaseBackend::Postgres {
            return Ok(());
        }
        for sql in PG_DOWN_STATEMENTS {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
