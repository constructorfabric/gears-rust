//! Add the `ar_status (ACTIVE|DISPUTED)` foundation seam used by the
//! chargeback `opened` AR-reclass (design §4.5, Group A / D3). Two additive,
//! backward-compatible columns:
//!
//! * `journal_line.ar_status varchar(16)` — a per-line snapshot, set on AR
//!   lines that participate in a dispute reclass (`ACTIVE`/`DISPUTED`), `NULL`
//!   on every other line. Nullable + a NULL-tolerant CHECK, so all existing
//!   (untagged) lines stay valid. `journal_line` is append-only (the P1
//!   reject-mutation trigger fires on UPDATE/DELETE of *rows*); `ALTER TABLE …
//!   ADD COLUMN` is DDL, not a row mutation, so the trigger does not block it.
//! * `ledger_ar_invoice_balance.disputed_minor bigint NOT NULL DEFAULT 0` — the
//!   disputed sub-portion of the invoice's open AR. `balance_minor` stays the
//!   FULL open AR (a reclass nets ZERO on it → AR-class-neutral); the projector
//!   routes the disputed delta here instead. Guarded by `>= 0` and
//!   `<= balance_minor` CHECKs (its own no-negative / no-over-dispute backstop;
//!   matches the existing `chk_ar_invoice_balance_*` naming).
//!
//! The per-invoice `ar_status` flag is DERIVED, not stored: an invoice is
//! `DISPUTED` exactly when `disputed_minor == balance_minor` (with
//! `balance_minor > 0`), computable from the two columns wherever it is needed.
//! Storing it would add a third column the atomic `on_conflict` net-update path
//! cannot maintain without a CASE expression — deriving is the simpler choice
//! given the projector code.
//!
//! `SQLite` mirrors the same shape with the systematic transforms (here only
//! the `bss.` schema prefix is dropped; the column types + CHECKs are identical).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_journal_line
        ADD COLUMN ar_status varchar(16)
        CONSTRAINT chk_journal_line_ar_status
        CHECK (ar_status IS NULL OR ar_status IN ('ACTIVE','DISPUTED'))",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD COLUMN disputed_minor bigint NOT NULL DEFAULT 0",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD CONSTRAINT chk_ar_invoice_balance_disputed_no_negative
        CHECK (disputed_minor >= 0)",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD CONSTRAINT chk_ar_invoice_balance_disputed_le_balance
        CHECK (disputed_minor <= balance_minor)",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_ar_invoice_balance
        DROP CONSTRAINT IF EXISTS chk_ar_invoice_balance_disputed_le_balance",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        DROP CONSTRAINT IF EXISTS chk_ar_invoice_balance_disputed_no_negative",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS disputed_minor",
    "ALTER TABLE bss.ledger_journal_line DROP COLUMN IF EXISTS ar_status",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; column types + CHECKs
// preserved). SQLite folds the column-level CHECK into `ADD COLUMN`; the two
// table-level CHECKs on `disputed_minor` are likewise expressed inline on the
// added column (SQLite's `ALTER TABLE ADD COLUMN` cannot add a standalone
// table constraint, but a column CHECK referencing a sibling column is valid).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_journal_line
        ADD COLUMN ar_status varchar(16)
        CONSTRAINT chk_journal_line_ar_status
        CHECK (ar_status IS NULL OR ar_status IN ('ACTIVE','DISPUTED'))",
    "ALTER TABLE ledger_ar_invoice_balance
        ADD COLUMN disputed_minor bigint NOT NULL DEFAULT 0
        CONSTRAINT chk_ar_invoice_balance_disputed_no_negative
        CHECK (disputed_minor >= 0 AND disputed_minor <= balance_minor)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_ar_invoice_balance DROP COLUMN disputed_minor",
    "ALTER TABLE ledger_journal_line DROP COLUMN ar_status",
];

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
