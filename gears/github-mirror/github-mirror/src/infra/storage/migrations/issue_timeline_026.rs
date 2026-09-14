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
CREATE TABLE IF NOT EXISTS gm_issue_timeline (
    tenant_id UUID NOT NULL,
    repo_id BIGINT NOT NULL,
    issue_number BIGINT NOT NULL,
    position BIGINT NOT NULL,
    event VARCHAR(64) NOT NULL,
    created_at VARCHAR(64),
    actor_login VARCHAR(255),
    payload_json TEXT NOT NULL,
    PRIMARY KEY (tenant_id, repo_id, issue_number, position)
);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_issue_timeline (
    tenant_id VARCHAR(36) NOT NULL,
    repo_id BIGINT NOT NULL,
    issue_number BIGINT NOT NULL,
    position BIGINT NOT NULL,
    event VARCHAR(64) NOT NULL,
    created_at VARCHAR(64),
    actor_login VARCHAR(255),
    payload_json MEDIUMTEXT NOT NULL,
    PRIMARY KEY (tenant_id, repo_id, issue_number, position)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_issue_timeline (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    issue_number INTEGER NOT NULL,
    position INTEGER NOT NULL,
    event TEXT NOT NULL,
    created_at TEXT,
    actor_login TEXT,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (tenant_id, repo_id, issue_number, position)
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_issue_timeline;")
            .await?;
        Ok(())
    }
}
