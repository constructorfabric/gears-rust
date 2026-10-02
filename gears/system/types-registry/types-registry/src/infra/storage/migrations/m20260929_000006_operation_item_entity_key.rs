//! Rename `operation_item.gts_id` to `entity_key`: a deletion item keeps an
//! unresolved Registry Reference rather than a fabricated identifier.
//!
//! Down is fail-closed while any deletion operation exists: those carry keys or
//! Registry-Reference fingerprints the previous code would misread or `409` on replay.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_STATEMENTS: &[&str] =
    &["ALTER TABLE types_registry__operation_item RENAME COLUMN gts_id TO entity_key"];

const DOWN_STATEMENTS: &[&str] =
    &["ALTER TABLE types_registry__operation_item RENAME COLUMN entity_key TO gts_id"];

const DELETION_PROBE: &str =
    "SELECT 1 AS present FROM types_registry__operation WHERE kind = 2 LIMIT 1";

fn statements(
    backend: sea_orm::DatabaseBackend,
    statements: &'static [&'static str],
) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres
        | sea_orm::DatabaseBackend::Sqlite
        | sea_orm::DatabaseBackend::MySql => Ok(statements),
        other => Err(DbErr::Migration(format!(
            "types-registry migrations support Postgres, SQLite and MySQL only; \
             got unsupported database backend {other:?}"
        ))),
    }
}

#[expect(
    elided_lifetimes_in_paths,
    reason = "`MigrationTrait` elides `SchemaManager`'s lifetime, so the impl must too"
)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        for sql in statements(backend, UP_STATEMENTS)? {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let down = statements(backend, DOWN_STATEMENTS)?;
        if conn
            .query_one_raw(Statement::from_string(backend, DELETION_PROBE.to_owned()))
            .await?
            .is_some()
        {
            return Err(DbErr::Migration(
                "cannot roll back the entity_key rename while deletion operations exist: \
                 the previous code would misread their keys or refuse their replay"
                    .to_owned(),
            ));
        }
        for sql in down {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
