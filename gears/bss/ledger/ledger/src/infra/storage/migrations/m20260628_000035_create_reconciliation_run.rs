//! Create the reconciliation-run table in schema `bss`:
//! `ledger_reconciliation_run` (one row per check execution, keyed by
//! `(tenant_id, run_id)`) — Slice 7 (design §4.3/§7). Records the variance of an
//! AR↔derived / Payments↔PSP / invoice-completeness check for a period; an
//! out-of-tolerance run opens an `exception_queue` row and feeds the close gate.
//! `watermark` is the max in-period `created_seq` the run covered (the close
//! flip checks it). (Ledger↔ERP / GL check types are deferred with ERP export,
//! VHP-1948.) `SQLite` mirrors the shape (`uuid`→`text`, `jsonb`→`text`,
//! `boolean`→`boolean DEFAULT 1`, `timestamptz`→`text`, no `bss.` prefix);
//! CHECKs, PK, index preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_reconciliation_run (
        tenant_id        uuid         NOT NULL,
        run_id           uuid         NOT NULL,
        period_id        varchar(64)  NOT NULL,
        check_type       varchar(32)  NOT NULL,
        variance_minor   bigint       NOT NULL DEFAULT 0,
        within_tolerance boolean      NOT NULL DEFAULT true,
        status           varchar(16)  NOT NULL,
        watermark        bigint,
        detail           jsonb,
        at_utc           timestamptz  NOT NULL DEFAULT now(),
        PRIMARY KEY (tenant_id, run_id),
        CONSTRAINT chk_ledger_reconciliation_run_check_type CHECK (check_type IN (
            'AR_DERIVED','PAYMENTS_PSP','INVOICE_COMPLETENESS')),
        CONSTRAINT chk_ledger_reconciliation_run_status CHECK (status IN (
            'RUNNING','DONE','FAILED'))
    )",
    "CREATE INDEX ledger_reconciliation_run_period_idx
        ON bss.ledger_reconciliation_run (tenant_id, period_id, check_type)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_reconciliation_run"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_reconciliation_run (
        tenant_id        text         NOT NULL,
        run_id           text         NOT NULL,
        period_id        varchar(64)  NOT NULL,
        check_type       varchar(32)  NOT NULL,
        variance_minor   bigint       NOT NULL DEFAULT 0,
        within_tolerance boolean      NOT NULL DEFAULT 1,
        status           varchar(16)  NOT NULL,
        watermark        bigint,
        detail           text,
        at_utc           text         NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        PRIMARY KEY (tenant_id, run_id),
        CONSTRAINT chk_ledger_reconciliation_run_check_type CHECK (check_type IN (
            'AR_DERIVED','PAYMENTS_PSP','INVOICE_COMPLETENESS')),
        CONSTRAINT chk_ledger_reconciliation_run_status CHECK (status IN (
            'RUNNING','DONE','FAILED'))
    )",
    "CREATE INDEX ledger_reconciliation_run_period_idx
        ON ledger_reconciliation_run (tenant_id, period_id, check_type)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_reconciliation_run"];

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
