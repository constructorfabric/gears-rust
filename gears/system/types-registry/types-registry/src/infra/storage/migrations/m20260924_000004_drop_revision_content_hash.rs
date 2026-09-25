//! Drop the authored-content digest from both revision tables.
//!
//! `content_hash` was only a prefilter for the `unchanged` decision, which now
//! compares canonical bytes directly (ADR-0012), and it is no longer part of the
//! read contract. No index or constraint names the column, so a plain
//! `DROP COLUMN` works on every backend, `SQLite` included.
//!
//! Down is fail-closed: the digest is not computable in SQL, and the code that
//! reads the column expects eight real bytes on every row, so down refuses while
//! either revision table holds a row and changes nothing. On empty tables it
//! restores the column; the `DEFAULT` exists only because `SQLite` refuses a
//! `NOT NULL` `ADD COLUMN` without one, and no existing row can receive it.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// The tables whose rows would need a digest that down cannot compute.
const REVISION_TABLES: [&str; 2] = [
    "types_registry__type_schema_revision",
    "types_registry__instance_revision",
];

const UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__type_schema_revision DROP COLUMN content_hash",
    "ALTER TABLE types_registry__instance_revision DROP COLUMN content_hash",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__type_schema_revision
        ADD COLUMN content_hash bytea NOT NULL DEFAULT ''::bytea",
    "ALTER TABLE types_registry__instance_revision
        ADD COLUMN content_hash bytea NOT NULL DEFAULT ''::bytea",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__type_schema_revision
        ADD COLUMN content_hash BLOB NOT NULL DEFAULT X''",
    "ALTER TABLE types_registry__instance_revision
        ADD COLUMN content_hash BLOB NOT NULL DEFAULT X''",
];

const MYSQL_DOWN_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__type_schema_revision
        ADD COLUMN content_hash VARBINARY(64) NOT NULL DEFAULT ''",
    "ALTER TABLE types_registry__instance_revision
        ADD COLUMN content_hash VARBINARY(64) NOT NULL DEFAULT ''",
];

fn unsupported(other: sea_orm::DatabaseBackend) -> DbErr {
    DbErr::Migration(format!(
        "types-registry migrations support Postgres, SQLite and MySQL only; \
         got unsupported database backend {other:?}"
    ))
}

/// The statement list for `backend`, or a refusal naming it.
fn up_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres
        | sea_orm::DatabaseBackend::Sqlite
        | sea_orm::DatabaseBackend::MySql => Ok(UP_STATEMENTS),
        other => Err(unsupported(other)),
    }
}

fn down_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(PG_DOWN_STATEMENTS),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_DOWN_STATEMENTS),
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_DOWN_STATEMENTS),
        other => Err(unsupported(other)),
    }
}

#[cfg(test)]
#[path = "m20260924_000004_drop_revision_content_hash_tests.rs"]
mod drop_revision_content_hash_tests;

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
        let statements = down_statements(backend)?;
        // Before any schema change, so a refusal leaves both tables as they were.
        for table in REVISION_TABLES {
            let probe = format!("SELECT 1 AS present FROM {table} LIMIT 1");
            if conn
                .query_one_raw(Statement::from_string(backend, probe))
                .await?
                .is_some()
            {
                return Err(DbErr::Migration(format!(
                    "cannot roll back the content_hash drop: {table} has revisions, and \
                     their content_hash cannot be recomputed in SQL; restoring the column \
                     would give the previous code rows with no valid digest"
                )));
            }
        }
        for sql in statements {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
