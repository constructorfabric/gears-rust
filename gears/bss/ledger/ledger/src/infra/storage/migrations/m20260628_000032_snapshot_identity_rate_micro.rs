//! Slice 5 remediation (codex#1): widen the immutable snapshot identity
//! `uq_fx_rate_snapshot_lock` to include `rate_micro`.
//!
//! The lock key was `(tenant, base, quote, provider, as_of, fallback_order)` —
//! it did NOT include the rate value. The `ledger_fx_rate` "latest known" store
//! is keyed `(tenant, base, quote, provider)` and `upsert_rate` OVERWRITES
//! `rate_micro` + `as_of` on conflict, and the ingest endpoint is idempotent on
//! `(tenant, base, quote, provider, as_of)` — so a provider/manual CORRECTION of
//! the rate at the SAME `as_of` is a supported operation. Under the old key the
//! snapshot freeze (read-first dedupe + this UNIQUE) would REUSE the prior
//! snapshot row for the corrected rate: the posting path translates lines at the
//! corrected `rate_micro` but stamps a `rate_snapshot_ref` pointing at the STALE
//! rate, so the audit snapshot no longer reproduces the posted functional amount.
//!
//! Adding `rate_micro` to the identity makes a corrected rate freeze a NEW,
//! distinct snapshot (a fresh `rate_id`) while an identical re-lock (same rate)
//! still dedupes — the audit snapshot always reproduces the rate that was posted.
//! The repo's read-first lookup is widened with `rate_micro` in lockstep.
//!
//! Postgres-only: the constraint is a named table constraint there, droppable +
//! re-addable in place; `SQLite` (non-production test backend) declares it inline
//! in `CREATE TABLE` and cannot ALTER it without a full table rebuild (mirrors the
//! m027 / m029 / m031 split). The repo lookup change applies on both backends.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — drop + re-add the named UNIQUE with `rate_micro` appended.
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_fx_rate_snapshot
        DROP CONSTRAINT uq_fx_rate_snapshot_lock",
    "ALTER TABLE bss.ledger_fx_rate_snapshot
        ADD CONSTRAINT uq_fx_rate_snapshot_lock UNIQUE
        (tenant_id, base_currency, quote_currency, provider, as_of, fallback_order, rate_micro)",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE bss.ledger_fx_rate_snapshot
        DROP CONSTRAINT uq_fx_rate_snapshot_lock",
    "ALTER TABLE bss.ledger_fx_rate_snapshot
        ADD CONSTRAINT uq_fx_rate_snapshot_lock UNIQUE
        (tenant_id, base_currency, quote_currency, provider, as_of, fallback_order)",
];

// ---------------------------------------------------------------------------
// SQLite variant — inline CREATE-TABLE constraint, not alterable in place.
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[];

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
