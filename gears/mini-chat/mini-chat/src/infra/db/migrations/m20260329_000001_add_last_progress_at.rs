use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
            _ => {
                return Err(DbErr::Migration("MySQL not supported for mini-chat".into()));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(DOWN).await?;
        Ok(())
    }
}

const POSTGRES_UP: &str = r"
ALTER TABLE chat_turns ADD COLUMN last_progress_at TIMESTAMPTZ;

UPDATE chat_turns
   SET last_progress_at = started_at
 WHERE last_progress_at IS NULL
   AND state = 'running';

CREATE INDEX IF NOT EXISTS idx_chat_turns_orphan_scan
    ON chat_turns (last_progress_at)
    WHERE state = 'running' AND deleted_at IS NULL;
";

const SQLITE_UP: &str = r"
ALTER TABLE chat_turns ADD COLUMN last_progress_at TEXT;

UPDATE chat_turns
   SET last_progress_at = started_at
 WHERE last_progress_at IS NULL
   AND state = 'running';

CREATE INDEX IF NOT EXISTS idx_chat_turns_orphan_scan
    ON chat_turns (last_progress_at)
    WHERE state = 'running' AND deleted_at IS NULL;
";

const DOWN: &str = r"
DROP INDEX IF EXISTS idx_chat_turns_orphan_scan;
ALTER TABLE chat_turns DROP COLUMN last_progress_at;
";
