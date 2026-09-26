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
CREATE TABLE IF NOT EXISTS gm_workflow_runs (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    workflow_id BIGINT NOT NULL,
    run_number BIGINT NOT NULL,
    run_attempt BIGINT NOT NULL,
    name VARCHAR(1024),
    event VARCHAR(64) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    head_branch VARCHAR(512),
    head_sha VARCHAR(64) NOT NULL,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    actor_login VARCHAR(255),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_workflow_runs_tenant_repo
    ON gm_workflow_runs (tenant_id, repo_id);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_workflow_runs (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    workflow_id BIGINT NOT NULL,
    run_number BIGINT NOT NULL,
    run_attempt BIGINT NOT NULL,
    name VARCHAR(1024),
    event VARCHAR(64) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    head_branch VARCHAR(512),
    head_sha VARCHAR(64) NOT NULL,
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    actor_login VARCHAR(255),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_workflow_runs_tenant_repo (tenant_id, repo_id)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_workflow_runs (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    workflow_id INTEGER NOT NULL,
    run_number INTEGER NOT NULL,
    run_attempt INTEGER NOT NULL,
    name TEXT,
    event TEXT NOT NULL,
    status TEXT,
    conclusion TEXT,
    head_branch TEXT,
    head_sha TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT,
    actor_login TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_workflow_runs_tenant_repo
    ON gm_workflow_runs (tenant_id, repo_id);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_workflow_runs;")
            .await?;
        Ok(())
    }
}
