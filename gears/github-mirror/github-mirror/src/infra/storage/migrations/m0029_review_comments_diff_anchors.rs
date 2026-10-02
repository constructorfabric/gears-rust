use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// One `ALTER TABLE` per column, in the order the model declares them.
const COLUMNS: [&str; 2] = ["position", "original_position"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        for column in COLUMNS {
            // Nullable: rows mirrored before these columns existed keep working,
            // and GitHub itself omits `position` once a comment's line is
            // outdated by a later push.
            let sql = match backend {
                sea_orm::DatabaseBackend::Postgres => format!(
                    "ALTER TABLE gm_review_comments ADD COLUMN IF NOT EXISTS {column} BIGINT;"
                ),
                sea_orm::DatabaseBackend::MySql => {
                    format!("ALTER TABLE gm_review_comments ADD COLUMN {column} BIGINT;")
                }
                sea_orm::DatabaseBackend::Sqlite => {
                    format!("ALTER TABLE gm_review_comments ADD COLUMN {column} INTEGER;")
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
                "ALTER TABLE gm_review_comments DROP COLUMN {column};"
            ))
            .await?;
        }

        Ok(())
    }
}
