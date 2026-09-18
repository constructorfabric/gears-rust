use sea_orm_migration::prelude::*;

use super::support::drop_column;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Adds `updated_at` to `gm_sync_sessions`: the stamp every write refreshes,
/// the progress heartbeat included, so `ended_at` can stay empty until the
/// run really ends.
///
/// Named with a `z_` prefix because the migration runner applies migrations in
/// **name** order and this one alters the table created by `sync_sessions_029`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let (text_type, exists) = match backend {
            sea_orm::DatabaseBackend::Postgres => ("VARCHAR(64)", "IF NOT EXISTS "),
            sea_orm::DatabaseBackend::MySql => ("VARCHAR(64)", ""),
            sea_orm::DatabaseBackend::Sqlite => ("TEXT", ""),
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };

        let sql =
            format!("ALTER TABLE gm_sync_sessions ADD COLUMN {exists}updated_at {text_type};");
        conn.execute_unprepared(&sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column(manager, "gm_sync_sessions", "updated_at").await
    }
}
