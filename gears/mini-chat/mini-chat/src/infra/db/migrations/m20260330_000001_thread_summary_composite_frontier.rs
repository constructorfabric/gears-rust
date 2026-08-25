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
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_DOWN,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN,
            _ => {
                return Err(DbErr::Migration("MySQL not supported for mini-chat".into()));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}

const POSTGRES_UP: &str = r"
ALTER TABLE thread_summaries
    RENAME COLUMN summarized_up_to TO summarized_up_to_message_id;

ALTER TABLE thread_summaries
    ADD COLUMN summarized_up_to_created_at TIMESTAMPTZ;

-- Backfill from the actual message timestamp when the row exists.
UPDATE thread_summaries ts
    SET summarized_up_to_created_at = m.created_at
    FROM messages m
    WHERE m.id = ts.summarized_up_to_message_id
      AND m.chat_id = ts.chat_id
      AND ts.summarized_up_to_created_at IS NULL;

-- Fallback for orphaned rows (message deleted): use the summary's own updated_at.
UPDATE thread_summaries
    SET summarized_up_to_created_at = updated_at
    WHERE summarized_up_to_created_at IS NULL;

ALTER TABLE thread_summaries
    ALTER COLUMN summarized_up_to_created_at SET NOT NULL;
";

const POSTGRES_DOWN: &str = r"
ALTER TABLE thread_summaries
    DROP COLUMN summarized_up_to_created_at;

ALTER TABLE thread_summaries
    RENAME COLUMN summarized_up_to_message_id TO summarized_up_to;
";

// SQLite: ADD COLUMN with DEFAULT, then backfill from messages, then fix remaining.
// SQLite does not support UPDATE ... FROM, so use correlated subquery.
const SQLITE_UP: &str = r"
ALTER TABLE thread_summaries
    RENAME COLUMN summarized_up_to TO summarized_up_to_message_id;

ALTER TABLE thread_summaries
    ADD COLUMN summarized_up_to_created_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00Z';

-- Backfill from the actual message timestamp.
UPDATE thread_summaries
    SET summarized_up_to_created_at = (
        SELECT messages.created_at FROM messages
        WHERE messages.id = thread_summaries.summarized_up_to_message_id
          AND messages.chat_id = thread_summaries.chat_id
    )
    WHERE EXISTS (
        SELECT 1 FROM messages
        WHERE messages.id = thread_summaries.summarized_up_to_message_id
          AND messages.chat_id = thread_summaries.chat_id
    );

-- Fallback for orphaned rows: use the summary's own updated_at.
UPDATE thread_summaries
    SET summarized_up_to_created_at = updated_at
    WHERE summarized_up_to_created_at = '1970-01-01T00:00:00Z';
";

const SQLITE_DOWN: &str = r"
ALTER TABLE thread_summaries
    DROP COLUMN summarized_up_to_created_at;

ALTER TABLE thread_summaries
    RENAME COLUMN summarized_up_to_message_id TO summarized_up_to;
";
