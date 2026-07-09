//! Create the period-close process table in schema `bss`:
//! `ledger_period_close` (one row per `(tenant_id, legal_entity_id, period_id)`)
//! — the owner of the close *process* lifecycle (OPEN → CLOSING → CLOSED →
//! REOPENED) that the Foundation `fiscal_period.status` posting-gate flips
//! alongside (Slice 7, design §4.1/§4.5/§7). During CLOSING the `fiscal_period`
//! row stays OPEN (concurrent posts still allowed under `FOR SHARE`); the flip
//! commits `period_close CLOSING→CLOSED` and `fiscal_period OPEN→CLOSED` in the
//! same txn. `blocked_reasons` records the last gate result; `recon_watermark`
//! is the max in-period `created_seq` the CLOSING recompute covered.
//! `SQLite` (non-production test backend) mirrors the shape with the systematic
//! transforms (`uuid`→`text`, `jsonb`→`text`, `bigint`→`integer`,
//! `timestamptz`→`text`, no `bss.` prefix). The CHECK, PK are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.ledger_period_close (
        tenant_id          uuid         NOT NULL,
        legal_entity_id    uuid         NOT NULL,
        period_id          varchar(64)  NOT NULL,
        status             varchar(16)  NOT NULL,
        initiated_by       varchar(256) NOT NULL,
        blocked_reasons    jsonb,
        recon_watermark    bigint,
        reopen_approval_id uuid,
        reopened_by        varchar(256),
        closed_at          timestamptz,
        PRIMARY KEY (tenant_id, legal_entity_id, period_id),
        CONSTRAINT chk_ledger_period_close_status
            CHECK (status IN ('OPEN','CLOSING','CLOSED','REOPENED'))
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_period_close"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE ledger_period_close (
        tenant_id          text         NOT NULL,
        legal_entity_id    text         NOT NULL,
        period_id          varchar(64)  NOT NULL,
        status             varchar(16)  NOT NULL,
        initiated_by       varchar(256) NOT NULL,
        blocked_reasons    text,
        recon_watermark    integer,
        reopen_approval_id text,
        reopened_by        varchar(256),
        closed_at          text,
        PRIMARY KEY (tenant_id, legal_entity_id, period_id),
        CONSTRAINT chk_ledger_period_close_status
            CHECK (status IN ('OPEN','CLOSING','CLOSED','REOPENED'))
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_period_close"];

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
