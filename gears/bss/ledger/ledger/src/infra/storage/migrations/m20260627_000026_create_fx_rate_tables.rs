//! Create the FX rate tables (Slice 5): the immutable per-lock
//! `ledger_fx_rate_snapshot` (frozen on each journal line via the
//! `rate_snapshot_ref` FK) and the mutable `ledger_fx_rate` "latest known"
//! local store that the `RateSyncJob` upserts and `RateSource` reads at lock
//! time (no synchronous provider call on the posting path). The snapshot table
//! is append-only — it reuses the P1 generic `bss.reject_mutation` guard on
//! UPDATE/DELETE so a provider revision can only INSERT a new row. `SQLite`
//! (non-production test backend) carries no triggers; immutability is
//! re-asserted in application code.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_fx_rate_snapshot (
        tenant_id        uuid         NOT NULL,
        rate_id          uuid         NOT NULL,
        base_currency    varchar(16)  NOT NULL,
        quote_currency   varchar(16)  NOT NULL,
        rate_micro       bigint       NOT NULL,
        as_of            timestamptz  NOT NULL,
        provider         varchar(128) NOT NULL,
        stale            boolean      NOT NULL DEFAULT false,
        fallback_order   integer      NOT NULL DEFAULT 0,
        triangulated_via varchar(128),
        PRIMARY KEY (tenant_id, rate_id),
        CONSTRAINT ck_fx_rate_snapshot_rate_positive CHECK (rate_micro > 0),
        CONSTRAINT uq_fx_rate_snapshot_lock UNIQUE
            (tenant_id, base_currency, quote_currency, provider, as_of, fallback_order)
    )",
    "CREATE INDEX idx_fx_rate_snapshot_pair
        ON bss.ledger_fx_rate_snapshot (tenant_id, base_currency, quote_currency, as_of)",
    // Append-only: a provider revision INSERTs a new row; UPDATE/DELETE reject.
    // Reuses the generic guard defined by the P1 journal-tables migration.
    "CREATE TRIGGER trg_fx_rate_snapshot_append_only
        BEFORE UPDATE OR DELETE ON bss.ledger_fx_rate_snapshot
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
    // Mutable "latest known" store: upserted by the RateSyncJob / ingest endpoint;
    // RateSource reads it at lock time (the per-lock freeze lands in the snapshot).
    "CREATE TABLE bss.ledger_fx_rate (
        tenant_id      uuid         NOT NULL,
        base_currency  varchar(16)  NOT NULL,
        quote_currency varchar(16)  NOT NULL,
        provider       varchar(128) NOT NULL,
        rate_micro     bigint       NOT NULL,
        as_of          timestamptz  NOT NULL,
        fallback_order integer      NOT NULL DEFAULT 0,
        updated_at     timestamptz  NOT NULL DEFAULT now(),
        PRIMARY KEY (tenant_id, base_currency, quote_currency, provider),
        CONSTRAINT ck_fx_rate_rate_positive CHECK (rate_micro > 0)
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_fx_rate",
    "DROP TRIGGER IF EXISTS trg_fx_rate_snapshot_append_only ON bss.ledger_fx_rate_snapshot",
    "DROP TABLE IF EXISTS bss.ledger_fx_rate_snapshot",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`, `boolean`→numeric; no triggers).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_fx_rate_snapshot (
        tenant_id        text         NOT NULL,
        rate_id          text         NOT NULL,
        base_currency    varchar(16)  NOT NULL,
        quote_currency   varchar(16)  NOT NULL,
        rate_micro       bigint       NOT NULL,
        as_of            text         NOT NULL,
        provider         varchar(128) NOT NULL,
        stale            boolean      NOT NULL DEFAULT 0,
        fallback_order   integer      NOT NULL DEFAULT 0,
        triangulated_via varchar(128),
        PRIMARY KEY (tenant_id, rate_id),
        CONSTRAINT ck_fx_rate_snapshot_rate_positive CHECK (rate_micro > 0),
        CONSTRAINT uq_fx_rate_snapshot_lock UNIQUE
            (tenant_id, base_currency, quote_currency, provider, as_of, fallback_order)
    )",
    "CREATE INDEX idx_fx_rate_snapshot_pair
        ON ledger_fx_rate_snapshot (tenant_id, base_currency, quote_currency, as_of)",
    "CREATE TABLE ledger_fx_rate (
        tenant_id      text         NOT NULL,
        base_currency  varchar(16)  NOT NULL,
        quote_currency varchar(16)  NOT NULL,
        provider       varchar(128) NOT NULL,
        rate_micro     bigint       NOT NULL,
        as_of          text         NOT NULL,
        fallback_order integer      NOT NULL DEFAULT 0,
        updated_at     text         NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        PRIMARY KEY (tenant_id, base_currency, quote_currency, provider),
        CONSTRAINT ck_fx_rate_rate_positive CHECK (rate_micro > 0)
    )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_fx_rate",
    "DROP TABLE IF EXISTS ledger_fx_rate_snapshot",
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
