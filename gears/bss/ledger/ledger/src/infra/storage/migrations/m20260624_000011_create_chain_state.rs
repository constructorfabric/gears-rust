//! Add the tamper-evidence chain pointer columns to `bss.ledger_journal_entry`
//! (`prev_entry_id` / `prev_period_id`) and create the per-tenant
//! `bss.chain_state` tip table (last sealed `row_hash` / entry id / period /
//! sequence). The chain-pointer index carries `period_id` so the tip can be
//! resolved by `(tenant_id, entry_id)` without touching the heap on Postgres;
//! `SQLite` (non-production test backend) omits the `INCLUDE` payload (it is
//! unsupported there) and `bytea` becomes `blob`. Every column/PK is preserved
//! on both backends.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_journal_entry ADD COLUMN prev_entry_id uuid",
    "ALTER TABLE bss.ledger_journal_entry ADD COLUMN prev_period_id varchar(6)",
    "CREATE INDEX idx_journal_entry_chain_ptr
        ON bss.ledger_journal_entry (tenant_id, entry_id) INCLUDE (period_id)",
    "CREATE TABLE bss.chain_state (
        tenant_id      uuid        NOT NULL PRIMARY KEY,
        last_row_hash  bytea       NOT NULL,
        last_entry_id  uuid        NOT NULL,
        last_period_id varchar(6)  NOT NULL,
        last_seq       bigint      NOT NULL
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.chain_state",
    "DROP INDEX IF EXISTS bss.idx_journal_entry_chain_ptr",
    "ALTER TABLE bss.ledger_journal_entry DROP COLUMN prev_period_id",
    "ALTER TABLE bss.ledger_journal_entry DROP COLUMN prev_entry_id",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------
//
// Systematic transforms from the Postgres variant:
// * schema prefix `bss.` dropped (single namespace);
// * `uuid` → `text`; `bytea` → `blob`; `bigint` → `integer`;
// * `INCLUDE (...)` is unsupported, so the chain-pointer index is a plain
//   `(tenant_id, entry_id)` index (the covered `period_id` is read from the
//   heap on SQLite — acceptable for the test backend).
// Every column and PK is preserved.

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE ledger_journal_entry ADD COLUMN prev_entry_id text",
    "ALTER TABLE ledger_journal_entry ADD COLUMN prev_period_id varchar(6)",
    "CREATE INDEX idx_journal_entry_chain_ptr ON ledger_journal_entry (tenant_id, entry_id)",
    "CREATE TABLE chain_state (
        tenant_id      text        NOT NULL PRIMARY KEY,
        last_row_hash  blob        NOT NULL,
        last_entry_id  text        NOT NULL,
        last_period_id varchar(6)  NOT NULL,
        last_seq       integer     NOT NULL
    )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS chain_state",
    "DROP INDEX IF EXISTS idx_journal_entry_chain_ptr",
    "ALTER TABLE ledger_journal_entry DROP COLUMN prev_period_id",
    "ALTER TABLE ledger_journal_entry DROP COLUMN prev_entry_id",
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
