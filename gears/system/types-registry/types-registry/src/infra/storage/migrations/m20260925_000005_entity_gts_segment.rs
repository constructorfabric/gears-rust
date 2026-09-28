//! Materialize parsed `gts_id` segments for exact SQL discovery filters:
//! `entity.chain_depth`, one `entity_gts_segment` row per segment, and the
//! `gts_id`-ordered indexes for `depth=1`, `kind` + `lifecycle_status`, and
//! `lifecycle_status` alone.
//!
//! No backfill: both are derived at admission, so `up` refuses while `entity`
//! holds a row and changes nothing. `SQLite` cannot add a `NOT NULL` column
//! without a default, so its CHECK rejects NULL instead.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ENTITY_TABLE: &str = "types_registry__entity";

const PG_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__entity
        ADD COLUMN chain_depth smallint NOT NULL
        CONSTRAINT ck_tr_entity_chain_depth CHECK (chain_depth >= 1)",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_depth
        ON types_registry__entity (chain_depth, gts_id)",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_kind_lifecycle
        ON types_registry__entity (entity_kind, lifecycle_status, gts_id)",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_lifecycle
        ON types_registry__entity (lifecycle_status, gts_id)",
    "CREATE TABLE IF NOT EXISTS types_registry__entity_gts_segment (
        entity_id     bigint        NOT NULL,
        segment_no    smallint      NOT NULL,
        segment_name  varchar(1024) COLLATE \"C\" NOT NULL,
        major         bigint        NOT NULL,
        minor         bigint        NULL,
        is_type       boolean       NOT NULL,

        CONSTRAINT pk_tr_entity_gts_segment PRIMARY KEY (entity_id, segment_no),
        CONSTRAINT fk_tr_entity_gts_segment_entity
            FOREIGN KEY (entity_id)
            REFERENCES types_registry__entity (id) ON DELETE CASCADE,
        CONSTRAINT ck_tr_entity_gts_segment_no CHECK (segment_no >= 0),
        CONSTRAINT ck_tr_entity_gts_segment_version
            CHECK (major >= 0 AND (minor IS NULL OR minor >= 0))
    )",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_gts_segment_lookup
        ON types_registry__entity_gts_segment (
            segment_no, segment_name, major, is_type, minor, entity_id
        )",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__entity
        ADD COLUMN chain_depth SMALLINT
        CONSTRAINT ck_tr_entity_chain_depth
            CHECK (chain_depth IS NOT NULL AND chain_depth >= 1)",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_depth
        ON types_registry__entity (chain_depth, gts_id)",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_kind_lifecycle
        ON types_registry__entity (entity_kind, lifecycle_status, gts_id)",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_lifecycle
        ON types_registry__entity (lifecycle_status, gts_id)",
    "CREATE TABLE IF NOT EXISTS types_registry__entity_gts_segment (
        entity_id     BIGINT   NOT NULL,
        segment_no    SMALLINT NOT NULL,
        segment_name  TEXT     COLLATE BINARY NOT NULL,
        major         BIGINT   NOT NULL,
        minor         BIGINT   NULL,
        is_type       INTEGER  NOT NULL,

        CONSTRAINT pk_tr_entity_gts_segment PRIMARY KEY (entity_id, segment_no),
        CONSTRAINT fk_tr_entity_gts_segment_entity
            FOREIGN KEY (entity_id)
            REFERENCES types_registry__entity (id) ON DELETE CASCADE,
        CONSTRAINT ck_tr_entity_gts_segment_no CHECK (segment_no >= 0),
        CONSTRAINT ck_tr_entity_gts_segment_version
            CHECK (major >= 0 AND (minor IS NULL OR minor >= 0)),
        CONSTRAINT ck_tr_entity_gts_segment_is_type CHECK (is_type IN (0, 1))
    )",
    "CREATE INDEX IF NOT EXISTS idx_tr_entity_gts_segment_lookup
        ON types_registry__entity_gts_segment (
            segment_no, segment_name, major, is_type, minor, entity_id
        )",
];

const MYSQL_UP_STATEMENTS: &[&str] = &[
    "ALTER TABLE types_registry__entity
        ADD COLUMN chain_depth SMALLINT NOT NULL,
        ADD CONSTRAINT ck_tr_entity_chain_depth CHECK (chain_depth >= 1),
        ADD KEY idx_tr_entity_depth (chain_depth, gts_id),
        ADD KEY idx_tr_entity_kind_lifecycle (entity_kind, lifecycle_status, gts_id),
        ADD KEY idx_tr_entity_lifecycle (lifecycle_status, gts_id)",
    "CREATE TABLE IF NOT EXISTS types_registry__entity_gts_segment (
        entity_id     BIGINT        NOT NULL,
        segment_no    SMALLINT      NOT NULL,
        segment_name  VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
        major         BIGINT        NOT NULL,
        minor         BIGINT        NULL,
        is_type       TINYINT(1)    NOT NULL,

        CONSTRAINT pk_tr_entity_gts_segment PRIMARY KEY (entity_id, segment_no),
        KEY idx_tr_entity_gts_segment_lookup (
            segment_no, segment_name, major, is_type, minor, entity_id
        ),
        CONSTRAINT fk_tr_entity_gts_segment_entity
            FOREIGN KEY (entity_id)
            REFERENCES types_registry__entity (id) ON DELETE CASCADE,
        CONSTRAINT ck_tr_entity_gts_segment_no CHECK (segment_no >= 0),
        CONSTRAINT ck_tr_entity_gts_segment_version
            CHECK (major >= 0 AND (minor IS NULL OR minor >= 0)),
        CONSTRAINT ck_tr_entity_gts_segment_is_type CHECK (is_type IN (0, 1))
    )",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS types_registry__entity_gts_segment",
    "DROP INDEX IF EXISTS idx_tr_entity_lifecycle",
    "DROP INDEX IF EXISTS idx_tr_entity_kind_lifecycle",
    "DROP INDEX IF EXISTS idx_tr_entity_depth",
    "ALTER TABLE types_registry__entity DROP COLUMN chain_depth",
];

// SQLite drops a column only once no index names it.
const SQLITE_DOWN_STATEMENTS: &[&str] = PG_DOWN_STATEMENTS;

const MYSQL_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS types_registry__entity_gts_segment",
    "ALTER TABLE types_registry__entity
        DROP CHECK ck_tr_entity_chain_depth,
        DROP INDEX idx_tr_entity_lifecycle,
        DROP INDEX idx_tr_entity_kind_lifecycle,
        DROP INDEX idx_tr_entity_depth,
        DROP COLUMN chain_depth",
];

fn unsupported(other: sea_orm::DatabaseBackend) -> DbErr {
    DbErr::Migration(format!(
        "types-registry migrations support Postgres, SQLite and MySQL only; \
         got unsupported database backend {other:?}"
    ))
}

fn up_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(PG_UP_STATEMENTS),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_UP_STATEMENTS),
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_UP_STATEMENTS),
        other => Err(unsupported(other)),
    }
}

fn down_statements(backend: sea_orm::DatabaseBackend) -> Result<&'static [&'static str], DbErr> {
    match backend {
        sea_orm::DatabaseBackend::Postgres => Ok(PG_DOWN_STATEMENTS),
        sea_orm::DatabaseBackend::Sqlite => Ok(SQLITE_DOWN_STATEMENTS),
        sea_orm::DatabaseBackend::MySql => Ok(MYSQL_DOWN_STATEMENTS),
        other => Err(unsupported(other)),
    }
}

#[cfg(test)]
#[path = "m20260925_000005_entity_gts_segment_tests.rs"]
mod entity_gts_segment_tests;

#[allow(elided_lifetimes_in_paths)]
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements = up_statements(backend)?;
        let probe = format!("SELECT 1 AS present FROM {ENTITY_TABLE} LIMIT 1");
        if conn
            .query_one_raw(Statement::from_string(backend, probe))
            .await?
            .is_some()
        {
            return Err(DbErr::Migration(format!(
                "cannot materialize GTS segments: {ENTITY_TABLE} has rows and this \
                 migration has no backfill; recreate the pre-release database"
            )));
        }
        for sql in statements {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        for sql in down_statements(backend)? {
            conn.execute_raw(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
