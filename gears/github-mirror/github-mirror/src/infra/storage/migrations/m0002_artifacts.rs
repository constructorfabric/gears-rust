use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES: &str = include_str!("m0002_artifacts/postgres.sql");
const MYSQL: &str = include_str!("m0002_artifacts/mysql.sql");
const SQLITE: &str = include_str!("m0002_artifacts/sqlite.sql");

const TABLES: [&str; 1] = ["gm_artifacts"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::run_schema(manager, POSTGRES, MYSQL, SQLITE).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::drop_tables(manager, &TABLES).await
    }
}
