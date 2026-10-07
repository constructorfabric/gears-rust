use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Index for the upload reaper scan (`status IN (pending, uploaded)`,
/// `cleanup_status IS NULL`, `deleted_at IS NULL`, `updated_at < cutoff`).
/// `cleanup_status` and `deleted_at` are index columns, so rows of deleted
/// chats or attachments that stay `pending`/`uploaded` forever (no hard
/// purge) are skipped in the index, not read and filtered on every scan.
/// Not a partial index: the query binds the status values as parameters, and
/// `SQLite` uses a partial index only when the query repeats its `WHERE`
/// literally.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_attachments_stale_upload \
                 ON attachments (status, cleanup_status, deleted_at, updated_at)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_attachments_stale_upload")
            .await?;
        Ok(())
    }
}
