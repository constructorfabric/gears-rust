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
        currency              varchar(16)  NOT NULL,
        settled                     text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(settled, currency_scale)),
        fee                         text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(fee, currency_scale)),
        allocated                   text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(allocated, currency_scale)),
        refunded                    text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(refunded, currency_scale)),
        refunded_unallocated        text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(refunded_unallocated, currency_scale)),
        clawed_back                 text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(clawed_back, currency_scale)),
        version         bigint       NOT NULL DEFAULT 0,
        currency_scale              smallint NOT NULL CHECK (currency_scale BETWEEN 0 AND 28),
        PRIMARY KEY (tenant_id, payment_id),
        CONSTRAINT chk_payment_settlement_alloc_le_settled
            CHECK (allocated::numeric <= settled::numeric),
        CONSTRAINT chk_payment_settlement_alloc_refu_le_settled
            CHECK (allocated::numeric + refunded_unallocated::numeric <= settled::numeric),
        CONSTRAINT chk_payment_settlement_fee_le_settled
            CHECK (fee::numeric <= settled::numeric),
        CONSTRAINT chk_payment_settlement_moneyout_le_settled
            CHECK (refunded::numeric + clawed_back::numeric <= settled::numeric),
        CONSTRAINT chk_payment_settlement_refunded_le_settled
            CHECK (refunded::numeric <= settled::numeric),
        CONSTRAINT chk_payment_settlement_nonneg CHECK (
            settled::numeric >= 0 AND fee::numeric >= 0 AND allocated::numeric >= 0
            AND refunded::numeric >= 0 AND refunded_unallocated::numeric >= 0
            AND clawed_back::numeric >= 0)
    )",
    "CREATE TABLE bss.ledger_payment_allocation (
        tenant_id             uuid         NOT NULL,
        allocation_id         uuid         NOT NULL,
        payer_tenant_id       uuid         NOT NULL,
        payment_id            varchar(128) NOT NULL,
        invoice_id            varchar(128) NOT NULL,
        amount                      text       NOT NULL CHECK (bss.ledger_decimal_valid(amount, currency_scale)),
        currency              varchar(16)  NOT NULL,
        precedence_policy_ref varchar(128) NOT NULL,
        allocated_at_utc      timestamptz  NOT NULL,
        currency_scale              smallint NOT NULL CHECK (currency_scale BETWEEN 0 AND 28),
        PRIMARY KEY (tenant_id, allocation_id, invoice_id),
        CONSTRAINT chk_payment_allocation_amount_pos CHECK (amount::numeric > 0)
    )",
    "CREATE INDEX ix_payment_allocation_payment ON bss.ledger_payment_allocation (tenant_id, payment_id)",
    "CREATE INDEX ix_payment_allocation_invoice ON bss.ledger_payment_allocation (tenant_id, invoice_id)",
    "CREATE TABLE bss.ledger_payment_allocation_refund (
        tenant_id       uuid         NOT NULL,
        payment_id      varchar(128) NOT NULL,
        invoice_id      varchar(128) NOT NULL,
        allocated                   text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(allocated, currency_scale)),
        refunded                    text       NOT NULL DEFAULT '0' CHECK (bss.ledger_decimal_valid(refunded, currency_scale)),
        version         bigint       NOT NULL DEFAULT 0,
        currency              varchar(16) NOT NULL,
        currency_scale              smallint NOT NULL CHECK (currency_scale BETWEEN 0 AND 28),
        PRIMARY KEY (tenant_id, payment_id, invoice_id),
        CONSTRAINT chk_par_refunded_le_allocated CHECK (refunded::numeric <= allocated::numeric),
        CONSTRAINT chk_par_nonneg CHECK (allocated::numeric >= 0 AND refunded::numeric >= 0)
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_payment_allocation_refund",
    "DROP TABLE IF EXISTS bss.ledger_payment_allocation",
    "DROP TABLE IF EXISTS bss.ledger_payment_settlement",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; metadata/sign CHECKs + PKs + indexes preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_payment_settlement (
        tenant_id       text         NOT NULL,
        payment_id                 varchar(128) NOT NULL,
        currency              varchar(16)  NOT NULL,
        settled                     text       NOT NULL DEFAULT '0' CHECK (length(settled) BETWEEN 1 AND 31),
        fee                         text       NOT NULL DEFAULT '0' CHECK (length(fee) BETWEEN 1 AND 31),
        allocated                   text       NOT NULL DEFAULT '0' CHECK (length(allocated) BETWEEN 1 AND 31),
        refunded                    text       NOT NULL DEFAULT '0' CHECK (length(refunded) BETWEEN 1 AND 31),
        refunded_unallocated        text       NOT NULL DEFAULT '0' CHECK (length(refunded_unallocated) BETWEEN 1 AND 31),
        clawed_back                 text       NOT NULL DEFAULT '0' CHECK (length(clawed_back) BETWEEN 1 AND 31),
        version         bigint       NOT NULL DEFAULT 0,
        currency_scale              smallint NOT NULL CHECK (currency_scale BETWEEN 0 AND 28),
        PRIMARY KEY (tenant_id, payment_id),
        CONSTRAINT chk_payment_settlement_nonneg CHECK (
            substr(settled, 1, 1) <> '-' AND substr(fee, 1, 1) <> '-' AND substr(allocated, 1, 1) <> '-'
            AND substr(refunded, 1, 1) <> '-' AND substr(refunded_unallocated, 1, 1) <> '-'
            AND substr(clawed_back, 1, 1) <> '-')
    )",
    "CREATE TABLE ledger_payment_allocation (
        tenant_id       text         NOT NULL,
        allocation_id         text         NOT NULL,
        payer_tenant_id       text         NOT NULL,
        payment_id            varchar(128) NOT NULL,
        invoice_id            varchar(128) NOT NULL,
        amount                      text       NOT NULL CHECK (length(amount) BETWEEN 1 AND 31),
        currency              varchar(16)  NOT NULL,
        precedence_policy_ref varchar(128) NOT NULL,
        allocated_at_utc      text         NOT NULL,
        currency_scale              smallint NOT NULL CHECK (currency_scale BETWEEN 0 AND 28),
        PRIMARY KEY (tenant_id, allocation_id, invoice_id),
        CONSTRAINT chk_payment_allocation_amount_pos CHECK ((substr(amount, 1, 1) <> '-' AND amount <> '0'))
    )",
    "CREATE INDEX ix_payment_allocation_payment ON ledger_payment_allocation (tenant_id, payment_id)",
    "CREATE INDEX ix_payment_allocation_invoice ON ledger_payment_allocation (tenant_id, invoice_id)",
    "CREATE TABLE ledger_payment_allocation_refund (
        tenant_id       text         NOT NULL,
        payment_id      varchar(128) NOT NULL,
        invoice_id      varchar(128) NOT NULL,
        allocated                   text       NOT NULL DEFAULT '0' CHECK (length(allocated) BETWEEN 1 AND 31),
        refunded                    text       NOT NULL DEFAULT '0' CHECK (length(refunded) BETWEEN 1 AND 31),
        version         bigint       NOT NULL DEFAULT 0,
        currency              varchar(16) NOT NULL,
        currency_scale              smallint NOT NULL CHECK (currency_scale BETWEEN 0 AND 28),
        PRIMARY KEY (tenant_id, payment_id, invoice_id),
        CONSTRAINT chk_par_nonneg CHECK (substr(allocated, 1, 1) <> '-' AND substr(refunded, 1, 1) <> '-')
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
            _ => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
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
            _ => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
