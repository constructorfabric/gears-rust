//! SeaORM migrations for the FileStorage module.
//!
//! P1 ships a single migration that mirrors `modules/file-storage/docs/migration.sql`.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(InitialV1)]
    }
}

#[derive(DeriveMigrationName)]
struct InitialV1;

#[async_trait::async_trait]
impl MigrationTrait for InitialV1 {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let create_table = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                r#"
CREATE TABLE IF NOT EXISTS files (
    id                            UUID PRIMARY KEY,
    tenant_id                     UUID NOT NULL,
    backend_id                    UUID NOT NULL,
    file_path                     TEXT NOT NULL,
    owner_id                      UUID NOT NULL,
    name                          VARCHAR(512) NOT NULL,
    gts_file_type                 VARCHAR(256) NOT NULL,
    mime_type                     VARCHAR(256) NOT NULL,
    size_bytes                    BIGINT NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    etag                          VARCHAR(128) NOT NULL,
    meta_revision                 BIGINT NOT NULL DEFAULT 0 CHECK (meta_revision >= 0),
    status                        VARCHAR(16) NOT NULL DEFAULT 'pending_upload',
    custom_metadata               JSONB NOT NULL,
    upload_expires_at             TIMESTAMPTZ,
    created_at                    TIMESTAMPTZ NOT NULL,
    modified_at                   TIMESTAMPTZ NOT NULL
);
"#
            }
            sea_orm::DatabaseBackend::MySql => {
                r#"
CREATE TABLE IF NOT EXISTS files (
    id                            CHAR(36) PRIMARY KEY,
    tenant_id                     CHAR(36) NOT NULL,
    backend_id                    CHAR(36) NOT NULL,
    file_path                     TEXT NOT NULL,
    owner_id                      CHAR(36) NOT NULL,
    name                          VARCHAR(512) NOT NULL,
    gts_file_type                 VARCHAR(256) NOT NULL,
    mime_type                     VARCHAR(256) NOT NULL,
    size_bytes                    BIGINT NOT NULL DEFAULT 0,
    etag                          VARCHAR(128) NOT NULL,
    meta_revision                 BIGINT NOT NULL DEFAULT 0,
    status                        VARCHAR(16) NOT NULL DEFAULT 'pending_upload',
    custom_metadata               JSON NOT NULL,
    upload_expires_at             DATETIME(6) NULL,
    created_at                    DATETIME(6) NOT NULL,
    modified_at                   DATETIME(6) NOT NULL
);
"#
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r#"
CREATE TABLE IF NOT EXISTS files (
    id                            TEXT PRIMARY KEY,
    tenant_id                     TEXT NOT NULL,
    backend_id                    TEXT NOT NULL,
    file_path                     TEXT NOT NULL,
    owner_id                      TEXT NOT NULL,
    name                          TEXT NOT NULL,
    gts_file_type                 TEXT NOT NULL,
    mime_type                     TEXT NOT NULL,
    size_bytes                    INTEGER NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    etag                          TEXT NOT NULL,
    meta_revision                 INTEGER NOT NULL DEFAULT 0 CHECK (meta_revision >= 0),
    status                        TEXT NOT NULL DEFAULT 'pending_upload',
    custom_metadata               TEXT NOT NULL,
    upload_expires_at             TEXT,
    created_at                    TEXT NOT NULL,
    modified_at                   TEXT NOT NULL
);
"#
            }
        };
        conn.execute_unprepared(create_table).await?;

        // Partial unique index — supported by Postgres and SQLite.
        let partial_uq = match backend {
            sea_orm::DatabaseBackend::Postgres => {
                "CREATE UNIQUE INDEX IF NOT EXISTS files_tenant_backend_path_uploaded_uq \
                 ON files (tenant_id, backend_id, file_path) \
                 WHERE status = 'uploaded';"
            }
            sea_orm::DatabaseBackend::Sqlite => {
                "CREATE UNIQUE INDEX IF NOT EXISTS files_tenant_backend_path_uploaded_uq \
                 ON files (tenant_id, backend_id, file_path) \
                 WHERE status = 'uploaded';"
            }
            sea_orm::DatabaseBackend::MySql => "",
        };
        if !partial_uq.is_empty() {
            conn.execute_unprepared(partial_uq).await?;
        }

        let other_indexes = [
            "CREATE INDEX IF NOT EXISTS files_tenant_backend_owner_idx \
             ON files (tenant_id, backend_id, owner_id);",
            "CREATE INDEX IF NOT EXISTS files_owner_lookup_idx \
             ON files (tenant_id, owner_id);",
            "CREATE INDEX IF NOT EXISTS files_created_idx \
             ON files (tenant_id, created_at);",
        ];
        for sql in other_indexes {
            conn.execute_unprepared(sql).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS files;").await?;
        Ok(())
    }
}
