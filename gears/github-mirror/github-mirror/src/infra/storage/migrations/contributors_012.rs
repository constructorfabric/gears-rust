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
CREATE TABLE IF NOT EXISTS gm_contributors (
    tenant_id UUID NOT NULL,
    repo_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    login VARCHAR(255) NOT NULL,
    contributions BIGINT NOT NULL,
    user_type VARCHAR(32) NOT NULL,
    avatar_url VARCHAR(1024),
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, repo_id, user_id)
);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_contributors (
    tenant_id VARCHAR(36) NOT NULL,
    repo_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    login VARCHAR(255) NOT NULL,
    contributions BIGINT NOT NULL,
    user_type VARCHAR(32) NOT NULL,
    avatar_url VARCHAR(1024),
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, repo_id, user_id)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_contributors (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    login TEXT NOT NULL,
    contributions INTEGER NOT NULL,
    user_type TEXT NOT NULL,
    avatar_url TEXT,
    html_url TEXT,
    PRIMARY KEY (tenant_id, repo_id, user_id)
);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_contributors;")
            .await?;
        Ok(())
    }
}
