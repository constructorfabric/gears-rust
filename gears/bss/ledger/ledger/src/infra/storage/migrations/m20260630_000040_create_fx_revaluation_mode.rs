//! Create the effective-dated per-tenant FX **revaluation-mode** table in schema
//! `bss`: `ledger_tenant_fx_revaluation_mode` (per-tenant, append-only `version`ed
//! rows, VHP-1986) pinning whether BSS runs the period-end unrealized revaluation
//! for a tenant (`MODE_B` = BSS is the ledger of record) or defers to the tenant's
//! ERP (`MODE_A`, the fail-safe default — BSS must not double-count). The
//! revaluation job / period-close resolve the row in effect at decision time
//! (latest `effective_from <= now`, highest `version` on a tie); absent a row the
//! gear default (`MODE_A`) applies. Tenant scoping is via `SecureORM` at query
//! time (no PG RLS policy block — same as the other policy tables). `SQLite`
//! mirrors the same shape (`uuid`→`text`, `timestamptz`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_tenant_fx_revaluation_mode (
        tenant_id         uuid         NOT NULL,
        version           bigint       NOT NULL,
        effective_from    timestamptz  NOT NULL,
        revaluation_mode  varchar(16)  NOT NULL,
        created_at_utc    timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_fx_revaluation_mode_version_nonneg CHECK (version >= 0),
        CONSTRAINT chk_fx_revaluation_mode_value
            CHECK (revaluation_mode IN ('MODE_A', 'MODE_B'))
    )",
    "CREATE INDEX ix_fx_revaluation_mode_effective
        ON bss.ledger_tenant_fx_revaluation_mode (tenant_id, effective_from)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_tenant_fx_revaluation_mode"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; the CHECKs + PK + index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_tenant_fx_revaluation_mode (
        tenant_id         text         NOT NULL,
        version           bigint       NOT NULL,
        effective_from    text         NOT NULL,
        revaluation_mode  varchar(16)  NOT NULL,
        created_at_utc    text         NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_fx_revaluation_mode_version_nonneg CHECK (version >= 0),
        CONSTRAINT chk_fx_revaluation_mode_value
            CHECK (revaluation_mode IN ('MODE_A', 'MODE_B'))
    )",
    "CREATE INDEX ix_fx_revaluation_mode_effective
        ON ledger_tenant_fx_revaluation_mode (tenant_id, effective_from)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_tenant_fx_revaluation_mode"];

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
