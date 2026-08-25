//! Migration `m0008` — drops the `integrity_check_runs` table
//! created by `m0003`.
//!
//! Coordination of the hierarchy-integrity reconciler is owned by
//! the AM-local lease (`am_leases`, created by `m0007`); the older
//! singleton-PK shape this table backed is no longer referenced
//! anywhere in the code, so the row count is inert weight.
//!
//! The `down` path recreates the table shape `m0003` produced —
//! used only if a deployment rolls back this migration and wants
//! to revive the legacy gate code. Row data is lost on drop.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "account-management migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite); add a dedicated MySQL migration set \
    before running against MySQL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if matches!(backend, sea_orm::DatabaseBackend::MySql) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS integrity_check_runs;")
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The down path is a best-effort rollback that recreates the
        // table shape `m0003` produced — used only if a deployment
        // rolls back the cutover commit AND wants to re-run the
        // legacy lock code (which would also need to be reverted).
        // It does NOT restore any rows; row data is lost on drop.
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let stmt = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                "CREATE TABLE integrity_check_runs ( \
                    id INTEGER PRIMARY KEY CHECK (id = 1), \
                    worker_id UUID NOT NULL, \
                    started_at TIMESTAMPTZ NOT NULL \
                );"
            }
            sea_orm::DatabaseBackend::Sqlite => {
                "CREATE TABLE integrity_check_runs ( \
                    id INTEGER PRIMARY KEY CHECK (id = 1), \
                    worker_id TEXT NOT NULL, \
                    started_at TIMESTAMP NOT NULL \
                );"
            }
            _ => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };
        conn.execute_unprepared(stmt).await?;
        Ok(())
    }
}
