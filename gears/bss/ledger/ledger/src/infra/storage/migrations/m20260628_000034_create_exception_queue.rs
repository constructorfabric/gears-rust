//! Create the durable, close-blocking exception queue in schema `bss`:
//! `ledger_exception_queue` (one row per open exception, keyed by
//! `(tenant_id, exception_id)`) — Slice 7 (design §4.6/§7). The per-slice
//! exception *stubs* (stuck-refund-clearing, clawback-underflow,
//! credit-note-split-blocked, chargeback-on-refunded, …) and the reconciliation
//! framework open rows here; the period-close gate blocks while any OPEN
//! close-blocking row exists for the period. `GL_WRITEOFF_VARIANCE` is the one
//! type Finance acknowledges → `APPROVED_EXCEPTION` (then non-blocking).
//! `SQLite` mirrors the shape (`uuid`→`text`, `jsonb`→`text`,
//! `timestamptz`→`text`, no `bss.` prefix); CHECKs, PK, indexes (incl. the
//! partial OPEN index) preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_exception_queue (
        tenant_id      uuid         NOT NULL,
        exception_id   uuid         NOT NULL,
        exception_type varchar(48)  NOT NULL,
        business_ref   varchar(256) NOT NULL,
        status         varchar(24)  NOT NULL DEFAULT 'OPEN',
        period_id      varchar(64),
        detail         jsonb,
        opened_at      timestamptz  NOT NULL DEFAULT now(),
        resolved_at    timestamptz,
        resolved_by    varchar(256),
        PRIMARY KEY (tenant_id, exception_id),
        CONSTRAINT chk_ledger_exception_queue_type CHECK (exception_type IN (
            'SETTLED_NO_MATCH','MAPPING_GAP','RECON_MISMATCH','PSP_VARIANCE',
            'SPLIT_AMBIGUOUS','RECOGNITION_POLICY_CONFLICT','UNSCHEDULED_DEFERRAL',
            'STUCK_REFUND_CLEARING','SETTLEMENT_RETURN_OVER_ALLOCATED',
            'CHARGEBACK_ON_REFUNDED','GL_WRITEOFF_VARIANCE','MISSED_POSTING')),
        CONSTRAINT chk_ledger_exception_queue_status CHECK (status IN (
            'OPEN','ACK','RESOLVED','APPROVED_EXCEPTION'))
    )",
    "CREATE INDEX ledger_exception_queue_open_idx
        ON bss.ledger_exception_queue (tenant_id, period_id) WHERE status = 'OPEN'",
    "CREATE INDEX ledger_exception_queue_type_idx
        ON bss.ledger_exception_queue (tenant_id, exception_type, status)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_exception_queue"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_exception_queue (
        tenant_id      text         NOT NULL,
        exception_id   text         NOT NULL,
        exception_type varchar(48)  NOT NULL,
        business_ref   varchar(256) NOT NULL,
        status         varchar(24)  NOT NULL DEFAULT 'OPEN',
        period_id      varchar(64),
        detail         text,
        opened_at      text         NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        resolved_at    text,
        resolved_by    varchar(256),
        PRIMARY KEY (tenant_id, exception_id),
        CONSTRAINT chk_ledger_exception_queue_type CHECK (exception_type IN (
            'SETTLED_NO_MATCH','MAPPING_GAP','RECON_MISMATCH','PSP_VARIANCE',
            'SPLIT_AMBIGUOUS','RECOGNITION_POLICY_CONFLICT','UNSCHEDULED_DEFERRAL',
            'STUCK_REFUND_CLEARING','SETTLEMENT_RETURN_OVER_ALLOCATED',
            'CHARGEBACK_ON_REFUNDED','GL_WRITEOFF_VARIANCE','MISSED_POSTING')),
        CONSTRAINT chk_ledger_exception_queue_status CHECK (status IN (
            'OPEN','ACK','RESOLVED','APPROVED_EXCEPTION'))
    )",
    "CREATE INDEX ledger_exception_queue_open_idx
        ON ledger_exception_queue (tenant_id, period_id) WHERE status = 'OPEN'",
    "CREATE INDEX ledger_exception_queue_type_idx
        ON ledger_exception_queue (tenant_id, exception_type, status)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_exception_queue"];

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
