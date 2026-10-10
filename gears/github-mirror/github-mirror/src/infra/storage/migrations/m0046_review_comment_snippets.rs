use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

use super::support::drop_column;

/// Adds `snippet_before` and `snippet_after` to `gm_review_comments`: the code
/// lines around the commented line, cut from `diff_hunk` when a sync asks for
/// them (`inline_comment_snippets` in the collection scope).
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => &[
                "ALTER TABLE gm_review_comments ADD COLUMN IF NOT EXISTS snippet_before TEXT;",
                "ALTER TABLE gm_review_comments ADD COLUMN IF NOT EXISTS snippet_after TEXT;",
            ],
            sea_orm::DatabaseBackend::MySql | sea_orm::DatabaseBackend::Sqlite => &[
                "ALTER TABLE gm_review_comments ADD COLUMN snippet_before TEXT;",
                "ALTER TABLE gm_review_comments ADD COLUMN snippet_after TEXT;",
            ],
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column(manager, "gm_review_comments", "snippet_after").await?;
        drop_column(manager, "gm_review_comments", "snippet_before").await
    }
}
