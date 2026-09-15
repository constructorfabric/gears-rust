//! Persist trusted in-process gear attribution on registration operations.
//!
//! Nullable preserves existing P0 operations: the worker interprets `NULL` as
//! the legacy `types-registry` owner. The value is attribution only and never an
//! authorization input.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &["ALTER TABLE types_registry__operation
        ADD COLUMN IF NOT EXISTS owning_gear varchar(1024) NULL"];
const SQLITE_MYSQL_UP: &[&str] = &["ALTER TABLE types_registry__operation
        ADD COLUMN owning_gear varchar(1024) NULL"];
const DOWN: &[&str] = &["ALTER TABLE types_registry__operation DROP COLUMN owning_gear"];

fn up_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(PG_UP),
        sea_orm::DatabaseBackend::Sqlite | sea_orm::DatabaseBackend::MySql => Ok(SQLITE_MYSQL_UP),
        other => Err(DbErr::Migration(format!(
            "types-registry migrations support Postgres, SQLite and MySQL only; got {other:?}"
        ))),
    }
}

fn down_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres
        | sea_orm::DatabaseBackend::Sqlite
        | sea_orm::DatabaseBackend::MySql => Ok(DOWN),
        other => Err(DbErr::Migration(format!(
            "types-registry migrations support Postgres, SQLite and MySQL only; got {other:?}"
        ))),
    }
}

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        for sql in up_statements(backend)? {
            manager
                .get_connection()
                .execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        for sql in down_statements(backend)? {
            manager
                .get_connection()
                .execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_backend_adds_nullable_owning_gear() {
        for backend in [
            sea_orm::DatabaseBackend::Postgres,
            sea_orm::DatabaseBackend::Sqlite,
            sea_orm::DatabaseBackend::MySql,
        ] {
            let sql = up_statements(backend).expect("supported").join("\n");
            assert!(sql.contains("types_registry__operation"));
            assert!(sql.contains("owning_gear"));
            assert!(sql.contains("NULL"));
        }
    }
}
