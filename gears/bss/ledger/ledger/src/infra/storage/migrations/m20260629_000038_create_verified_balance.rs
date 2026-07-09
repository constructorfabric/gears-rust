//! Create the cumulative tie-out baseline table in schema `bss`:
//! `ledger_verified_balance` (VHP-1843 incremental tie-out). One row per
//! `(tenant_id, grain, grain_key)` holding the cumulative VERIFIED balance
//! through the last closed period. Written atomically in the period-close
//! SERIALIZABLE txn right after the clean full tie-out passes (the closing
//! period's cache is the verified total); the daily job and the `AR_DERIVED`
//! recon check then verify `baseline + fold(open periods) == cache` instead of
//! folding all-time. `grain` is the cache discriminator (`account`, `ar_payer`,
//! `ar_invoice`, `ar_invoice_disputed`, `tax`, `unallocated`, `reusable_credit`)
//! and `grain_key` is the canonical per-instance key the tie-out fold produces,
//! so the verify compares like-for-like. Tenant scoping is via `SecureORM` at
//! query time (no PG RLS block — same as the other derived-cache tables).
//! `SQLite` mirrors the same shape with the systematic transforms
//! (`uuid`→`text`, `timestamptz`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_verified_balance (
        tenant_id              uuid         NOT NULL,
        grain                  varchar(32)  NOT NULL,
        grain_key              varchar(512) NOT NULL,
        verified_balance_minor bigint       NOT NULL,
        through_period         varchar(64)  NOT NULL,
        watermark_seq          bigint       NOT NULL,
        updated_at_utc         timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, grain, grain_key),
        CONSTRAINT chk_verified_balance_grain CHECK (grain IN (
            'account', 'ar_payer', 'ar_invoice', 'ar_invoice_disputed',
            'tax', 'unallocated', 'reusable_credit'
        )),
        CONSTRAINT chk_verified_balance_watermark_nonneg CHECK (watermark_seq >= 0)
    )",
    "CREATE INDEX ix_verified_balance_through
        ON bss.ledger_verified_balance (tenant_id, through_period)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_verified_balance"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; the CHECKs + PK + index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_verified_balance (
        tenant_id              text         NOT NULL,
        grain                  varchar(32)  NOT NULL,
        grain_key              varchar(512) NOT NULL,
        verified_balance_minor bigint       NOT NULL,
        through_period         varchar(64)  NOT NULL,
        watermark_seq          bigint       NOT NULL,
        updated_at_utc         text         NOT NULL,
        PRIMARY KEY (tenant_id, grain, grain_key),
        CONSTRAINT chk_verified_balance_grain CHECK (grain IN (
            'account', 'ar_payer', 'ar_invoice', 'ar_invoice_disputed',
            'tax', 'unallocated', 'reusable_credit'
        )),
        CONSTRAINT chk_verified_balance_watermark_nonneg CHECK (watermark_seq >= 0)
    )",
    "CREATE INDEX ix_verified_balance_through
        ON ledger_verified_balance (tenant_id, through_period)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_verified_balance"];

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
