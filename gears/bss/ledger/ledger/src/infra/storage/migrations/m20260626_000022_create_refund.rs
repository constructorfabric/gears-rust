//! Create the Slice 3 Phase-2 `ledger_refund` record table in schema `bss` — the
//! record of a PSP refund's two-stage lifecycle (design §4.1 ERD / §7), keyed by
//! the surrogate `(tenant_id, refund_id)`.
//!
//! **PK vs UNIQUE.** `refund_id` is the **business/surrogate** id and the
//! `SecureORM` `resource_col`; like its sibling note tables (`credit_note` /
//! `debit_note`) it is a `varchar(128)` (NOT a `uuid` column) so it lines up with
//! the `resource_col`. The PK is the surrogate `(tenant_id, refund_id)`. The
//! **idempotency grain** is the natural `(tenant_id, psp_refund_id, phase)`
//! (design §7) — that is a SEPARATE `UNIQUE` index, NOT the PK: a single PSP
//! refund advances through several `phase` rows (`initiated → confirmed`, or
//! `rejected`/`voided`), and we want one row per `(psp_refund_id, phase)` for
//! idempotent phase-transition recording while keeping a stable single-column
//! surrogate handle for REST (`GET /refunds/{refundId}`) and for the
//! `relates_to_refund_id` self-reference. Hence surrogate PK + natural UNIQUE,
//! mirroring the design's `refund_id PK` + `UNIQUE (tenant_id, psp_refund_id,
//! phase)`.
//!
//! Both refund **patterns** carry the origin `payment_id` NOT NULL (design §9 D7
//! assumption / Rev2 B-1); `invoice_id` is required for Pattern B (`B_RESTORE_AR`)
//! and NULL for Pattern A (`A_UNALLOCATED`). `clearing_state` tracks the two-stage
//! `REFUND_CLEARING` drain (`PENDING → SETTLED`, or `REVERSED` on PSP
//! reject/void). `reverses_entry_id` is set ONLY on the stage-1 line-negation when
//! the PSP rejected/voided an initiated refund (it references the negated journal
//! entry); `relates_to_refund_id` is the refund-of-refund forward link
//! (claw-back / additional-outbound).
//!
//! All CHECKs are created in final form up-front (Foundation §7.2). `SQLite`
//! mirrors the same shape with the systematic transforms (`uuid`→`text`,
//! `timestamptz`→`text`); the CHECKs + PK + UNIQUE index are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_refund (
        tenant_id             uuid          NOT NULL,
        refund_id             varchar(128)  NOT NULL,
        psp_refund_id         varchar(128)  NOT NULL,
        phase                 varchar(16)   NOT NULL,
        pattern               varchar(16)   NOT NULL,
        payment_id            varchar(128)  NOT NULL,
        invoice_id            varchar(128),
        currency              varchar(16)   NOT NULL,
        amount_minor          bigint        NOT NULL,
        clearing_state        varchar(16)   NOT NULL,
        relates_to_refund_id  varchar(128),
        reverses_entry_id     uuid,
        created_at_utc        timestamptz   NOT NULL,
        version               bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, refund_id),
        CONSTRAINT chk_ledger_refund_phase CHECK (phase IN
            ('initiated','confirmed','rejected','voided','unknown_final')),
        CONSTRAINT chk_ledger_refund_pattern CHECK (pattern IN
            ('A_UNALLOCATED','B_RESTORE_AR')),
        CONSTRAINT chk_ledger_refund_clearing_state CHECK (clearing_state IN
            ('PENDING','SETTLED','REVERSED')),
        CONSTRAINT chk_ledger_refund_amount_nonneg CHECK (amount_minor >= 0)
    )",
    "CREATE UNIQUE INDEX uq_ledger_refund_psp_phase
        ON bss.ledger_refund (tenant_id, psp_refund_id, phase)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_refund"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PK + UNIQUE index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_refund (
        tenant_id             text          NOT NULL,
        refund_id             varchar(128)  NOT NULL,
        psp_refund_id         varchar(128)  NOT NULL,
        phase                 varchar(16)   NOT NULL,
        pattern               varchar(16)   NOT NULL,
        payment_id            varchar(128)  NOT NULL,
        invoice_id            varchar(128),
        currency              varchar(16)   NOT NULL,
        amount_minor          bigint        NOT NULL,
        clearing_state        varchar(16)   NOT NULL,
        relates_to_refund_id  varchar(128),
        reverses_entry_id     text,
        created_at_utc        text          NOT NULL,
        version               bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, refund_id),
        CONSTRAINT chk_ledger_refund_phase CHECK (phase IN
            ('initiated','confirmed','rejected','voided','unknown_final')),
        CONSTRAINT chk_ledger_refund_pattern CHECK (pattern IN
            ('A_UNALLOCATED','B_RESTORE_AR')),
        CONSTRAINT chk_ledger_refund_clearing_state CHECK (clearing_state IN
            ('PENDING','SETTLED','REVERSED')),
        CONSTRAINT chk_ledger_refund_amount_nonneg CHECK (amount_minor >= 0)
    )",
    "CREATE UNIQUE INDEX uq_ledger_refund_psp_phase
        ON ledger_refund (tenant_id, psp_refund_id, phase)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_refund"];

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
