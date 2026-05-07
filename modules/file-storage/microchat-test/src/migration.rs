//! Single-step migration for the test microchat's `chat_attachments`
//! table. Runs against the same `Db` handle that holds
//! `cf-file-storage`'s tables — two namespaces, one SQLite — to keep
//! the test harness simple.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(MicrochatMigrationName)]
    }
}

#[derive(DeriveMigrationName)]
pub struct MicrochatMigrationName;

#[async_trait::async_trait]
impl MigrationTrait for MicrochatMigrationName {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let create_table = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                r#"
CREATE TABLE IF NOT EXISTS chat_attachments (
    file_id     UUID PRIMARY KEY,
    chat_id     UUID NOT NULL,
    owner_id    UUID NOT NULL,
    name        TEXT NOT NULL,
    mime        TEXT NOT NULL,
    status      TEXT NOT NULL CHECK(status IN ('pending','active','deleted')),
    etag        TEXT,
    size_bytes  BIGINT,
    created_at  TEXT NOT NULL
);
"#
            }
            sea_orm::DatabaseBackend::MySql => {
                r#"
CREATE TABLE IF NOT EXISTS chat_attachments (
    file_id     CHAR(36) PRIMARY KEY,
    chat_id     CHAR(36) NOT NULL,
    owner_id    CHAR(36) NOT NULL,
    name        TEXT NOT NULL,
    mime        TEXT NOT NULL,
    status      VARCHAR(16) NOT NULL CHECK(status IN ('pending','active','deleted')),
    etag        TEXT,
    size_bytes  BIGINT,
    created_at  VARCHAR(64) NOT NULL
);
"#
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r#"
CREATE TABLE IF NOT EXISTS chat_attachments (
    file_id     TEXT PRIMARY KEY,
    chat_id     TEXT NOT NULL,
    owner_id    TEXT NOT NULL,
    name        TEXT NOT NULL,
    mime        TEXT NOT NULL,
    status      TEXT NOT NULL CHECK(status IN ('pending','active','deleted')),
    etag        TEXT,
    size_bytes  INTEGER,
    created_at  TEXT NOT NULL
);
"#
            }
        };
        conn.execute_unprepared(create_table).await?;

        let indexes = [
            "CREATE INDEX IF NOT EXISTS idx_chat_attachments_owner_status \
             ON chat_attachments (owner_id, status);",
            "CREATE INDEX IF NOT EXISTS idx_chat_attachments_chat \
             ON chat_attachments (chat_id);",
        ];
        for sql in indexes {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS chat_attachments;")
            .await?;
        Ok(())
    }
}
