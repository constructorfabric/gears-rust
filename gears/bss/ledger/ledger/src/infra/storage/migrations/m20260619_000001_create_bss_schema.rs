//! Create the `bss` Postgres schema. Postgres-only: `SQLite` has a single
//! namespace, so the `SQLite` branch is a no-op and later table migrations
//! create unqualified tables that resolve into `bss` via `search_path`.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// Validate canonical decimal storage without rounding or approximate arithmetic.
const DECIMAL_VALIDATOR: &str =
    "CREATE OR REPLACE FUNCTION bss.ledger_decimal_valid(value text, stored_scale integer)
RETURNS boolean LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
    IF value IS NULL THEN RETURN true; END IF;
    IF stored_scale IS NULL OR stored_scale NOT BETWEEN 0 AND 28 THEN RETURN false; END IF;
    IF length(value) NOT BETWEEN 1 AND 31
       OR value !~ '^-?(0|[1-9][0-9]*)([.][0-9]*[1-9])?$'
       OR value = '-0' THEN RETURN false; END IF;
    RETURN length(ltrim(replace(replace(value, '-', ''), '.', ''), '0')) <= 28
       AND length(split_part(value, '.', 2)) <= stored_scale
       AND abs(value::numeric) < 10000000000000000000000000000::numeric;
END;
$$";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                conn.execute_raw(Statement::from_string(
                    backend,
                    "CREATE SCHEMA IF NOT EXISTS bss".to_owned(),
                ))
                .await?;
                // Decimal text is canonical, bounded and aligned with stored money scale.
                // No floating-point conversion participates in these checks.
                conn.execute_raw(Statement::from_string(
                    backend,
                    DECIMAL_VALIDATOR.to_owned(),
                ))
                .await?;
            }
            sea_orm::DatabaseBackend::Sqlite => { /* single namespace; no-op */ }
            _ => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        if backend == sea_orm::DatabaseBackend::Postgres {
            conn.execute_raw(Statement::from_string(
                backend,
                "DROP SCHEMA IF EXISTS bss CASCADE".to_owned(),
            ))
            .await?;
        }
        Ok(())
    }
}
