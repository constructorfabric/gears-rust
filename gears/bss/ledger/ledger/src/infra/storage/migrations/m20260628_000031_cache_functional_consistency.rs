//! Require functional amount, currency and stored scale to be all present or
//! all absent on every functional-bearing balance cache. PostgreSQL adds the
//! checks here; SQLite already carries them in the fresh m003 declarations.

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
        CHECK ((functional_balance IS NULL) = (functional_currency IS NULL) AND (functional_balance IS NULL) = (functional_currency_scale IS NULL))",
    "ALTER TABLE bss.ledger_ar_invoice_balance
        ADD CONSTRAINT chk_ar_invoice_balance_func_consistency
        CHECK ((functional_balance IS NULL) = (functional_currency IS NULL) AND (functional_balance IS NULL) = (functional_currency_scale IS NULL))",
    "ALTER TABLE bss.ledger_ar_payer_balance
        ADD CONSTRAINT chk_ar_payer_balance_func_consistency
        CHECK ((functional_balance IS NULL) = (functional_currency IS NULL) AND (functional_balance IS NULL) = (functional_currency_scale IS NULL))",
    "ALTER TABLE bss.ledger_unallocated_balance
        ADD CONSTRAINT chk_unallocated_balance_func_consistency
        CHECK ((functional_balance IS NULL) = (functional_currency IS NULL) AND (functional_balance IS NULL) = (functional_currency_scale IS NULL))",
    "ALTER TABLE bss.ledger_reusable_credit_subbalance
        ADD CONSTRAINT chk_reusable_credit_func_consistency
        CHECK ((functional_balance IS NULL) = (functional_currency IS NULL) AND (functional_balance IS NULL) = (functional_currency_scale IS NULL))",
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
