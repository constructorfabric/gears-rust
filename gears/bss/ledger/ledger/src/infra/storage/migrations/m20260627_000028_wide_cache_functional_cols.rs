//! Add optional canonical functional money and stored scale to wide caches.
//! PostgreSQL adds these columns here; SQLite declares the triplets and their
//! co-null constraints in m003 because it cannot add table constraints in place.
//! Migration names and order remain unchanged for the fresh schema chain.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_account_balance ADD COLUMN functional_balance text",
    "ALTER TABLE bss.ledger_account_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE bss.ledger_account_balance ADD COLUMN functional_currency_scale smallint CHECK (functional_currency_scale BETWEEN 0 AND 28)",
    "ALTER TABLE bss.ledger_account_balance ADD CONSTRAINT chk_ledger_account_balance_functional_decimal CHECK (bss.ledger_decimal_valid(functional_balance, functional_currency_scale))",
    "ALTER TABLE bss.ledger_ar_invoice_balance ADD COLUMN functional_balance text",
    "ALTER TABLE bss.ledger_ar_invoice_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE bss.ledger_ar_invoice_balance ADD COLUMN functional_currency_scale smallint CHECK (functional_currency_scale BETWEEN 0 AND 28)",
    "ALTER TABLE bss.ledger_ar_invoice_balance ADD CONSTRAINT chk_ledger_ar_invoice_balance_functional_decimal CHECK (bss.ledger_decimal_valid(functional_balance, functional_currency_scale))",
    "ALTER TABLE bss.ledger_ar_payer_balance ADD COLUMN functional_balance text",
    "ALTER TABLE bss.ledger_ar_payer_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE bss.ledger_ar_payer_balance ADD COLUMN functional_currency_scale smallint CHECK (functional_currency_scale BETWEEN 0 AND 28)",
    "ALTER TABLE bss.ledger_ar_payer_balance ADD CONSTRAINT chk_ledger_ar_payer_balance_functional_decimal CHECK (bss.ledger_decimal_valid(functional_balance, functional_currency_scale))",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_ar_payer_balance DROP CONSTRAINT IF EXISTS chk_ledger_ar_payer_balance_functional_decimal",
    "ALTER TABLE bss.ledger_ar_payer_balance DROP COLUMN IF EXISTS functional_currency_scale",
    "ALTER TABLE bss.ledger_ar_payer_balance DROP COLUMN IF EXISTS functional_currency",
    "ALTER TABLE bss.ledger_ar_payer_balance DROP COLUMN IF EXISTS functional_balance",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP CONSTRAINT IF EXISTS chk_ledger_ar_invoice_balance_functional_decimal",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS functional_currency_scale",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS functional_currency",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS functional_balance",
    "ALTER TABLE bss.ledger_account_balance DROP CONSTRAINT IF EXISTS chk_ledger_account_balance_functional_decimal",
    "ALTER TABLE bss.ledger_account_balance DROP COLUMN IF EXISTS functional_currency_scale",
    "ALTER TABLE bss.ledger_account_balance DROP COLUMN IF EXISTS functional_currency",
    "ALTER TABLE bss.ledger_account_balance DROP COLUMN IF EXISTS functional_balance",
];

// ---------------------------------------------------------------------------
// SQLite triplets and checks are already present in the fresh m003 schema.
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
