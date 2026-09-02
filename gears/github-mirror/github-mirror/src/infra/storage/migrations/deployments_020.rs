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
CREATE TABLE IF NOT EXISTS gm_deployments (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    git_ref VARCHAR(512) NOT NULL,
    sha VARCHAR(64) NOT NULL,
    environment VARCHAR(255) NOT NULL,
    task VARCHAR(255) NOT NULL,
    description TEXT,
    creator_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_deployments_tenant_repo
    ON gm_deployments (tenant_id, repo_id);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_deployments (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    git_ref VARCHAR(512) NOT NULL,
    sha VARCHAR(64) NOT NULL,
    environment VARCHAR(255) NOT NULL,
    task VARCHAR(255) NOT NULL,
    description MEDIUMTEXT,
    creator_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_deployments_tenant_repo (tenant_id, repo_id)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_deployments (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    git_ref TEXT NOT NULL,
    sha TEXT NOT NULL,
    environment TEXT NOT NULL,
    task TEXT NOT NULL,
    description TEXT,
    creator_login TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_deployments_tenant_repo
    ON gm_deployments (tenant_id, repo_id);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_deployments;")
            .await?;
        Ok(())
    }
}
