use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

use super::support::drop_column;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => {
                "ALTER TABLE gm_sync_watermarks ADD COLUMN IF NOT EXISTS stage_etags TEXT;"
            }
            sea_orm::DatabaseBackend::MySql | sea_orm::DatabaseBackend::Sqlite => {
                "ALTER TABLE gm_sync_watermarks ADD COLUMN stage_etags TEXT;"
            }
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column(manager, "gm_sync_watermarks", "stage_etags").await
    }
}
