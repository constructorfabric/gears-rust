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
CREATE TABLE IF NOT EXISTS gm_issues (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    body TEXT,
    state VARCHAR(32) NOT NULL,
    is_pull_request BOOLEAN NOT NULL,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_issues_tenant_repo_number
    ON gm_issues (tenant_id, repo_id, number);
CREATE INDEX IF NOT EXISTS idx_gm_issues_tenant_repo_state
    ON gm_issues (tenant_id, repo_id, state);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_issues (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    body TEXT,
    state VARCHAR(32) NOT NULL,
    is_pull_request BOOLEAN NOT NULL,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, id),
    UNIQUE KEY idx_gm_issues_tenant_repo_number (tenant_id, repo_id, number),
    KEY idx_gm_issues_tenant_repo_state (tenant_id, repo_id, state)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_issues (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    state TEXT NOT NULL,
    is_pull_request INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    html_url TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_issues_tenant_repo_number
    ON gm_issues (tenant_id, repo_id, number);
CREATE INDEX IF NOT EXISTS idx_gm_issues_tenant_repo_state
    ON gm_issues (tenant_id, repo_id, state);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_issues;")
            .await?;
        Ok(())
    }
}
