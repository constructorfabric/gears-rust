use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Adds `assets_json` — the release's assets serialized as JSON — to
/// `gm_releases`, so download names/URLs/sizes survive mirroring.
///
/// Named with a `z_` prefix because the migration runner applies migrations
/// in **name** order and this one alters a table created by `releases_010`.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                "ALTER TABLE gm_releases ADD COLUMN IF NOT EXISTS assets_json TEXT;"
            }
            sea_orm::DatabaseBackend::MySql | sea_orm::DatabaseBackend::Sqlite => {
                "ALTER TABLE gm_releases ADD COLUMN assets_json TEXT;"
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
        // The table may already be gone: `releases_010`'s own `down()` runs
        // in the same reverse pass and name-ordering puts it after this one.
        if let Err(e) = conn
            .execute_unprepared("ALTER TABLE gm_releases DROP COLUMN assets_json;")
            .await
        {
            let message = e.to_string();
            if !message.contains("no such table") {
                return Err(e);
            }
        }
        Ok(())
    }
}
