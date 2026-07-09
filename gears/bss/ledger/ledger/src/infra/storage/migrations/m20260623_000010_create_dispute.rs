//! Create the chargeback dispute-state table in schema `bss`:
//! `ledger_dispute` (one row per dispute, keyed by `(tenant_id, dispute_id)`).
//! The PK holds the *current* state; `cycle` increments in place on a re-open
//! (pre-arbitration → arbitration), and per-phase history lives in the journal
//! and `idempotency_dedup` (`dispute_id:cycle:phase`). The `variant`
//! (`CASH_HOLD` | `AR_RECLASS`) is chosen by the LEDGER at `opened` from the
//! request's `funds_at_open` fact and recorded here; the `won`/`lost` outcomes
//! branch on it (design §0 D1 / §1 / §2).
//!
//! Lock order (reconciled with the `PostingService` project→sidecar flow, Slice 2):
//! the balance caches are projected FIRST in the post txn, then the
//! in-txn sidecar writes the counter rows (`ledger_payment_settlement`,
//! `ledger_dispute`, `payment_allocation_refund`) — i.e. the counters are taken
//! AFTER the balance grains, not before. All Slice 2 posts share this one order,
//! so the total lock order stays acyclic; Slice 3 must follow it.
//!
//! All CHECKs are created in final form up-front (Foundation §7.2). `SQLite`
//! mirrors the same shape with the systematic transforms (`uuid`→`text`); the
//! CHECKs + PK + index are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_dispute (
        tenant_id             uuid          NOT NULL,
        dispute_id            varchar(128)  NOT NULL,
        payment_id            varchar(128)  NOT NULL,
        currency              varchar(16)   NOT NULL,
        variant               varchar(16)   NOT NULL,
        last_phase            varchar(16)   NOT NULL,
        cycle                 integer       NOT NULL DEFAULT 1,
        disputed_amount_minor bigint        NOT NULL DEFAULT 0,
        cash_hold_minor       bigint        NOT NULL DEFAULT 0,
        version               bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, dispute_id),
        CONSTRAINT chk_ledger_dispute_variant
            CHECK (variant IN ('CASH_HOLD','AR_RECLASS')),
        CONSTRAINT chk_ledger_dispute_last_phase
            CHECK (last_phase IN ('OPENED','WON','LOST','PARTIAL')),
        CONSTRAINT chk_ledger_dispute_cycle CHECK (cycle >= 1),
        CONSTRAINT chk_ledger_dispute_amount_nonneg CHECK (disputed_amount_minor >= 0),
        CONSTRAINT chk_ledger_dispute_cash_hold_nonneg CHECK (cash_hold_minor >= 0),
        CONSTRAINT chk_ledger_dispute_cash_hold_le_disputed
            CHECK (cash_hold_minor <= disputed_amount_minor)
    )",
    "CREATE INDEX ledger_dispute_payment_idx ON bss.ledger_dispute (tenant_id, payment_id)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_dispute"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`; the
// CHECKs + PK + index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_dispute (
        tenant_id             text          NOT NULL,
        dispute_id            varchar(128)  NOT NULL,
        payment_id            varchar(128)  NOT NULL,
        currency              varchar(16)   NOT NULL,
        variant               varchar(16)   NOT NULL,
        last_phase            varchar(16)   NOT NULL,
        cycle                 integer       NOT NULL DEFAULT 1,
        disputed_amount_minor bigint        NOT NULL DEFAULT 0,
        cash_hold_minor       bigint        NOT NULL DEFAULT 0,
        version               bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, dispute_id),
        CONSTRAINT chk_ledger_dispute_variant
            CHECK (variant IN ('CASH_HOLD','AR_RECLASS')),
        CONSTRAINT chk_ledger_dispute_last_phase
            CHECK (last_phase IN ('OPENED','WON','LOST','PARTIAL')),
        CONSTRAINT chk_ledger_dispute_cycle CHECK (cycle >= 1),
        CONSTRAINT chk_ledger_dispute_amount_nonneg CHECK (disputed_amount_minor >= 0),
        CONSTRAINT chk_ledger_dispute_cash_hold_nonneg CHECK (cash_hold_minor >= 0),
        CONSTRAINT chk_ledger_dispute_cash_hold_le_disputed
            CHECK (cash_hold_minor <= disputed_amount_minor)
    )",
    "CREATE INDEX ledger_dispute_payment_idx ON ledger_dispute (tenant_id, payment_id)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_dispute"];

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
