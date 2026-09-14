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
CREATE TABLE IF NOT EXISTS gm_pull_request_commits (
    tenant_id UUID NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    sha VARCHAR(64) NOT NULL,
    message TEXT NOT NULL,
    author_login VARCHAR(255),
    committer_login VARCHAR(255),
    authored_at VARCHAR(64),
    committed_at VARCHAR(64),
    PRIMARY KEY (tenant_id, repo_id, pull_number, sha)
);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_request_commits (
    tenant_id VARCHAR(36) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    sha VARCHAR(64) NOT NULL,
    message MEDIUMTEXT NOT NULL,
    author_login VARCHAR(255),
    committer_login VARCHAR(255),
    authored_at VARCHAR(64),
    committed_at VARCHAR(64),
    PRIMARY KEY (tenant_id, repo_id, pull_number, sha)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_request_commits (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    sha TEXT NOT NULL,
    message TEXT NOT NULL,
    author_login TEXT,
    committer_login TEXT,
    authored_at TEXT,
    committed_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, pull_number, sha)
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_pull_request_commits;")
            .await?;
        Ok(())
    }
}
