use sea_orm_migration::prelude::*;

use super::support::drop_column;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                "ALTER TABLE gm_sync_sessions ADD COLUMN IF NOT EXISTS telemetry_json TEXT;"
            }
            sea_orm::DatabaseBackend::MySql | sea_orm::DatabaseBackend::Sqlite => {
                "ALTER TABLE gm_sync_sessions ADD COLUMN telemetry_json TEXT;"
            }
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column(manager, "gm_sync_sessions", "telemetry_json").await
    }
}
