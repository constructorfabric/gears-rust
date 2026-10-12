//! Add the locked FX snapshot reference and require a positive functional leg
//! when transaction money is zero. PostgreSQL adds the tenant-scoped snapshot FK
//! and final amount constraint here. SQLite's m002 fresh declaration already
//! carries the final sign/zero check; this migration adds its reference column.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_journal_line ADD COLUMN rate_snapshot_ref uuid",
    // Composite FK (tenant-scoped) to the immutable snapshot. Nullable ref →
    // MATCH SIMPLE skips the check when rate_snapshot_ref IS NULL (single-currency).
    // `NOT VALID` on both adds: existing rows are known-valid (legacy lines carry a
    // NULL `rate_snapshot_ref` — the FK is MATCH SIMPLE, skipped on NULL — and a
    // positive `amount`, satisfying the tightened CHECK), so Postgres skips
    // the validating full-table scan (no ACCESS EXCLUSIVE rewrite on a large
    // append-only journal). Both constraints still enforce every NEW write.
    "ALTER TABLE bss.ledger_journal_line
        ADD CONSTRAINT fk_journal_line_rate_snapshot
        FOREIGN KEY (tenant_id, rate_snapshot_ref)
        REFERENCES bss.ledger_fx_rate_snapshot (tenant_id, rate_id) NOT VALID",
    "ALTER TABLE bss.ledger_journal_line DROP CONSTRAINT chk_journal_line_amount",
    "ALTER TABLE bss.ledger_journal_line
        ADD CONSTRAINT chk_journal_line_amount
        CHECK (amount::numeric > 0 OR (amount::numeric = 0 AND functional_amount IS NOT NULL AND functional_amount::numeric > 0)) NOT VALID",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_journal_line DROP CONSTRAINT IF EXISTS chk_journal_line_amount",
    // Restore the P1 relaxed form (IS NOT NULL).
    "ALTER TABLE bss.ledger_journal_line
        ADD CONSTRAINT chk_journal_line_amount
        CHECK (amount::numeric > 0 OR (amount::numeric = 0 AND functional_amount IS NOT NULL))",
    "ALTER TABLE bss.ledger_journal_line DROP CONSTRAINT IF EXISTS fk_journal_line_rate_snapshot",
    "ALTER TABLE bss.ledger_journal_line DROP COLUMN IF EXISTS rate_snapshot_ref",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (column add only; the table-level
// CHECK tighten + FK are Postgres-only — SQLite cannot ALTER them in place).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] =
    &["ALTER TABLE ledger_journal_line ADD COLUMN rate_snapshot_ref text"];

const SQLITE_DOWN_STATEMENTS: &[&str] =
    &["ALTER TABLE ledger_journal_line DROP COLUMN rate_snapshot_ref"];

// ---------------------------------------------------------------------------
// Migration dispatch.
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_UP_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP_STATEMENTS,
            _ => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN_STATEMENTS,
            _ => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
