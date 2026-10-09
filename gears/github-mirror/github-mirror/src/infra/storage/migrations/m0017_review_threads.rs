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
CREATE TABLE IF NOT EXISTS gm_review_threads (
    tenant_id UUID NOT NULL,
    id VARCHAR(128) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    is_resolved BOOLEAN NOT NULL,
    is_outdated BOOLEAN NOT NULL,
    path VARCHAR(1024),
    line BIGINT,
    resolved_by VARCHAR(255),
    comments_count BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_review_threads_tenant_repo_pull
    ON gm_review_threads (tenant_id, repo_id, pull_number);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_review_threads (
    tenant_id VARCHAR(36) NOT NULL,
    id VARCHAR(128) NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    is_resolved BOOLEAN NOT NULL,
    is_outdated BOOLEAN NOT NULL,
    path VARCHAR(1024),
    line BIGINT,
    resolved_by VARCHAR(255),
    comments_count BIGINT NOT NULL,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_review_threads_tenant_repo_pull (tenant_id, repo_id, pull_number)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_review_threads (
    tenant_id TEXT NOT NULL,
    id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    is_resolved INTEGER NOT NULL,
    is_outdated INTEGER NOT NULL,
    path TEXT,
    line INTEGER,
    resolved_by TEXT,
    comments_count INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_review_threads_tenant_repo_pull
    ON gm_review_threads (tenant_id, repo_id, pull_number);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_review_threads;")
            .await?;
        Ok(())
    }
}
