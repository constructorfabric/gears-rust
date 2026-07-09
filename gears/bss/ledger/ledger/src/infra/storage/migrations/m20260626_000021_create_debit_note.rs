//! Create the Slice 3 `ledger_debit_note` record table in schema `bss` — the
//! record linking a posted debit note (an additional charge) to its originating
//! posted invoice and its recognized/deferred split (design §7), keyed by
//! `(tenant_id, debit_note_id)`.
//!
//! `debit_note_id` is the **business** id (mirrors `recognition_schedule`'s
//! `schedule_id` — a `varchar(128)`, NOT a `uuid` column — so it lines up with
//! the `SecureORM` `resource_col`). The PK doubles as the design's
//! `UNIQUE (tenant_id, debit_note_id)` (§7). `amount_minor` is incl-tax;
//! `recognized_part_minor` + `deferred_part_minor` are the ex-tax split parts —
//! as with `credit_note`, there is **deliberately no** `recognized + deferred ==
//! amount` CHECK (parts are ex-tax, `amount_minor` is incl-tax). A debit note
//! **raises** the invoice's headroom (`invoice_exposure.debit_note_total_minor
//! += amount`, see `m20260626_000019`) under the lock order; only the nonneg
//! CHECKs are enforced on this record table.
//!
//! All CHECKs are created in final form up-front (Foundation §7.2). `SQLite`
//! mirrors the same shape with the systematic transforms (`uuid`→`text`,
//! `timestamptz`→`text`); the CHECKs + PK are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.ledger_debit_note (
        tenant_id             uuid          NOT NULL,
        debit_note_id         varchar(128)  NOT NULL,
        origin_invoice_id     varchar(128)  NOT NULL,
        currency              varchar(16)   NOT NULL,
        amount_minor          bigint        NOT NULL,
        recognized_part_minor bigint        NOT NULL DEFAULT 0,
        deferred_part_minor   bigint        NOT NULL DEFAULT 0,
        created_at_utc        timestamptz   NOT NULL,
        PRIMARY KEY (tenant_id, debit_note_id),
        CONSTRAINT chk_ledger_debit_note_amount_nonneg
            CHECK (amount_minor >= 0),
        CONSTRAINT chk_ledger_debit_note_recognized_nonneg
            CHECK (recognized_part_minor >= 0),
        CONSTRAINT chk_ledger_debit_note_deferred_nonneg
            CHECK (deferred_part_minor >= 0)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_debit_note"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PK preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE ledger_debit_note (
        tenant_id             text          NOT NULL,
        debit_note_id         varchar(128)  NOT NULL,
        origin_invoice_id     varchar(128)  NOT NULL,
        currency              varchar(16)   NOT NULL,
        amount_minor          bigint        NOT NULL,
        recognized_part_minor bigint        NOT NULL DEFAULT 0,
        deferred_part_minor   bigint        NOT NULL DEFAULT 0,
        created_at_utc        text          NOT NULL,
        PRIMARY KEY (tenant_id, debit_note_id),
        CONSTRAINT chk_ledger_debit_note_amount_nonneg
            CHECK (amount_minor >= 0),
        CONSTRAINT chk_ledger_debit_note_recognized_nonneg
            CHECK (recognized_part_minor >= 0),
        CONSTRAINT chk_ledger_debit_note_deferred_nonneg
            CHECK (deferred_part_minor >= 0)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_debit_note"];

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
