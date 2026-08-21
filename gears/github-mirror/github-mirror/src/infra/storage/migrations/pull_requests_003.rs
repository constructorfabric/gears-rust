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
CREATE TABLE IF NOT EXISTS gm_pull_requests (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    body TEXT,
    state VARCHAR(32) NOT NULL,
    draft BOOLEAN NOT NULL,
    merged BOOLEAN NOT NULL,
    head_sha VARCHAR(64),
    base_sha VARCHAR(64),
    lines_added BIGINT NOT NULL DEFAULT 0,
    lines_removed BIGINT NOT NULL DEFAULT 0,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    merged_at VARCHAR(64),
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_pull_requests_tenant_repo_number
    ON gm_pull_requests (tenant_id, repo_id, number);
CREATE INDEX IF NOT EXISTS idx_gm_pull_requests_tenant_repo_state
    ON gm_pull_requests (tenant_id, repo_id, state);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_requests (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    body TEXT,
    state VARCHAR(32) NOT NULL,
    draft BOOLEAN NOT NULL,
    merged BOOLEAN NOT NULL,
    head_sha VARCHAR(64),
    base_sha VARCHAR(64),
    lines_added BIGINT NOT NULL DEFAULT 0,
    lines_removed BIGINT NOT NULL DEFAULT 0,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    merged_at VARCHAR(64),
    PRIMARY KEY (tenant_id, id),
    UNIQUE KEY idx_gm_pull_requests_tenant_repo_number (tenant_id, repo_id, number),
    KEY idx_gm_pull_requests_tenant_repo_state (tenant_id, repo_id, state)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_pull_requests (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    state TEXT NOT NULL,
    draft INTEGER NOT NULL,
    merged INTEGER NOT NULL,
    head_sha TEXT,
    base_sha TEXT,
    lines_added INTEGER NOT NULL DEFAULT 0,
    lines_removed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    merged_at TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_pull_requests_tenant_repo_number
    ON gm_pull_requests (tenant_id, repo_id, number);
CREATE INDEX IF NOT EXISTS idx_gm_pull_requests_tenant_repo_state
    ON gm_pull_requests (tenant_id, repo_id, state);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_pull_requests;")
            .await?;
        Ok(())
    }
}
