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
            sea_orm::DatabaseBackend::Postgres => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_request_files (
    tenant_id UUID NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    filename VARCHAR(1024) NOT NULL,
    status VARCHAR(32) NOT NULL,
    additions BIGINT NOT NULL,
    deletions BIGINT NOT NULL,
    changes BIGINT NOT NULL,
    previous_filename VARCHAR(1024),
    sha VARCHAR(64),
    PRIMARY KEY (tenant_id, repo_id, pull_number, filename)
);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_request_files (
    tenant_id VARCHAR(36) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    filename VARCHAR(768) NOT NULL,
    status VARCHAR(32) NOT NULL,
    additions BIGINT NOT NULL,
    deletions BIGINT NOT NULL,
    changes BIGINT NOT NULL,
    previous_filename VARCHAR(1024),
    sha VARCHAR(64),
    PRIMARY KEY (tenant_id, repo_id, pull_number, filename)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_request_files (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    filename TEXT NOT NULL,
    status TEXT NOT NULL,
    additions INTEGER NOT NULL,
    deletions INTEGER NOT NULL,
    changes INTEGER NOT NULL,
    previous_filename TEXT,
    sha TEXT,
    PRIMARY KEY (tenant_id, repo_id, pull_number, filename)
);
                "
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_pull_request_files;")
            .await?;
        Ok(())
    }
}
