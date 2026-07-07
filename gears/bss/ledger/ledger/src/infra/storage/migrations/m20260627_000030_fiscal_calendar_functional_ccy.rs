//! Add the legal-entity **functional currency** (`functional_currency`) to
//! `ledger_fiscal_calendar` (Slice 5 — the S5-F3 functional-currency source).
//!
//! The functional currency is per-legal-entity accounting config (F5: one per
//! LE), so it rides the existing per-`(tenant, legal_entity)` calendar row
//! alongside the timezone / granularity / FY-start. **Nullable**: a tenant with no
//! configured functional currency is treated as single-currency (the
//! `RateLocker` short-circuits, functional stays NULL, byte-green as today); FX
//! activates per-tenant once a functional currency is seeded and a transaction
//! currency differs from it.
//!
//! Design §S5-F3 places the source-of-truth in account-management, but AM models
//! no accounting currency and is upstream (gears-rust); the ledger therefore owns
//! it as provisioning reference data (the FX analogue of the §0.1 / Slice-4
//! Variant-A "the gear completes the substrate the design assumed upstream").
//! Additive nullable column → no table rebuild on either backend.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] =
    &["ALTER TABLE bss.ledger_fiscal_calendar ADD COLUMN functional_currency varchar(16)"];

const PG_DOWN_STATEMENTS: &[&str] =
    &["ALTER TABLE bss.ledger_fiscal_calendar DROP COLUMN IF EXISTS functional_currency"];

const SQLITE_UP_STATEMENTS: &[&str] =
    &["ALTER TABLE ledger_fiscal_calendar ADD COLUMN functional_currency varchar(16)"];

const SQLITE_DOWN_STATEMENTS: &[&str] =
    &["ALTER TABLE ledger_fiscal_calendar DROP COLUMN functional_currency"];

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
