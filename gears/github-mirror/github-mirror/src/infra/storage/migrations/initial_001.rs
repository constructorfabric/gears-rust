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
CREATE TABLE IF NOT EXISTS gm_repositories (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    owner VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    full_name VARCHAR(512) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    private BOOLEAN NOT NULL,
    pushed_at VARCHAR(64),
    stars BIGINT NOT NULL DEFAULT 0,
    forks BIGINT NOT NULL DEFAULT 0,
    description TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_repositories_tenant_full_name
    ON gm_repositories (tenant_id, full_name);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_repositories (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    owner VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    full_name VARCHAR(512) NOT NULL,
    default_branch VARCHAR(255) NOT NULL,
    private BOOLEAN NOT NULL,
    pushed_at VARCHAR(64),
    stars BIGINT NOT NULL DEFAULT 0,
    forks BIGINT NOT NULL DEFAULT 0,
    description TEXT,
    PRIMARY KEY (tenant_id, id),
    UNIQUE KEY idx_gm_repositories_tenant_full_name (tenant_id, full_name(255))
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_repositories (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    full_name TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    private INTEGER NOT NULL,
    pushed_at TEXT,
    stars INTEGER NOT NULL DEFAULT 0,
    forks INTEGER NOT NULL DEFAULT 0,
    description TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_gm_repositories_tenant_full_name
    ON gm_repositories (tenant_id, full_name);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_repositories;")
            .await?;
        Ok(())
    }
}
