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
CREATE TABLE IF NOT EXISTS gm_review_comments (
    tenant_id UUID NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    author_login VARCHAR(255),
    body TEXT,
    path VARCHAR(1024),
    diff_hunk TEXT,
    in_reply_to_id BIGINT,
    commit_id VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_review_comments_tenant_repo_pull
    ON gm_review_comments (tenant_id, repo_id, pull_number);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                r"
CREATE TABLE IF NOT EXISTS gm_review_comments (
    tenant_id VARCHAR(36) NOT NULL,
    id BIGINT NOT NULL,
    repo_id BIGINT NOT NULL,
    pull_number BIGINT NOT NULL,
    author_login VARCHAR(255),
    body MEDIUMTEXT,
    path VARCHAR(1024),
    diff_hunk MEDIUMTEXT,
    in_reply_to_id BIGINT,
    commit_id VARCHAR(64),
    created_at VARCHAR(64) NOT NULL,
    updated_at VARCHAR(64) NOT NULL,
    html_url VARCHAR(1024),
    PRIMARY KEY (tenant_id, id),
    KEY idx_gm_review_comments_tenant_repo_pull (tenant_id, repo_id, pull_number)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS gm_review_comments (
    tenant_id TEXT NOT NULL,
    id INTEGER NOT NULL,
    repo_id INTEGER NOT NULL,
    pull_number INTEGER NOT NULL,
    author_login TEXT,
    body TEXT,
    path TEXT,
    diff_hunk TEXT,
    in_reply_to_id INTEGER,
    commit_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    html_url TEXT,
    PRIMARY KEY (tenant_id, id)
);
CREATE INDEX IF NOT EXISTS idx_gm_review_comments_tenant_repo_pull
    ON gm_review_comments (tenant_id, repo_id, pull_number);
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
        conn.execute_unprepared("DROP TABLE IF EXISTS gm_review_comments;")
            .await?;
        Ok(())
    }
}
