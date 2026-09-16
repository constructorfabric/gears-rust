//! Persist the per-candidate ADR-0004 waiver request for the worker.
//! A separate migration upgrades existing installations. `DEFAULT false` is
//! valid for old items, since previous acceptance rejected effective `force`.
//!
//! The column uses `compat_forced` because `force` is reserved in `MySQL`.
//! It records the request; the revision column records the effective waiver.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &["ALTER TABLE types_registry__operation_item
        ADD COLUMN IF NOT EXISTS compat_forced boolean NOT NULL DEFAULT false"];

// SQLite has no `IF NOT EXISTS` for `ADD COLUMN`, and its boolean is an INTEGER
// with the same 0/1 CHECK every other lowered boolean in this schema carries.
const SQLITE_UP_STATEMENTS: &[&str] = &["ALTER TABLE types_registry__operation_item
        ADD COLUMN compat_forced INTEGER NOT NULL DEFAULT 0
        CHECK (compat_forced IN (0, 1))"];

const MYSQL_UP_STATEMENTS: &[&str] = &["ALTER TABLE types_registry__operation_item
        ADD COLUMN compat_forced TINYINT(1) NOT NULL DEFAULT 0,
        ADD CONSTRAINT ck_tr_operation_item_compat_forced_bool
            CHECK (compat_forced IN (0, 1))"];

const PG_SQLITE_DOWN_STATEMENTS: &[&str] =
    &["ALTER TABLE types_registry__operation_item DROP COLUMN compat_forced"];
const MYSQL_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__operation_item
        DROP CHECK ck_tr_operation_item_compat_forced_bool",
    "ALTER TABLE types_registry__operation_item DROP COLUMN compat_forced",
];

/// The statement list for `backend`, or a refusal naming it.
fn up_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(PG_UP_STATEMENTS),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP_STATEMENTS),
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_UP_STATEMENTS),
        other => Err(DbErr::Migration(format!(
            "types-registry migrations support Postgres, SQLite and MySQL only; \
             got unsupported database backend {other:?}"
        ))),
    }
}

fn down_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
            Ok(PG_SQLITE_DOWN_STATEMENTS)
        }
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_DOWN_STATEMENTS),
        other => Err(DbErr::Migration(format!(
            "types-registry migrations support Postgres, SQLite and MySQL only; \
             got unsupported database backend {other:?}"
        ))),
    }
}

#[cfg(test)]
#[path = "m20260908_000003_operation_item_compat_forced_tests.rs"]
mod operation_item_compat_forced_tests;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        for sql in up_statements(backend)? {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        for sql in down_statements(backend)? {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
