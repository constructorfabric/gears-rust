//! Add the dual-currency functional columns (`functional_balance_minor`,
//! `functional_currency`) to the three wide balance caches that lacked them
//! (`account_balance`, `ar_invoice_balance`, `ar_payer_balance`). The two
//! narrow caches `unallocated_balance` / `reusable_credit_subbalance` already
//! carry them from P1. Populated by the `BalanceProjector` on cross-currency
//! posts (Slice 5 Phase 1, group B); left NULL on single-currency grains, where
//! the functional balance equals the transaction balance by identity.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_account_balance ADD COLUMN functional_balance_minor bigint",
    "ALTER TABLE bss.ledger_account_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE bss.ledger_ar_invoice_balance ADD COLUMN functional_balance_minor bigint",
    "ALTER TABLE bss.ledger_ar_invoice_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE bss.ledger_ar_payer_balance ADD COLUMN functional_balance_minor bigint",
    "ALTER TABLE bss.ledger_ar_payer_balance ADD COLUMN functional_currency varchar(16)",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_ar_payer_balance DROP COLUMN IF EXISTS functional_currency",
    "ALTER TABLE bss.ledger_ar_payer_balance DROP COLUMN IF EXISTS functional_balance_minor",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS functional_currency",
    "ALTER TABLE bss.ledger_ar_invoice_balance DROP COLUMN IF EXISTS functional_balance_minor",
    "ALTER TABLE bss.ledger_account_balance DROP COLUMN IF EXISTS functional_currency",
    "ALTER TABLE bss.ledger_account_balance DROP COLUMN IF EXISTS functional_balance_minor",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `varchar`→text affinity).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_account_balance ADD COLUMN functional_balance_minor bigint",
    "ALTER TABLE ledger_account_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE ledger_ar_invoice_balance ADD COLUMN functional_balance_minor bigint",
    "ALTER TABLE ledger_ar_invoice_balance ADD COLUMN functional_currency varchar(16)",
    "ALTER TABLE ledger_ar_payer_balance ADD COLUMN functional_balance_minor bigint",
    "ALTER TABLE ledger_ar_payer_balance ADD COLUMN functional_currency varchar(16)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_ar_payer_balance DROP COLUMN functional_currency",
    "ALTER TABLE ledger_ar_payer_balance DROP COLUMN functional_balance_minor",
    "ALTER TABLE ledger_ar_invoice_balance DROP COLUMN functional_currency",
    "ALTER TABLE ledger_ar_invoice_balance DROP COLUMN functional_balance_minor",
    "ALTER TABLE ledger_account_balance DROP COLUMN functional_currency",
    "ALTER TABLE ledger_account_balance DROP COLUMN functional_balance_minor",
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
