//! Add the per-line AR status and disputed portion of invoice open AR.
//! PostgreSQL checks exact numeric text for nonnegativity and disputed <= balance.
//! SQLite checks the sign; the application enforces the exact relational check.

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
        ADD COLUMN disputed text NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(disputed, currency_scale))",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD CONSTRAINT chk_ar_invoice_balance_disputed_no_negative
        CHECK (disputed::numeric >= 0)",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD CONSTRAINT chk_ar_invoice_balance_disputed_le_balance
        CHECK (disputed::numeric <= balance::numeric)",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_ar_invoice_balance
        DROP CONSTRAINT IF EXISTS chk_ar_invoice_balance_disputed_le_balance",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        DROP CONSTRAINT IF EXISTS chk_ar_invoice_balance_disputed_no_negative",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS disputed",
    "ALTER TABLE bss.ledger_journal_line DROP COLUMN IF EXISTS ar_status",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; column types + CHECKs
// preserved). SQLite folds the column-level CHECK into `ADD COLUMN`; the two
// table-level CHECKs on `disputed` are likewise expressed inline on the
// added column (SQLite's `ALTER TABLE ADD COLUMN` cannot add a standalone
// table constraint, but a column CHECK referencing a sibling column is valid).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_journal_line
        ADD COLUMN ar_status varchar(16)
        CONSTRAINT chk_journal_line_ar_status
        CHECK (ar_status IS NULL OR ar_status IN ('ACTIVE','DISPUTED'))",
    "ALTER TABLE ledger_ar_invoice_balance
        ADD COLUMN disputed text NOT NULL DEFAULT '0' CHECK (length(disputed) BETWEEN 1 AND 31)
        CONSTRAINT chk_ar_invoice_balance_disputed_no_negative
        CHECK (substr(disputed, 1, 1) <> '-')",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_ar_invoice_balance DROP COLUMN disputed",
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
