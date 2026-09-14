use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        // Nullable: rows mirrored before this column existed keep working,
        // and reads fall back to the repository's canonical GitHub URL.
        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                "ALTER TABLE gm_repositories ADD COLUMN IF NOT EXISTS clone_url VARCHAR(1024);"
            }
            sea_orm::DatabaseBackend::MySql => {
                "ALTER TABLE gm_repositories ADD COLUMN clone_url VARCHAR(1024);"
            }
            sea_orm::DatabaseBackend::Sqlite => {
                "ALTER TABLE gm_repositories ADD COLUMN clone_url TEXT;"
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
        let conn = manager.get_connection();
        conn.execute_unprepared("ALTER TABLE gm_repositories DROP COLUMN clone_url;")
            .await?;
        Ok(())
    }
}
