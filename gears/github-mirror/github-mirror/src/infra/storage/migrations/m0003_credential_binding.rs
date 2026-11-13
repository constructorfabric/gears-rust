use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES: &str = include_str!("m0003_credential_binding/postgres.sql");
const MYSQL: &str = include_str!("m0003_credential_binding/mysql.sql");
const SQLITE: &str = include_str!("m0003_credential_binding/sqlite.sql");

const DOWN_POSTGRES: &str = include_str!("m0003_credential_binding/down_postgres.sql");
const DOWN_MYSQL: &str = include_str!("m0003_credential_binding/down_mysql.sql");
const DOWN_SQLITE: &str = include_str!("m0003_credential_binding/down_sqlite.sql");

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::run_schema(manager, POSTGRES, MYSQL, SQLITE).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::run_schema(manager, DOWN_POSTGRES, DOWN_MYSQL, DOWN_SQLITE).await
    }
}
