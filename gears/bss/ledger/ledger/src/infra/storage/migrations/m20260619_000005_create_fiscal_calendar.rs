//! Create the `fiscal_calendar` reference table (per-legal-entity calendar
//! config: timezone, granularity, FY-start month) in schema `bss`. `SQLite`
//! mirrors the same shape with the systematic transforms; every CHECK and PK
//! is kept.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.ledger_fiscal_calendar (
        tenant_id       uuid        NOT NULL,
        legal_entity_id uuid        NOT NULL,
        fiscal_tz       varchar(64) NOT NULL,
        granularity     text        NOT NULL CHECK (granularity IN ('MONTH')),
        fy_start_month  smallint    NOT NULL CHECK (fy_start_month BETWEEN 1 AND 12),
        PRIMARY KEY (tenant_id, legal_entity_id)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_fiscal_calendar"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`;
// all CHECKs + PK preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE ledger_fiscal_calendar (
        tenant_id       text        NOT NULL,
        legal_entity_id text        NOT NULL,
        fiscal_tz       varchar(64) NOT NULL,
        granularity     text        NOT NULL CHECK (granularity IN ('MONTH')),
        fy_start_month  smallint    NOT NULL CHECK (fy_start_month BETWEEN 1 AND 12),
        PRIMARY KEY (tenant_id, legal_entity_id)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_fiscal_calendar"];

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
