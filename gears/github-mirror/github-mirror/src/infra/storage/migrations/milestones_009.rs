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
CREATE TABLE IF NOT EXISTS gm_milestones (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    state VARCHAR(32) NOT NULL,
    description TEXT,
    open_issues BIGINT NOT NULL,
    closed_issues BIGINT NOT NULL,
    due_on VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_milestones_tenant_repo
    ON gm_milestones (tenant_id, repo_id);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_milestones (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    title VARCHAR(1024) NOT NULL,
    state VARCHAR(32) NOT NULL,
    description MEDIUMTEXT,
    open_issues BIGINT NOT NULL,
    closed_issues BIGINT NOT NULL,
    due_on VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    closed_at VARCHAR(64),
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_milestones_tenant_repo (tenant_id, repo_id)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_milestones (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    description TEXT,
    open_issues INTEGER NOT NULL,
    closed_issues INTEGER NOT NULL,
    due_on TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    html_url TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_milestones_tenant_repo
    ON gm_milestones (tenant_id, repo_id);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_milestones;")
            .await?;
        Ok(())
    }
}
