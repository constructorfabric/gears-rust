//! Create the typed entry-annotation overlay: `bss.entry_annotation` (Slice 6
//! Phase 2 Group 2B, Variant C remodel). Each row is the CURRENT controlled
//! non-financial annotation (`description`) on one journal entry / line. Unlike
//! the journal + secured-audit chains, this table is MUTABLE current-state: a
//! re-annotation UPSERTs the row in place. The append-only HISTORY of every
//! change lives in the secured-audit chain (`metadata-change` records), so this
//! table carries no append-only trigger and no before/after column. `SQLite`
//! (non-production test backend) mirrors the shape with the systematic
//! transforms (`uuid`→`text`, `timestamptz`→`text`, no `bss.` prefix).
//! PK `(tenant_id, target_id, target_kind)` — one current annotation per target.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.entry_annotation (
        tenant_id        uuid        NOT NULL,
        target_id        uuid        NOT NULL,
        target_kind      text        NOT NULL CHECK (target_kind IN ('ENTRY','LINE')),
        target_period_id varchar(6)  NOT NULL,
        description      text,
        actor_ref        text        NOT NULL,
        updated_at       timestamptz NOT NULL DEFAULT now(),
        PRIMARY KEY (tenant_id, target_id, target_kind)
    )",
    // No separate (tenant_id, target_id) index: the PK btree
    // (tenant_id, target_id, target_kind) already serves that prefix, and every
    // read/upsert filters on the full PK.
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.entry_annotation"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE entry_annotation (
        tenant_id        text       NOT NULL,
        target_id        text       NOT NULL,
        target_kind      text       NOT NULL CHECK (target_kind IN ('ENTRY','LINE')),
        target_period_id varchar(6) NOT NULL,
        description      text,
        actor_ref        text       NOT NULL,
        updated_at       text       NOT NULL DEFAULT (CURRENT_TIMESTAMP),
        PRIMARY KEY (tenant_id, target_id, target_kind)
    )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS entry_annotation"];

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
