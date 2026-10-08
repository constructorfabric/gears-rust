use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// One `ALTER TABLE` per column, in the order the model declares them.
const COLUMNS: [&str; 3] = ["html_url", "head_ref", "base_ref"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        for column in COLUMNS {
            // Nullable: rows mirrored before these columns existed keep working.
            let sql = match backend {
                sea_orm::DatabaseBackend::Postgres => format!(
                    "ALTER TABLE gm_pull_requests ADD COLUMN IF NOT EXISTS {column} VARCHAR(1024);"
                ),
                sea_orm::DatabaseBackend::MySql => {
                    format!("ALTER TABLE gm_pull_requests ADD COLUMN {column} VARCHAR(1024);")
                }
                sea_orm::DatabaseBackend::Sqlite => {
                    format!("ALTER TABLE gm_pull_requests ADD COLUMN {column} TEXT;")
                }
                other => {
                    return Err(DbErr::Custom(format!(
                        "migration has no DDL for database backend {other:?}"
                    )));
                }
            };

            conn.execute_unprepared(&sql).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for column in COLUMNS {
            conn.execute_unprepared(&format!(
                "ALTER TABLE gm_pull_requests DROP COLUMN {column};"
            ))
            .await?;
        }

        Ok(())
    }
}
