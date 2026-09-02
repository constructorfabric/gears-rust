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
CREATE TABLE IF NOT EXISTS gm_check_runs (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    head_sha VARCHAR(64) NOT NULL,
    name VARCHAR(1024) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    started_at VARCHAR(64),
    completed_at VARCHAR(64),
    html_url VARCHAR(1024),
    details_url VARCHAR(1024),
    check_suite_id BIGINT,
    app_slug VARCHAR(255),
    app_name VARCHAR(255),
    output_title VARCHAR(1024),
    output_summary TEXT,
    annotations_count BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_check_runs_tenant_repo_sha
    ON gm_check_runs (tenant_id, repo_id, head_sha);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_check_runs (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    head_sha VARCHAR(64) NOT NULL,
    name VARCHAR(1024) NOT NULL,
    status VARCHAR(32),
    conclusion VARCHAR(32),
    started_at VARCHAR(64),
    completed_at VARCHAR(64),
    html_url VARCHAR(1024),
    details_url VARCHAR(1024),
    check_suite_id BIGINT,
    app_slug VARCHAR(255),
    app_name VARCHAR(255),
    output_title VARCHAR(1024),
    output_summary MEDIUMTEXT,
    annotations_count BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_check_runs_tenant_repo_sha (tenant_id, repo_id, head_sha)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_check_runs (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    head_sha TEXT NOT NULL,
    name TEXT NOT NULL,
    status TEXT,
    conclusion TEXT,
    started_at TEXT,
    completed_at TEXT,
    html_url TEXT,
    details_url TEXT,
    check_suite_id INTEGER,
    app_slug TEXT,
    app_name TEXT,
    output_title TEXT,
    output_summary TEXT,
    annotations_count INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_check_runs_tenant_repo_sha
    ON gm_check_runs (tenant_id, repo_id, head_sha);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_check_runs;")
            .await?;
        Ok(())
    }
}
