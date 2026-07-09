//! P2 initial migration — all P2 schema in one step.
//!
//! Combines every P2 milestone's DDL into a single migration so P2 ships as
//! one atomic schema bump on top of the P1 baseline. Tables (in FK order):
//!   - `policies`: per-tenant / per-user policy body (allowed types, size
//!     limits, metadata limits, enabled event types). Body is JSONB/text.
//!   - `retention_rules`: per-tenant / per-user / per-file retention criteria.
//!   - `multipart_uploads`: in-flight multipart upload sessions.
//!   - `multipart_upload_parts`: individual parts within a session.
//!   - `idempotency_keys`: deduplication keys for POST /files.
//!   - `audit_outbox`: transactional-outbox rows for the audit trail.
//!   - `events_outbox`: transactional-outbox rows for file events.
//!
//! Mirrors the P2 section of `gears/file-storage/docs/migration.sql`; table
//! names are flat (unqualified) -- consistent with P1 and the `SeaORM` entity
//! `table_name` attributes.
//!
//! @cpt-cf-file-storage-fr-allowed-types-policy
//! @cpt-cf-file-storage-fr-size-limits-policy
//! @cpt-cf-file-storage-fr-metadata-limits
//! @cpt-cf-file-storage-fr-retention-policies
//! @cpt-cf-file-storage-fr-multipart-upload
//! @cpt-cf-file-storage-fr-upload-idempotency
//! @cpt-cf-file-storage-fr-audit-trail
//! @cpt-cf-file-storage-nfr-audit-completeness
//! @cpt-cf-file-storage-fr-file-events

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const POSTGRES_UP: &str = r"
CREATE TABLE IF NOT EXISTS policies (
    policy_id        uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id        uuid         NOT NULL,
    scope            text         NOT NULL  CHECK (scope IN ('tenant', 'user')),
    scope_owner_id   uuid,
    body             jsonb        NOT NULL,
    created_at       timestamptz  NOT NULL  DEFAULT now(),
    updated_at       timestamptz  NOT NULL  DEFAULT now(),
    CHECK ((scope = 'user' AND scope_owner_id IS NOT NULL) OR
           (scope = 'tenant' AND scope_owner_id IS NULL))
);

CREATE INDEX IF NOT EXISTS policies_scope_idx
    ON policies (tenant_id, scope, scope_owner_id);

CREATE TABLE IF NOT EXISTS retention_rules (
    rule_id          uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id        uuid         NOT NULL,
    scope            text         NOT NULL  CHECK (scope IN ('tenant', 'user', 'file')),
    scope_target_id  uuid,
    body             jsonb        NOT NULL,
    created_at       timestamptz  NOT NULL  DEFAULT now(),
    CHECK ((scope = 'tenant' AND scope_target_id IS NULL) OR
           (scope IN ('user', 'file') AND scope_target_id IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS retention_rules_scope_idx
    ON retention_rules (tenant_id, scope, scope_target_id);

CREATE INDEX IF NOT EXISTS retention_rules_file_scope_idx
    ON retention_rules (scope_target_id)
    WHERE scope = 'file';

CREATE TABLE IF NOT EXISTS multipart_uploads (
    upload_id              uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    file_id                uuid         NOT NULL
                                        REFERENCES files (file_id) ON DELETE CASCADE,
    version_id             uuid         NOT NULL,
    backend_upload_handle  text         NOT NULL,
    state                  text         NOT NULL  DEFAULT 'in_progress'
                                        CHECK (state IN ('in_progress', 'completed', 'aborted')),
    declared_mime          text         NOT NULL,
    mime_validated         boolean      NOT NULL  DEFAULT false,
    created_at             timestamptz  NOT NULL  DEFAULT now(),
    expires_at             timestamptz  NOT NULL
);

CREATE INDEX IF NOT EXISTS multipart_uploads_file_idx
    ON multipart_uploads (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_expired_idx
    ON multipart_uploads (expires_at)
    WHERE state = 'in_progress';

CREATE TABLE IF NOT EXISTS multipart_upload_parts (
    upload_id    uuid         NOT NULL
                              REFERENCES multipart_uploads (upload_id) ON DELETE CASCADE,
    part_number  int          NOT NULL  CHECK (part_number > 0),
    backend_etag text         NOT NULL,
    part_hash    bytea        NOT NULL,
    size         bigint       NOT NULL  CHECK (size >= 0),
    uploaded_at  timestamptz  NOT NULL  DEFAULT now(),
    PRIMARY KEY (upload_id, part_number)
);

CREATE TABLE IF NOT EXISTS idempotency_keys (
    tenant_id        uuid         NOT NULL,
    owner_kind       text         NOT NULL  CHECK (owner_kind IN ('user', 'app')),
    owner_id         uuid         NOT NULL,
    idempotency_key  text         NOT NULL,
    file_id          uuid         NOT NULL
                                  REFERENCES files (file_id) ON DELETE CASCADE,
    response_status  int          NOT NULL,
    response_body    text         NOT NULL,
    response_etag    text         NOT NULL,
    created_at       timestamptz  NOT NULL  DEFAULT now(),
    expires_at       timestamptz  NOT NULL,
    PRIMARY KEY (tenant_id, owner_kind, owner_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idempotency_keys_expired_idx
    ON idempotency_keys (expires_at);

CREATE TABLE IF NOT EXISTS audit_outbox (
    event_id      uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id     uuid         NOT NULL,
    actor_kind    text         NOT NULL,
    actor_id      uuid         NOT NULL,
    file_id       uuid,
    operation     text         NOT NULL,
    outcome       text         NOT NULL,
    detail        jsonb        NOT NULL,
    occurred_at   timestamptz  NOT NULL  DEFAULT now(),
    published_at  timestamptz
);

CREATE INDEX IF NOT EXISTS audit_outbox_unpublished_idx
    ON audit_outbox (occurred_at)
    WHERE published_at IS NULL;

CREATE TABLE IF NOT EXISTS events_outbox (
    event_id      uuid         PRIMARY KEY  DEFAULT gen_random_uuid(),
    tenant_id     uuid         NOT NULL,
    owner_id      uuid         NOT NULL,
    file_id       uuid         NOT NULL,
    event_type    text         NOT NULL,
    payload       jsonb        NOT NULL,
    occurred_at   timestamptz  NOT NULL  DEFAULT now(),
    published_at  timestamptz
);

CREATE INDEX IF NOT EXISTS events_outbox_unpublished_idx
    ON events_outbox (occurred_at)
    WHERE published_at IS NULL;
";

const SQLITE_UP: &str = r"
CREATE TABLE IF NOT EXISTS policies (
    policy_id        TEXT  PRIMARY KEY NOT NULL,
    tenant_id        TEXT  NOT NULL,
    scope            TEXT  NOT NULL  CHECK (scope IN ('tenant', 'user')),
    scope_owner_id   TEXT,
    body             TEXT  NOT NULL,
    created_at       TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    updated_at       TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    CHECK ((scope = 'user' AND scope_owner_id IS NOT NULL) OR
           (scope = 'tenant' AND scope_owner_id IS NULL))
);

CREATE INDEX IF NOT EXISTS policies_scope_idx
    ON policies (tenant_id, scope, scope_owner_id);

CREATE TABLE IF NOT EXISTS retention_rules (
    rule_id          TEXT  PRIMARY KEY NOT NULL,
    tenant_id        TEXT  NOT NULL,
    scope            TEXT  NOT NULL  CHECK (scope IN ('tenant', 'user', 'file')),
    scope_target_id  TEXT,
    body             TEXT  NOT NULL,
    created_at       TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    CHECK ((scope = 'tenant' AND scope_target_id IS NULL) OR
           (scope IN ('user', 'file') AND scope_target_id IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS retention_rules_scope_idx
    ON retention_rules (tenant_id, scope, scope_target_id);

CREATE INDEX IF NOT EXISTS retention_rules_file_scope_idx
    ON retention_rules (scope_target_id, scope);

CREATE TABLE IF NOT EXISTS multipart_uploads (
    upload_id              TEXT  PRIMARY KEY NOT NULL,
    file_id                TEXT  NOT NULL
                                 REFERENCES files (file_id) ON DELETE CASCADE,
    version_id             TEXT  NOT NULL,
    backend_upload_handle  TEXT  NOT NULL,
    state                  TEXT  NOT NULL  DEFAULT 'in_progress'
                                 CHECK (state IN ('in_progress', 'completed', 'aborted')),
    declared_mime          TEXT  NOT NULL,
    mime_validated         INTEGER NOT NULL DEFAULT 0,
    created_at             TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    expires_at             TEXT  NOT NULL
);

CREATE INDEX IF NOT EXISTS multipart_uploads_file_idx
    ON multipart_uploads (file_id);
CREATE INDEX IF NOT EXISTS multipart_uploads_expired_idx
    ON multipart_uploads (expires_at, state);

CREATE TABLE IF NOT EXISTS multipart_upload_parts (
    upload_id    TEXT     NOT NULL
                          REFERENCES multipart_uploads (upload_id) ON DELETE CASCADE,
    part_number  INTEGER  NOT NULL CHECK (part_number > 0),
    backend_etag TEXT     NOT NULL,
    part_hash    BLOB     NOT NULL,
    size         INTEGER  NOT NULL CHECK (size >= 0),
    uploaded_at  TEXT     NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (upload_id, part_number)
);

CREATE TABLE IF NOT EXISTS idempotency_keys (
    tenant_id        TEXT  NOT NULL,
    owner_kind       TEXT  NOT NULL CHECK (owner_kind IN ('user', 'app')),
    owner_id         TEXT  NOT NULL,
    idempotency_key  TEXT  NOT NULL,
    file_id          TEXT  NOT NULL
                           REFERENCES files (file_id) ON DELETE CASCADE,
    response_status  INTEGER NOT NULL,
    response_body    TEXT    NOT NULL,
    response_etag    TEXT    NOT NULL,
    created_at       TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at       TEXT    NOT NULL,
    PRIMARY KEY (tenant_id, owner_kind, owner_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idempotency_keys_expired_idx
    ON idempotency_keys (expires_at);

CREATE TABLE IF NOT EXISTS audit_outbox (
    event_id      TEXT  PRIMARY KEY NOT NULL,
    tenant_id     TEXT  NOT NULL,
    actor_kind    TEXT  NOT NULL,
    actor_id      TEXT  NOT NULL,
    file_id       TEXT,
    operation     TEXT  NOT NULL,
    outcome       TEXT  NOT NULL,
    detail        TEXT  NOT NULL,
    occurred_at   TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    published_at  TEXT
);

CREATE INDEX IF NOT EXISTS audit_outbox_unpublished_idx
    ON audit_outbox (occurred_at, published_at);

CREATE TABLE IF NOT EXISTS events_outbox (
    event_id      TEXT  PRIMARY KEY NOT NULL,
    tenant_id     TEXT  NOT NULL,
    owner_id      TEXT  NOT NULL,
    file_id       TEXT  NOT NULL,
    event_type    TEXT  NOT NULL,
    payload       TEXT  NOT NULL,
    occurred_at   TEXT  NOT NULL  DEFAULT CURRENT_TIMESTAMP,
    published_at  TEXT
);

CREATE INDEX IF NOT EXISTS events_outbox_unpublished_idx
    ON events_outbox (occurred_at, published_at);
";

const DOWN: &str = r"
DROP TABLE IF EXISTS events_outbox;
DROP TABLE IF EXISTS audit_outbox;
DROP TABLE IF EXISTS idempotency_keys;
DROP TABLE IF EXISTS multipart_upload_parts;
DROP TABLE IF EXISTS multipart_uploads;
DROP TABLE IF EXISTS retention_rules;
DROP TABLE IF EXISTS policies;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES_UP,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP,
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
        let conn = manager.get_connection();
        match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres | sea_orm::DatabaseBackend::Sqlite => {
                conn.execute_unprepared(DOWN).await?;
                Ok(())
            }
            sea_orm::DatabaseBackend::MySql => Err(DbErr::Custom(
                "file-storage migrations support Postgres and SQLite only".to_owned(),
            )),
        }
    }
}
