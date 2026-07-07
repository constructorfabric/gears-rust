//! Create the effective-dated posting-policy table in schema `bss`:
//! `tenant_posting_policy` (per-tenant, append-only `version`ed rows that pin the
//! two invoice-posting policies the design names tenant-overridable, VHP-1853):
//! the missing-mapping mode (`SUSPENSE` route-to-suspense default vs `HARD_BLOCK`)
//! and the AR-aging bucket thresholds (CSV upper-bounds, e.g. `30,60,90`). The
//! orchestrator / aging read resolves the row in effect at decision time (latest
//! `effective_from <= now`, highest `version` on a tie); absent a row the gear's
//! built-in defaults apply (`SUSPENSE` + `30,60,90`, the prior hardcoded
//! behaviour). Tenant scoping is via `SecureORM` at query time (no PG RLS policy
//! block — same as the other policy tables). `SQLite` mirrors the same shape with
//! the systematic transforms (`uuid`→`text`, `timestamptz`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_tenant_posting_policy (
        tenant_id            uuid         NOT NULL,
        version              bigint       NOT NULL,
        effective_from       timestamptz  NOT NULL,
        missing_mapping_mode varchar(16)  NOT NULL,
        ar_aging_thresholds  varchar(128) NOT NULL,
        created_at_utc       timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_posting_policy_version_nonneg CHECK (version >= 0),
        CONSTRAINT chk_posting_policy_missing_mapping_mode
            CHECK (missing_mapping_mode IN ('SUSPENSE', 'HARD_BLOCK'))
    )",
    "CREATE INDEX ix_posting_policy_effective
        ON bss.ledger_tenant_posting_policy (tenant_id, effective_from)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_tenant_posting_policy"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; the CHECKs + PK + index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_tenant_posting_policy (
        tenant_id            text         NOT NULL,
        version              bigint       NOT NULL,
        effective_from       text         NOT NULL,
        missing_mapping_mode varchar(16)  NOT NULL,
        ar_aging_thresholds  varchar(128) NOT NULL,
        created_at_utc       text         NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_posting_policy_version_nonneg CHECK (version >= 0),
        CONSTRAINT chk_posting_policy_missing_mapping_mode
            CHECK (missing_mapping_mode IN ('SUSPENSE', 'HARD_BLOCK'))
    )",
    "CREATE INDEX ix_posting_policy_effective
        ON ledger_tenant_posting_policy (tenant_id, effective_from)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_tenant_posting_policy"];

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
