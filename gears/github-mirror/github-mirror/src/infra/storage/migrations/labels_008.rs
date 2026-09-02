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
CREATE TABLE IF NOT EXISTS gm_labels (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    name VARCHAR(255) NOT NULL,
    color VARCHAR(16) NOT NULL,
    is_default BOOLEAN NOT NULL,
    description VARCHAR(1024),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_labels_tenant_repo
    ON gm_labels (tenant_id, repo_id);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_labels (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    name VARCHAR(255) NOT NULL,
    color VARCHAR(16) NOT NULL,
    is_default BOOLEAN NOT NULL,
    description VARCHAR(1024),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_labels_tenant_repo (tenant_id, repo_id)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_labels (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    color TEXT NOT NULL,
    is_default INTEGER NOT NULL,
    description TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_labels_tenant_repo
    ON gm_labels (tenant_id, repo_id);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_labels;")
            .await?;
        Ok(())
    }
}
