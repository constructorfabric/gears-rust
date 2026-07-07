//! Forward-fix: add `CREDIT_NOTE` + `DEBIT_NOTE` to the dual-control
//! `ledger_approval.kind` CHECK (Slice 3 Phase-1 dual-control follow-up, VHP-1856)
//! on clusters where `m20260624_000012` was ALREADY applied with its prior kind set.
//!
//! `m20260624_000012` was amended in place to add `CREDIT_NOTE` + `DEBIT_NOTE` to the
//! `chk_ledger_approval_kind` CHECK (so an over-threshold credit/debit note can be
//! parked PENDING — the §5 D1–D2 governance gate the Phase-1 as-built deferred). In-place
//! edits to an already-run migration do NOT re-apply (migrations run once, by name), so a
//! cluster that ran the prior `000012` keeps the OLD CHECK — and the new note gate's
//! `INSERT … kind = 'CREDIT_NOTE'` then trips that CHECK (a 500). This migration brings
//! such a cluster up to date; it is **idempotent** (DROP CONSTRAINT IF EXISTS + re-create),
//! so on a FRESH cluster — where `000012`'s amended form already created the final shape —
//! it is a harmless no-op re-statement.
//!
//! **Postgres-only.** `SQLite` is non-production and always migrates fresh from the
//! amended `000012` (which already carries both note kinds in the CHECK), so the `SQLite`
//! branch here is intentionally empty (mirroring `m20260626_000023` / `_000024`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    // Add the `CREDIT_NOTE` + `DEBIT_NOTE` kinds. DROP IF EXISTS first so a fresh
    // cluster (where amended 000012 already created the final CHECK) re-creates cleanly.
    "ALTER TABLE bss.ledger_approval DROP CONSTRAINT IF EXISTS chk_ledger_approval_kind",
    "ALTER TABLE bss.ledger_approval ADD CONSTRAINT chk_ledger_approval_kind CHECK (kind IN
        ('REVERSE','MATERIAL_BACKDATING','CREDIT_GRANT','CHARGEBACK_LOSS','PAYER_CLOSURE','PERIOD_REOPEN','RECOGNITION_SCHEDULE_CHANGE','REFUND','MANUAL_ADJUSTMENT','CREDIT_NOTE','DEBIT_NOTE'))",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_approval DROP CONSTRAINT IF EXISTS chk_ledger_approval_kind",
    "ALTER TABLE bss.ledger_approval ADD CONSTRAINT chk_ledger_approval_kind CHECK (kind IN
        ('REVERSE','MATERIAL_BACKDATING','CREDIT_GRANT','CHARGEBACK_LOSS','PAYER_CLOSURE','PERIOD_REOPEN','RECOGNITION_SCHEDULE_CHANGE','REFUND','MANUAL_ADJUSTMENT'))",
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
