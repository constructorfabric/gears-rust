//! Create the Slice 3 `ledger_credit_note` record table in schema `bss` — the
//! record linking a posted credit note to its originating posted invoice item,
//! its revenue stream, and the recognized/deferred split basis (design §7),
//! keyed by `(tenant_id, credit_note_id)`.
//!
//! `credit_note_id` is the **business** id (mirrors `recognition_schedule`'s
//! `schedule_id` — a `varchar(128)`, NOT a `uuid` column — so it lines up with
//! the `SecureORM` `resource_col`). The PK doubles as the design's
//! `UNIQUE (tenant_id, credit_note_id)` (§7). `amount_minor` is incl-tax;
//! `recognized_part_minor` + `deferred_part_minor` are the ex-tax split parts
//! recorded by the `RecognizedDeferredSplitter` (Phase 1, Group B) — there is
//! **deliberately no** `recognized + deferred == amount` CHECK, because the parts
//! are ex-tax while `amount_minor` is incl-tax (they do not sum to it). Only the
//! nonneg CHECKs are enforced here; the headroom cap lives on
//! `invoice_exposure` (see `m20260626_000019`), and the schedule-reduction guard
//! (`recognized_minor <= total_deferred_minor`) lives on `recognition_schedule`
//! (Slice 4) — both written by the credit-note handler under the lock order.
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

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.ledger_credit_note (
        tenant_id              uuid          NOT NULL,
        credit_note_id         varchar(128)  NOT NULL,
        origin_invoice_id      varchar(128)  NOT NULL,
        origin_invoice_item_ref varchar(128),
        revenue_stream         varchar(64)   NOT NULL,
        currency               varchar(16)   NOT NULL,
        amount_minor           bigint        NOT NULL,
        recognized_part_minor  bigint        NOT NULL DEFAULT 0,
        deferred_part_minor    bigint        NOT NULL DEFAULT 0,
        split_basis_ref        varchar(256),
        reason_code            varchar(64)   NOT NULL,
        created_at_utc         timestamptz   NOT NULL,
        PRIMARY KEY (tenant_id, credit_note_id),
        CONSTRAINT chk_ledger_credit_note_amount_nonneg
            CHECK (amount_minor >= 0),
        CONSTRAINT chk_ledger_credit_note_recognized_nonneg
            CHECK (recognized_part_minor >= 0),
        CONSTRAINT chk_ledger_credit_note_deferred_nonneg
            CHECK (deferred_part_minor >= 0)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_credit_note"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PK preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE ledger_credit_note (
        tenant_id              text          NOT NULL,
        credit_note_id         varchar(128)  NOT NULL,
        origin_invoice_id      varchar(128)  NOT NULL,
        origin_invoice_item_ref varchar(128),
        revenue_stream         varchar(64)   NOT NULL,
        currency               varchar(16)   NOT NULL,
        amount_minor           bigint        NOT NULL,
        recognized_part_minor  bigint        NOT NULL DEFAULT 0,
        deferred_part_minor    bigint        NOT NULL DEFAULT 0,
        split_basis_ref        varchar(256),
        reason_code            varchar(64)   NOT NULL,
        created_at_utc         text          NOT NULL,
        PRIMARY KEY (tenant_id, credit_note_id),
        CONSTRAINT chk_ledger_credit_note_amount_nonneg
            CHECK (amount_minor >= 0),
        CONSTRAINT chk_ledger_credit_note_recognized_nonneg
            CHECK (recognized_part_minor >= 0),
        CONSTRAINT chk_ledger_credit_note_deferred_nonneg
            CHECK (deferred_part_minor >= 0)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_credit_note"];

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
