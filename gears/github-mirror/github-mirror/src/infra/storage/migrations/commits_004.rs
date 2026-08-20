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
CREATE TABLE IF NOT EXISTS gm_commits (
    tenant_id UUID NOT NULL,
    repo_id BIGINT NOT NULL,
    sha VARCHAR(64) NOT NULL,
    message TEXT NOT NULL,
    author_login VARCHAR(255),
    committer_login VARCHAR(255),
    authored_at VARCHAR(64),
    committed_at VARCHAR(64),
    additions BIGINT NOT NULL DEFAULT 0,
    deletions BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, repo_id, sha)
);
CREATE INDEX IF NOT EXISTS idx_gm_commits_tenant_repo_committed
    ON gm_commits (tenant_id, repo_id, committed_at);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_commits (
    tenant_id VARCHAR(36) NOT NULL,
    repo_id BIGINT NOT NULL,
    sha VARCHAR(64) NOT NULL,
    message TEXT NOT NULL,
    author_login VARCHAR(255),
    committer_login VARCHAR(255),
    authored_at VARCHAR(64),
    committed_at VARCHAR(64),
    additions BIGINT NOT NULL DEFAULT 0,
    deletions BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, repo_id, sha),
    KEY idx_gm_commits_tenant_repo_committed (tenant_id, repo_id, committed_at)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_commits (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    sha TEXT NOT NULL,
    message TEXT NOT NULL,
    author_login TEXT,
    committer_login TEXT,
    authored_at TEXT,
    committed_at TEXT,
    additions INTEGER NOT NULL DEFAULT 0,
    deletions INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, repo_id, sha)
);
CREATE INDEX IF NOT EXISTS idx_gm_commits_tenant_repo_committed
    ON gm_commits (tenant_id, repo_id, committed_at);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_commits;")
            .await?;
        Ok(())
    }
}
