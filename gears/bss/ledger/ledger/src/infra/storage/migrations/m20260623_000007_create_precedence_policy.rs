//! Create the effective-dated precedence-policy table in schema `bss`:
//! `tenant_precedence_policy` (per-tenant, append-only `version`ed rows that
//! pin an allocation precedence `strategy` from an `effective_from` instant
//! onward). The allocator resolves the row in effect at decision time (latest
//! `effective_from <= now`, highest `version` on a tie) and stamps its
//! `strategy#version` onto the allocation's audit trail; absent a row it falls
//! back to oldest-first. Tenant scoping is via `SecureORM` at query time (no PG
//! RLS policy block — same as the payment tables). `SQLite` mirrors the same
//! shape with the systematic transforms (`uuid`→`text`, `timestamptz`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_tenant_precedence_policy (
        tenant_id      uuid         NOT NULL,
        version        bigint       NOT NULL,
        effective_from timestamptz  NOT NULL,
        strategy       varchar(64)  NOT NULL,
        created_at_utc timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_precedence_policy_version_nonneg CHECK (version >= 0)
    )",
    "CREATE INDEX ix_precedence_policy_effective
        ON bss.ledger_tenant_precedence_policy (tenant_id, effective_from)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.ledger_tenant_precedence_policy"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; the CHECK + PK + index preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_tenant_precedence_policy (
        tenant_id      text         NOT NULL,
        version        bigint       NOT NULL,
        effective_from text         NOT NULL,
        strategy       varchar(64)  NOT NULL,
        created_at_utc text         NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_precedence_policy_version_nonneg CHECK (version >= 0)
    )",
    "CREATE INDEX ix_precedence_policy_effective
        ON ledger_tenant_precedence_policy (tenant_id, effective_from)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS ledger_tenant_precedence_policy"];

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
