//! Create the three payment counter tables in schema `bss`:
//! `payment_settlement` (the per-payment money-out serialization point —
//! `settled`/`fee`/`allocated`/`refunded`/`refunded_unallocated`/`clawed_back`
//! minor-unit counters guarded by cap CHECKs), `payment_allocation` (one
//! row per `(payment, invoice)` split + two read indexes), and
//! `payment_allocation_refund` (per-`(payment, invoice)` allocated/refunded
//! counter for Slice 3's refund cap). All CHECKs are created in final form
//! up-front (Foundation §7.2). `SQLite` mirrors the same shape with the
//! systematic transforms (`uuid`→`text`, `timestamptz`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_payment_settlement (
        tenant_id                  uuid         NOT NULL,
        payment_id                 varchar(128) NOT NULL,
        currency                   varchar(16)  NOT NULL,
        settled_minor              bigint       NOT NULL DEFAULT 0,
        fee_minor                  bigint       NOT NULL DEFAULT 0,
        allocated_minor            bigint       NOT NULL DEFAULT 0,
        refunded_minor             bigint       NOT NULL DEFAULT 0,
        refunded_unallocated_minor bigint       NOT NULL DEFAULT 0,
        clawed_back_minor          bigint       NOT NULL DEFAULT 0,
        version                    bigint       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payment_id),
        CONSTRAINT chk_payment_settlement_alloc_le_settled
            CHECK (allocated_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_alloc_refu_le_settled
            CHECK (allocated_minor + refunded_unallocated_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_fee_le_settled
            CHECK (fee_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_moneyout_le_settled
            CHECK (refunded_minor + clawed_back_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_refunded_le_settled
            CHECK (refunded_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_nonneg CHECK (
            settled_minor >= 0 AND fee_minor >= 0 AND allocated_minor >= 0
            AND refunded_minor >= 0 AND refunded_unallocated_minor >= 0
            AND clawed_back_minor >= 0)
    )",
    "CREATE TABLE bss.ledger_payment_allocation (
        tenant_id             uuid         NOT NULL,
        allocation_id         uuid         NOT NULL,
        payer_tenant_id       uuid         NOT NULL,
        payment_id            varchar(128) NOT NULL,
        invoice_id            varchar(128) NOT NULL,
        amount_minor          bigint       NOT NULL,
        currency              varchar(16)  NOT NULL,
        precedence_policy_ref varchar(128) NOT NULL,
        allocated_at_utc      timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, allocation_id, invoice_id),
        CONSTRAINT chk_payment_allocation_amount_pos CHECK (amount_minor > 0)
    )",
    "CREATE INDEX ix_payment_allocation_payment ON bss.ledger_payment_allocation (tenant_id, payment_id)",
    "CREATE INDEX ix_payment_allocation_invoice ON bss.ledger_payment_allocation (tenant_id, invoice_id)",
    "CREATE TABLE bss.ledger_payment_allocation_refund (
        tenant_id       uuid         NOT NULL,
        payment_id      varchar(128) NOT NULL,
        invoice_id      varchar(128) NOT NULL,
        allocated_minor bigint       NOT NULL DEFAULT 0,
        refunded_minor  bigint       NOT NULL DEFAULT 0,
        version         bigint       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payment_id, invoice_id),
        CONSTRAINT chk_par_refunded_le_allocated CHECK (refunded_minor <= allocated_minor),
        CONSTRAINT chk_par_nonneg CHECK (allocated_minor >= 0 AND refunded_minor >= 0)
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_payment_allocation_refund",
    "DROP TABLE IF EXISTS bss.ledger_payment_allocation",
    "DROP TABLE IF EXISTS bss.ledger_payment_settlement",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PKs + indexes preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_payment_settlement (
        tenant_id                  text         NOT NULL,
        payment_id                 varchar(128) NOT NULL,
        currency                   varchar(16)  NOT NULL,
        settled_minor              bigint       NOT NULL DEFAULT 0,
        fee_minor                  bigint       NOT NULL DEFAULT 0,
        allocated_minor            bigint       NOT NULL DEFAULT 0,
        refunded_minor             bigint       NOT NULL DEFAULT 0,
        refunded_unallocated_minor bigint       NOT NULL DEFAULT 0,
        clawed_back_minor          bigint       NOT NULL DEFAULT 0,
        version                    bigint       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payment_id),
        CONSTRAINT chk_payment_settlement_alloc_le_settled
            CHECK (allocated_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_alloc_refu_le_settled
            CHECK (allocated_minor + refunded_unallocated_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_fee_le_settled
            CHECK (fee_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_moneyout_le_settled
            CHECK (refunded_minor + clawed_back_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_refunded_le_settled
            CHECK (refunded_minor <= settled_minor),
        CONSTRAINT chk_payment_settlement_nonneg CHECK (
            settled_minor >= 0 AND fee_minor >= 0 AND allocated_minor >= 0
            AND refunded_minor >= 0 AND refunded_unallocated_minor >= 0
            AND clawed_back_minor >= 0)
    )",
    "CREATE TABLE ledger_payment_allocation (
        tenant_id             text         NOT NULL,
        allocation_id         text         NOT NULL,
        payer_tenant_id       text         NOT NULL,
        payment_id            varchar(128) NOT NULL,
        invoice_id            varchar(128) NOT NULL,
        amount_minor          bigint       NOT NULL,
        currency              varchar(16)  NOT NULL,
        precedence_policy_ref varchar(128) NOT NULL,
        allocated_at_utc      text         NOT NULL,
        PRIMARY KEY (tenant_id, allocation_id, invoice_id),
        CONSTRAINT chk_payment_allocation_amount_pos CHECK (amount_minor > 0)
    )",
    "CREATE INDEX ix_payment_allocation_payment ON ledger_payment_allocation (tenant_id, payment_id)",
    "CREATE INDEX ix_payment_allocation_invoice ON ledger_payment_allocation (tenant_id, invoice_id)",
    "CREATE TABLE ledger_payment_allocation_refund (
        tenant_id       text         NOT NULL,
        payment_id      varchar(128) NOT NULL,
        invoice_id      varchar(128) NOT NULL,
        allocated_minor bigint       NOT NULL DEFAULT 0,
        refunded_minor  bigint       NOT NULL DEFAULT 0,
        version         bigint       NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, payment_id, invoice_id),
        CONSTRAINT chk_par_refunded_le_allocated CHECK (refunded_minor <= allocated_minor),
        CONSTRAINT chk_par_nonneg CHECK (allocated_minor >= 0 AND refunded_minor >= 0)
    )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_payment_allocation_refund",
    "DROP TABLE IF EXISTS ledger_payment_allocation",
    "DROP TABLE IF EXISTS ledger_payment_settlement",
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
