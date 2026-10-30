use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES: &str = include_str!("m0002_artifacts/postgres.sql");
const MYSQL: &str = include_str!("m0002_artifacts/mysql.sql");
const SQLITE: &str = include_str!("m0002_artifacts/sqlite.sql");

const TABLES: [&str; 1] = ["gm_artifacts"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = match manager.get_database_backend() {
            DatabaseBackend::Postgres => POSTGRES,
            DatabaseBackend::MySql => MYSQL,
            DatabaseBackend::Sqlite => SQLITE,
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };
        let conn = manager.get_connection();
        for statement in schema
            .split(';')
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
        {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for table in TABLES {
            conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
                .await?;
        }
        Ok(())
    }
}
