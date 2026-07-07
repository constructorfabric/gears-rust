//! Create the `bss` Postgres schema. Postgres-only: `SQLite` has a single
//! namespace, so the `SQLite` branch is a no-op and later table migrations
//! create unqualified tables that resolve into `bss` via `search_path`.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        match backend {
            sea_orm::DatabaseBackend::Postgres => {
                conn.execute(Statement::from_string(
                    backend,
                    "CREATE SCHEMA IF NOT EXISTS bss".to_owned(),
                ))
                .await?;
            }
            sea_orm::DatabaseBackend::Sqlite => { /* single namespace; no-op */ }
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        if backend == sea_orm::DatabaseBackend::Postgres {
            conn.execute(Statement::from_string(
                backend,
                "DROP SCHEMA IF EXISTS bss CASCADE".to_owned(),
            ))
            .await?;
        }
        Ok(())
    }
}
