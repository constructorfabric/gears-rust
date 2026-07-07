//! Safety-net invariant (Slice 5 remediation): on every functional-bearing
//! balance cache, the two FX columns must be set together —
//! `functional_balance_minor IS NULL` **iff** `functional_currency IS NULL`.
//!
//! Rationale: a cross-currency grain carries BOTH columns; a single-currency
//! grain carries NEITHER (functional ≡ transaction by identity, decision 8). The
//! one way to violate that is the drift bug this remediation closes — a
//! transaction-only post (reversal / settlement-return / claw-back that dropped
//! the functional column) relieving a cross-currency grain: the projector's
//! conflict path nets `balance_minor + delta` while overwriting
//! `functional_currency` to NULL and leaving `functional_balance_minor` frozen,
//! producing a `(balance Some, currency NULL)` row. This CHECK rejects exactly
//! that row, turning a SILENT transaction-vs-functional drift into a loud,
//! transactional abort — the backstop behind the per-path carry-forward fixes.
//!
//! Added `NOT VALID`: every legacy / single-currency row already satisfies the
//! predicate (both columns NULL by construction — they predate or opt out of the
//! functional translation), so the validating full-table scan is skipped (no
//! `ACCESS EXCLUSIVE` table rewrite on a large cache); the constraint still
//! enforces every NEW write, which is all the drift guard needs.
//!
//! Postgres-only: `SQLite` (the non-production test backend) cannot
//! `ADD CONSTRAINT` to an existing table without a full rebuild (mirrors the
//! m027 / m029 split); the real backend the FX postgres tests run against is
//! Postgres.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_account_balance
        ADD CONSTRAINT chk_account_balance_func_consistency
        CHECK ((functional_balance_minor IS NULL) = (functional_currency IS NULL)) NOT VALID",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD CONSTRAINT chk_ar_invoice_balance_func_consistency
        CHECK ((functional_balance_minor IS NULL) = (functional_currency IS NULL)) NOT VALID",
    "ALTER TABLE bss.ledger_ar_payer_balance
        ADD CONSTRAINT chk_ar_payer_balance_func_consistency
        CHECK ((functional_balance_minor IS NULL) = (functional_currency IS NULL)) NOT VALID",
    "ALTER TABLE bss.ledger_unallocated_balance
        ADD CONSTRAINT chk_unallocated_balance_func_consistency
        CHECK ((functional_balance_minor IS NULL) = (functional_currency IS NULL)) NOT VALID",
    "ALTER TABLE bss.ledger_reusable_credit_subbalance
        ADD CONSTRAINT chk_reusable_credit_func_consistency
        CHECK ((functional_balance_minor IS NULL) = (functional_currency IS NULL)) NOT VALID",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_reusable_credit_subbalance
        DROP CONSTRAINT IF EXISTS chk_reusable_credit_func_consistency",
    "ALTER TABLE bss.ledger_unallocated_balance
        DROP CONSTRAINT IF EXISTS chk_unallocated_balance_func_consistency",
    "ALTER TABLE bss.ledger_ar_payer_balance
        DROP CONSTRAINT IF EXISTS chk_ar_payer_balance_func_consistency",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        DROP CONSTRAINT IF EXISTS chk_ar_invoice_balance_func_consistency",
    "ALTER TABLE bss.ledger_account_balance
        DROP CONSTRAINT IF EXISTS chk_account_balance_func_consistency",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema: no table-level CHECK add in place.
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[];

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
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
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
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
