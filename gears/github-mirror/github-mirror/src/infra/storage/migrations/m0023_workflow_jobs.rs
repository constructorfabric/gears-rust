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
CREATE TABLE IF NOT EXISTS gm_workflow_jobs (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    run_id BIGINT NOT NULL,
    run_attempt BIGINT NOT NULL,
    name VARCHAR(1024) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    head_sha VARCHAR(64) NOT NULL,
    runner_name VARCHAR(255),
    started_at VARCHAR(64),
    completed_at VARCHAR(64),
    html_url VARCHAR(1024),
    steps_json TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_workflow_jobs_tenant_repo_run
    ON gm_workflow_jobs (tenant_id, repo_id, run_id);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_workflow_jobs (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    run_id BIGINT NOT NULL,
    run_attempt BIGINT NOT NULL,
    name VARCHAR(1024) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    head_sha VARCHAR(64) NOT NULL,
    runner_name VARCHAR(255),
    started_at VARCHAR(64),
    completed_at VARCHAR(64),
    html_url VARCHAR(1024),
    steps_json MEDIUMTEXT,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_workflow_jobs_tenant_repo_run (tenant_id, repo_id, run_id)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_workflow_jobs (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    run_id INTEGER NOT NULL,
    run_attempt INTEGER NOT NULL,
    name TEXT NOT NULL,
    status TEXT,
    conclusion TEXT,
    head_sha TEXT NOT NULL,
    runner_name TEXT,
    started_at TEXT,
    completed_at TEXT,
    html_url TEXT,
    steps_json TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_workflow_jobs_tenant_repo_run
    ON gm_workflow_jobs (tenant_id, repo_id, run_id);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_workflow_jobs;")
            .await?;
        Ok(())
    }
}
