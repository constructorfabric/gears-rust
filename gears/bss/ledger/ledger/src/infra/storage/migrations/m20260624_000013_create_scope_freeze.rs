//! Create the `bss.scope_freeze` table: a per-tenant tamper-freeze switch the
//! integrity verifier sets to STOP further posting into a scope whose chain
//! failed verification. A row is ACTIVE while `cleared_at IS NULL`; `period_id`
//! is `'ALL'` for a tenant-wide freeze or a concrete `varchar(6)` period to
//! freeze just that period. The composite PK `(tenant_id, scope, period_id)`
//! lets a tenant carry both an `'ALL'` freeze and per-period freezes at once.
//! `SQLite` (non-production test backend) mirrors the shape with the systematic
//! transforms (`uuid`→`text`, `timestamptz`→`text`, no `bss.` prefix); the PK
//! and the `period_id DEFAULT 'ALL'` are preserved on both backends.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.scope_freeze (
        tenant_id   uuid         NOT NULL,
        scope       text         NOT NULL,
        period_id   varchar(6)   NOT NULL DEFAULT 'ALL',
        reason      text         NOT NULL,
        frozen_at   timestamptz  NOT NULL DEFAULT now(),
        set_by      text         NOT NULL,
        cleared_by  text,
        cleared_at  timestamptz,
        PRIMARY KEY (tenant_id, scope, period_id)
    )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.scope_freeze"];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema for fast tests / dev.
// ---------------------------------------------------------------------------
//
// Systematic transforms from the Postgres variant:
// * schema prefix `bss.` dropped (single namespace);
// * `uuid` → `text`;
// * `timestamptz NOT NULL DEFAULT now()` → `text NOT NULL DEFAULT (CURRENT_TIMESTAMP)`
//   (mirroring how the P1 journal-tables migration maps `posted_at_utc`);
// * nullable `timestamptz` → nullable `text` (no default).
// The PK and the `period_id DEFAULT 'ALL'` are preserved.

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE scope_freeze (
        tenant_id   text         NOT NULL,
        scope       text         NOT NULL,
        period_id   varchar(6)   NOT NULL DEFAULT 'ALL',
        reason      text         NOT NULL,
        frozen_at   text         NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        set_by      text         NOT NULL,
        cleared_by  text,
        cleared_at  text,
        PRIMARY KEY (tenant_id, scope, period_id)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS scope_freeze"];

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
