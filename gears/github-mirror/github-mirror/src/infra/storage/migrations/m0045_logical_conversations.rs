use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

use super::support::drop_column;

/// Logical conversations derived after each sync: inline review-comment reply
/// chains and top-level comments grouped by quoting. One row per conversation,
/// and a `conversation_id` on every comment pointing at its root.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => &[
                r"
CREATE TABLE IF NOT EXISTS gm_logical_conversations (
    tenant_id UUID NOT NULL,
    repo_id BIGINT NOT NULL,
    conv_type VARCHAR(16) NOT NULL,
    root_comment_id BIGINT NOT NULL,
    parent_kind VARCHAR(16) NOT NULL,
    parent_number BIGINT NOT NULL,
    comment_count BIGINT NOT NULL,
    is_resolved BOOLEAN,
    created_at VARCHAR(64),
    extracted_at TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, repo_id, conv_type, root_comment_id)
);
                ",
                "CREATE INDEX IF NOT EXISTS idx_gm_logical_conversations_parent ON gm_logical_conversations (tenant_id, repo_id, parent_number);",
                "ALTER TABLE gm_comments ADD COLUMN IF NOT EXISTS conversation_id BIGINT;",
                "ALTER TABLE gm_review_comments ADD COLUMN IF NOT EXISTS conversation_id BIGINT;",
            ],
            sea_orm::DatabaseBackend::MySql => &[
                r"
CREATE TABLE IF NOT EXISTS gm_logical_conversations (
    tenant_id BINARY(16) NOT NULL,
    repo_id BIGINT NOT NULL,
    conv_type VARCHAR(16) NOT NULL,
    root_comment_id BIGINT NOT NULL,
    parent_kind VARCHAR(16) NOT NULL,
    parent_number BIGINT NOT NULL,
    comment_count BIGINT NOT NULL,
    is_resolved BOOLEAN,
    created_at VARCHAR(64),
    extracted_at DATETIME(6),
    PRIMARY KEY (tenant_id, repo_id, conv_type, root_comment_id),
    INDEX idx_gm_logical_conversations_parent (tenant_id, repo_id, parent_number)
);
                ",
                "ALTER TABLE gm_comments ADD COLUMN conversation_id BIGINT;",
                "ALTER TABLE gm_review_comments ADD COLUMN conversation_id BIGINT;",
            ],
            sea_orm::DatabaseBackend::Sqlite => &[
                r"
CREATE TABLE IF NOT EXISTS gm_logical_conversations (
    tenant_id TEXT NOT NULL,
    repo_id INTEGER NOT NULL,
    conv_type TEXT NOT NULL,
    root_comment_id INTEGER NOT NULL,
    parent_kind TEXT NOT NULL,
    parent_number INTEGER NOT NULL,
    comment_count INTEGER NOT NULL,
    is_resolved BOOLEAN,
    created_at TEXT,
    extracted_at TEXT,
    PRIMARY KEY (tenant_id, repo_id, conv_type, root_comment_id)
);
                ",
                "CREATE INDEX IF NOT EXISTS idx_gm_logical_conversations_parent ON gm_logical_conversations (tenant_id, repo_id, parent_number);",
                "ALTER TABLE gm_comments ADD COLUMN conversation_id BIGINT;",
                "ALTER TABLE gm_review_comments ADD COLUMN conversation_id BIGINT;",
            ],
            other => {
                return Err(DbErr::Custom(format!(
                    "migration has no DDL for database backend {other:?}"
                )));
            }
        };

        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column(manager, "gm_review_comments", "conversation_id").await?;
        drop_column(manager, "gm_comments", "conversation_id").await?;
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS gm_logical_conversations;")
            .await?;
        Ok(())
    }
}
