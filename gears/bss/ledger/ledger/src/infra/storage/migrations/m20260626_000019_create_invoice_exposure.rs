//! Create the Slice 3 credit-note **headroom** counter table in schema `bss`:
//! `ledger_invoice_exposure` — the per-invoice guarded counter that bounds total
//! credit-note exposure (design §4.7 / §7), keyed by `(tenant_id, invoice_id)`.
//!
//! `original_total_minor` is **seeded at first touch** = the invoice's posted AR
//! (incl. tax), via `INSERT … ON CONFLICT DO UPDATE` (the Slice 1 first-touch
//! upsert pattern, so concurrent creators serialize with no duplicate-key);
//! `debit_note_total_minor` is raised by S4 debit notes; `credit_note_total_minor`
//! is raised by credit notes. The
//! `chk_ledger_invoice_exposure_headroom` CHECK —
//! `credit_note_total_minor <= original_total_minor + debit_note_total_minor`
//! (AC #24) — is the **authoritative** in-transaction headroom guard: the
//! `CreditNoteHandler` (Phase 1) bumps `credit_note_total_minor` by an in-place
//! delta under the lock order and the CHECK is evaluated post-delta; an over-cap
//! note aborts the txn (surfaced as `CREDIT_NOTE_EXCEEDS_HEADROOM`, 400 — the
//! platform `CanonicalError` ladder has no 422). The
//! nonneg CHECKs are the residual defense-in-depth.
//!
//! **Lock order.** `invoice_exposure` is acquired AFTER the shared balance caches
//! and the recognition tables, BEFORE `payment_allocation_refund`, per the single
//! global order in design §4.7:
//! `payment_settlement → account_balance → ar_invoice_balance → ar_payer_balance
//! → unallocated_balance → reusable_credit_subbalance → tax_subbalance →
//! recognition_schedule → recognition_segment → invoice_exposure →
//! payment_allocation_refund`. Like the recognition tables, this is a procedural
//! counter grain (a single-row in-place counter delta in the handler), NOT a
//! `BalanceProjector` balance grain, so it carries no `GrainTable` rank (the
//! projector ranks stay balance-only; see `grain_lock_order_ranks_are_pinned`).
//!
//! All CHECKs are created in final form up-front (Foundation §7.2). `SQLite`
//! mirrors the same shape with the systematic transforms (`uuid`→`text`); the
//! CHECKs + PK are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.ledger_invoice_exposure (
        tenant_id               uuid          NOT NULL,
        invoice_id              varchar(128)  NOT NULL,
        currency                varchar(16)   NOT NULL,
        original_total_minor    bigint        NOT NULL,
        debit_note_total_minor  bigint        NOT NULL DEFAULT 0,
        credit_note_total_minor bigint        NOT NULL DEFAULT 0,
        version                 bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, invoice_id),
        CONSTRAINT chk_ledger_invoice_exposure_headroom
            CHECK (credit_note_total_minor <= original_total_minor + debit_note_total_minor),
        CONSTRAINT chk_ledger_invoice_exposure_original_nonneg
            CHECK (original_total_minor >= 0),
        CONSTRAINT chk_ledger_invoice_exposure_debit_nonneg
            CHECK (debit_note_total_minor >= 0),
        CONSTRAINT chk_ledger_invoice_exposure_credit_nonneg
            CHECK (credit_note_total_minor >= 0)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_invoice_exposure"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`; all CHECKs
// + PK preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE ledger_invoice_exposure (
        tenant_id               text          NOT NULL,
        invoice_id              varchar(128)  NOT NULL,
        currency                varchar(16)   NOT NULL,
        original_total_minor    bigint        NOT NULL,
        debit_note_total_minor  bigint        NOT NULL DEFAULT 0,
        credit_note_total_minor bigint        NOT NULL DEFAULT 0,
        version                 bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, invoice_id),
        CONSTRAINT chk_ledger_invoice_exposure_headroom
            CHECK (credit_note_total_minor <= original_total_minor + debit_note_total_minor),
        CONSTRAINT chk_ledger_invoice_exposure_original_nonneg
            CHECK (original_total_minor >= 0),
        CONSTRAINT chk_ledger_invoice_exposure_debit_nonneg
            CHECK (debit_note_total_minor >= 0),
        CONSTRAINT chk_ledger_invoice_exposure_credit_nonneg
            CHECK (credit_note_total_minor >= 0)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_invoice_exposure"];

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
