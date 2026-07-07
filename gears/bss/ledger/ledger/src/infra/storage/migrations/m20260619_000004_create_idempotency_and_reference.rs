//! Create the idempotency-dedup table and the reference/control tables
//! (chart of accounts, fiscal periods, currency-scale registry, posting
//! locks, payer state) in schema `bss`. `SQLite` mirrors the same shape with
//! the systematic transforms; every CHECK, PK, and unique index is kept.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_idempotency_dedup (
        tenant_id       uuid         NOT NULL,
        flow            text         NOT NULL,
        business_id     varchar(256) NOT NULL,
        payload_hash    varchar(128) NOT NULL,
        result_entry_id uuid,
        posted_at_utc   timestamptz,
        status          text         NOT NULL,
        retain_until    timestamptz,
        PRIMARY KEY (tenant_id, flow, business_id)
    )",
    "CREATE TABLE bss.ledger_tenant_account (
        account_id      uuid         NOT NULL,
        tenant_id       uuid         NOT NULL,
        legal_entity_id uuid         NOT NULL,
        account_class   text         NOT NULL,
        currency        varchar(16)  NOT NULL,
        revenue_stream  text,
        normal_side     text         NOT NULL CHECK (normal_side IN ('DR','CR')),
        may_go_negative boolean      NOT NULL DEFAULT false,
        lifecycle_state text         NOT NULL DEFAULT 'OPEN' CHECK (lifecycle_state IN ('OPEN','CLOSED')),
        PRIMARY KEY (account_id)
    )",
    "CREATE UNIQUE INDEX uq_tenant_account_coa
        ON bss.ledger_tenant_account (tenant_id, legal_entity_id, account_class, currency, COALESCE(revenue_stream,'-'))",
    "CREATE TABLE bss.ledger_fiscal_period (
        tenant_id       uuid        NOT NULL,
        legal_entity_id uuid        NOT NULL,
        period_id       varchar(6)  NOT NULL,
        fiscal_tz       varchar(64) NOT NULL,
        status          text        NOT NULL DEFAULT 'OPEN' CHECK (status IN ('OPEN','CLOSED')),
        PRIMARY KEY (tenant_id, legal_entity_id, period_id)
    )",
    "CREATE TABLE bss.ledger_currency_scale_registry (
        tenant_id          uuid        NOT NULL,
        currency           varchar(16) NOT NULL,
        minor_units        smallint    NOT NULL,
        plausible_max_major bigint     NOT NULL DEFAULT 1000000000000 CHECK (plausible_max_major > 0),
        source             text        NOT NULL,
        PRIMARY KEY (tenant_id, currency)
    )",
    "CREATE TABLE bss.ledger_tenant_posting_lock (
        tenant_id   uuid        NOT NULL,
        locked      boolean     NOT NULL DEFAULT false,
        reason_code text,
        set_by      uuid,
        set_at      timestamptz,
        cleared_by  uuid,
        cleared_at  timestamptz,
        PRIMARY KEY (tenant_id)
    )",
    "CREATE TABLE bss.ledger_payer_state (
        tenant_id                uuid    NOT NULL,
        payer_tenant_id          uuid    NOT NULL,
        lifecycle_state          text    NOT NULL DEFAULT 'OPEN' CHECK (lifecycle_state IN ('OPEN','CLOSED')),
        closed_with_open_balance boolean NOT NULL DEFAULT false,
        approved_by              uuid,
        changed_at               timestamptz,
        PRIMARY KEY (tenant_id, payer_tenant_id)
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_payer_state",
    "DROP TABLE IF EXISTS bss.ledger_tenant_posting_lock",
    "DROP TABLE IF EXISTS bss.ledger_currency_scale_registry",
    "DROP TABLE IF EXISTS bss.ledger_fiscal_period",
    "DROP TABLE IF EXISTS bss.ledger_tenant_account",
    "DROP TABLE IF EXISTS bss.ledger_idempotency_dedup",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PKs + unique index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_idempotency_dedup (
        tenant_id       text         NOT NULL,
        flow            text         NOT NULL,
        business_id     varchar(256) NOT NULL,
        payload_hash    varchar(128) NOT NULL,
        result_entry_id text,
        posted_at_utc   text,
        status          text         NOT NULL,
        retain_until    text,
        PRIMARY KEY (tenant_id, flow, business_id)
    )",
    "CREATE TABLE ledger_tenant_account (
        account_id      text         NOT NULL,
        tenant_id       text         NOT NULL,
        legal_entity_id text         NOT NULL,
        account_class   text         NOT NULL,
        currency        varchar(16)  NOT NULL,
        revenue_stream  text,
        normal_side     text         NOT NULL CHECK (normal_side IN ('DR','CR')),
        may_go_negative boolean      NOT NULL DEFAULT false,
        lifecycle_state text         NOT NULL DEFAULT 'OPEN' CHECK (lifecycle_state IN ('OPEN','CLOSED')),
        PRIMARY KEY (account_id)
    )",
    "CREATE UNIQUE INDEX uq_tenant_account_coa
        ON ledger_tenant_account (tenant_id, legal_entity_id, account_class, currency, COALESCE(revenue_stream,'-'))",
    "CREATE TABLE ledger_fiscal_period (
        tenant_id       text        NOT NULL,
        legal_entity_id text        NOT NULL,
        period_id       varchar(6)  NOT NULL,
        fiscal_tz       varchar(64) NOT NULL,
        status          text        NOT NULL DEFAULT 'OPEN' CHECK (status IN ('OPEN','CLOSED')),
        PRIMARY KEY (tenant_id, legal_entity_id, period_id)
    )",
    "CREATE TABLE ledger_currency_scale_registry (
        tenant_id          text        NOT NULL,
        currency           varchar(16) NOT NULL,
        minor_units        smallint    NOT NULL,
        plausible_max_major bigint     NOT NULL DEFAULT 1000000000000 CHECK (plausible_max_major > 0),
        source             text        NOT NULL,
        PRIMARY KEY (tenant_id, currency)
    )",
    "CREATE TABLE ledger_tenant_posting_lock (
        tenant_id   text        NOT NULL,
        locked      boolean     NOT NULL DEFAULT false,
        reason_code text,
        set_by      text,
        set_at      text,
        cleared_by  text,
        cleared_at  text,
        PRIMARY KEY (tenant_id)
    )",
    "CREATE TABLE ledger_payer_state (
        tenant_id                text    NOT NULL,
        payer_tenant_id          text    NOT NULL,
        lifecycle_state          text    NOT NULL DEFAULT 'OPEN' CHECK (lifecycle_state IN ('OPEN','CLOSED')),
        closed_with_open_balance boolean NOT NULL DEFAULT false,
        approved_by              text,
        changed_at               text,
        PRIMARY KEY (tenant_id, payer_tenant_id)
    )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_payer_state",
    "DROP TABLE IF EXISTS ledger_tenant_posting_lock",
    "DROP TABLE IF EXISTS ledger_currency_scale_registry",
    "DROP TABLE IF EXISTS ledger_fiscal_period",
    "DROP TABLE IF EXISTS ledger_tenant_account",
    "DROP TABLE IF EXISTS ledger_idempotency_dedup",
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
