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
CREATE TABLE IF NOT EXISTS gm_commit_statuses (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    state VARCHAR(32) NOT NULL,
    context VARCHAR(512) NOT NULL,
    description TEXT,
    target_url VARCHAR(1024),
    creator_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_commit_statuses_tenant_repo_sha
    ON gm_commit_statuses (tenant_id, repo_id, commit_sha);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_commit_statuses (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    commit_sha VARCHAR(64) NOT NULL,
    state VARCHAR(32) NOT NULL,
    context VARCHAR(512) NOT NULL,
    description MEDIUMTEXT,
    target_url VARCHAR(1024),
    creator_login VARCHAR(255),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_commit_statuses_tenant_repo_sha (tenant_id, repo_id, commit_sha)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_commit_statuses (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    state TEXT NOT NULL,
    context TEXT NOT NULL,
    description TEXT,
    target_url TEXT,
    creator_login TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_commit_statuses_tenant_repo_sha
    ON gm_commit_statuses (tenant_id, repo_id, commit_sha);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_commit_statuses;")
            .await?;
        Ok(())
    }
}
