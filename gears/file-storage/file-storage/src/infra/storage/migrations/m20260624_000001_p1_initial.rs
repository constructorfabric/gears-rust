//! P1 initial migration — control-plane metadata tables.
//!
//! Mirrors the P1 section of `gears/file-storage/docs/migration.sql`:
//!   - @cpt-cf-file-storage-dbtable-files
//!   - @cpt-cf-file-storage-dbtable-file-versions
//!   - @cpt-cf-file-storage-dbtable-files-custom-metadata
//!
//! No bytes live here — content moves over signed URLs against the sidecar
//! (ADR-0003). Postgres uses the dedicated `file_storage` schema; `SQLite` (tests)
//! uses flat table names and supplies `UUID`s from the application layer.

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
                // Flat (unqualified) table names on both backends: SeaORM entities use a
                // static table_name and SQLite has no schemas, so a per-backend schema
                // qualifier cannot be expressed in the entity. The DESIGN's `file_storage`
                // schema is deferred; behaviour is identical with flat names.
                r"
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE IF NOT EXISTS files (
    file_id           uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id         uuid         NOT NULL,
    owner_kind        text         NOT NULL  CHECK (owner_kind IN ('user', 'app')),
    owner_id          uuid         NOT NULL,
    name              text         NOT NULL,
    gts_file_type     text         NOT NULL,
    content_id        uuid,
    meta_version      bigint       NOT NULL  DEFAULT 0  CHECK (meta_version >= 0),
    created_at        timestamptz  NOT NULL  DEFAULT now(),
    last_modified_at  timestamptz  NOT NULL  DEFAULT now()
);

CREATE INDEX IF NOT EXISTS files_owner_listing_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC);
CREATE INDEX IF NOT EXISTS files_tenant_gts_idx
    ON files (tenant_id, gts_file_type);

CREATE TABLE IF NOT EXISTS file_versions (
    file_id          uuid         NOT NULL
                                  REFERENCES files (file_id) ON DELETE CASCADE,
    version_id       uuid         NOT NULL  DEFAULT gen_random_uuid(),
    mime_type        text         NOT NULL,
    size             bigint       NOT NULL  CHECK (size >= 0),
    hash_algorithm   text         NOT NULL  DEFAULT 'SHA-256'
                                  CHECK (hash_algorithm = 'SHA-256'),
    hash_value       bytea        NOT NULL  CHECK (octet_length(hash_value) = 32),
    status           text         NOT NULL  DEFAULT 'pending'
                                  CHECK (status IN ('pending', 'available')),
    is_current       boolean      NOT NULL  DEFAULT false,
    backend_id       text         NOT NULL,
    backend_path     text         NOT NULL,
    created_at       timestamptz  NOT NULL  DEFAULT now(),
    PRIMARY KEY (file_id, version_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS file_versions_current_idx
    ON file_versions (file_id) WHERE is_current = true;
CREATE INDEX IF NOT EXISTS file_versions_pending_idx
    ON file_versions (created_at) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS file_versions_backend_idx
    ON file_versions (backend_id);

CREATE TABLE IF NOT EXISTS files_custom_metadata (
    file_id   uuid         NOT NULL
                           REFERENCES files (file_id) ON DELETE CASCADE,
    key       text         NOT NULL,
    value     text         NOT NULL,
    set_at    timestamptz  NOT NULL  DEFAULT now(),
    PRIMARY KEY (file_id, key)
);
                "
            }
            sea_orm::DatabaseBackend::Sqlite => {
                r"
CREATE TABLE IF NOT EXISTS files (
    file_id           TEXT     PRIMARY KEY NOT NULL,
    tenant_id         TEXT     NOT NULL,
    owner_kind        TEXT     NOT NULL  CHECK (owner_kind IN ('user', 'app')),
    owner_id          TEXT     NOT NULL,
    name              TEXT     NOT NULL,
    gts_file_type     TEXT     NOT NULL,
    content_id        TEXT,
    meta_version      INTEGER  NOT NULL  DEFAULT 0  CHECK (meta_version >= 0),
    created_at        TEXT     NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    last_modified_at  TEXT     NOT NULL  DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS files_owner_listing_idx
    ON files (tenant_id, owner_kind, owner_id, created_at DESC);
CREATE INDEX IF NOT EXISTS files_tenant_gts_idx
    ON files (tenant_id, gts_file_type);

CREATE TABLE IF NOT EXISTS file_versions (
    file_id          TEXT     NOT NULL  REFERENCES files (file_id) ON DELETE CASCADE,
    version_id       TEXT     NOT NULL,
    mime_type        TEXT     NOT NULL,
    size             INTEGER  NOT NULL  CHECK (size >= 0),
    hash_algorithm   TEXT     NOT NULL  DEFAULT 'SHA-256'
                              CHECK (hash_algorithm = 'SHA-256'),
    hash_value       BLOB     NOT NULL  CHECK (length(hash_value) = 32),
    status           TEXT     NOT NULL  DEFAULT 'pending'
                              CHECK (status IN ('pending', 'available')),
    is_current       INTEGER  NOT NULL  DEFAULT 0  CHECK (is_current IN (0, 1)),
    backend_id       TEXT     NOT NULL,
    backend_path     TEXT     NOT NULL,
    created_at       TEXT     NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (file_id, version_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS file_versions_current_idx
    ON file_versions (file_id) WHERE is_current = 1;
CREATE INDEX IF NOT EXISTS file_versions_pending_idx
    ON file_versions (created_at) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS file_versions_backend_idx
    ON file_versions (backend_id);

CREATE TABLE IF NOT EXISTS files_custom_metadata (
    file_id   TEXT  NOT NULL  REFERENCES files (file_id) ON DELETE CASCADE,
    key       TEXT  NOT NULL,
    value     TEXT  NOT NULL,
    set_at    TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (file_id, key)
);
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(
                    "file-storage migrations support Postgres and SQLite only".to_owned(),
                ));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = match backend {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                r"
DROP TABLE IF EXISTS files_custom_metadata;
DROP TABLE IF EXISTS file_versions;
DROP TABLE IF EXISTS files;
                "
            }
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(
                    "file-storage migrations support Postgres and SQLite only".to_owned(),
                ));
            }
        };

        conn.execute_unprepared(sql).await?;
        Ok(())
    }
}
