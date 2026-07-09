use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};
use sea_orm_migration::prelude::*;

use super::tables::OutboxTables;
use super::types::OutboxError;

/// Single migration that creates the entire outbox schema from scratch.
///
/// Tables are created in FK dependency order:
/// body -> partitions -> incoming -> outgoing -> dead-letters -> processor
///
/// # Idempotency
///
/// All `CREATE TABLE` statements use `IF NOT EXISTS` so the migration is safe
/// to re-run (e.g., after a partial failure). `CREATE INDEX` statements do
/// **not** -- `MySQL` has no `IF NOT EXISTS` syntax for indexes, and keeping the
/// Pg/SQLite paths consistent avoids a false sense of safety on only some
/// backends. Because each index immediately follows its `CREATE TABLE IF NOT
/// EXISTS`, the index can only pre-exist if the migration crashed between the
/// two statements *and* the migration runner retries without rolling back.
/// The sea-orm migration framework tracks completed migrations, so this edge
/// case requires a crash mid-transaction -- acceptable for a `preview-outbox`
/// alpha feature with no production deployments.
struct CreateOutboxSchema {
    tables: OutboxTables,
}

impl CreateOutboxSchema {
    fn new(tables: OutboxTables) -> Self {
        Self { tables }
    }
}

impl Default for CreateOutboxSchema {
    fn default() -> Self {
        Self::new(OutboxTables::default())
    }
}

impl MigrationName for CreateOutboxSchema {
    fn name(&self) -> &str {
        self.tables.migration_name()
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateOutboxSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        let tables = &self.tables;

        create_body(conn, backend, tables).await?;
        create_partitions(conn, backend, tables).await?;
        create_incoming(conn, backend, tables).await?;
        create_outgoing(conn, backend, tables).await?;
        create_dead_letters(conn, backend, tables).await?;
        create_processor(conn, backend, tables).await?;
        create_vacuum_counter(conn, backend, tables).await?;
        create_mysql_id_sequence_tables(conn, backend, tables).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();

        for table in [
            self.tables.incoming_id_sequence(),
            self.tables.body_id_sequence(),
            self.tables.vacuum_counter(),
            self.tables.processor(),
            self.tables.dead_letters(),
            self.tables.outgoing(),
            self.tables.incoming(),
            self.tables.partitions(),
            self.tables.body(),
        ] {
            conn.execute(Statement::from_string(
                backend,
                format!("DROP TABLE IF EXISTS {table}"),
            ))
            .await?;
        }
        Ok(())
    }
}

async fn create_body(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let body_table = tables.body();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {body_table} (
                id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                payload       BYTEA  NOT NULL,
                payload_type  VARCHAR(1024) NOT NULL,
                created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {body_table} (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                payload       BLOB   NOT NULL,
                payload_type  TEXT   NOT NULL,
                created_at    TEXT   NOT NULL DEFAULT (datetime('now'))
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {body_table} (
                id            BIGINT PRIMARY KEY,
                payload       LONGBLOB NOT NULL,
                payload_type  VARCHAR(1024) NOT NULL,
                created_at    TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)
            )"
                )
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_partitions(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let partitions = tables.partitions();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {partitions} (
                id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                queue     VARCHAR(1024) NOT NULL,
                partition SMALLINT NOT NULL,
                sequence  BIGINT   NOT NULL DEFAULT 0,
                UNIQUE (queue, partition)
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {partitions} (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                queue     TEXT    NOT NULL,
                partition INTEGER NOT NULL,
                sequence  INTEGER NOT NULL DEFAULT 0,
                UNIQUE (queue, partition)
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {partitions} (
                id        BIGINT AUTO_INCREMENT PRIMARY KEY,
                queue     VARCHAR(1024) CHARACTER SET ascii NOT NULL,
                `partition` SMALLINT NOT NULL,
                sequence  BIGINT   NOT NULL DEFAULT 0,
                UNIQUE KEY (queue, `partition`)
            )"
                )
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_incoming(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let incoming = tables.incoming();
    let partitions = tables.partitions();
    let body_table = tables.body();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {incoming} (
                id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                partition_id BIGINT   NOT NULL REFERENCES {partitions}(id),
                body_id      BIGINT   NOT NULL REFERENCES {body_table}(id)
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {incoming} (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                partition_id INTEGER NOT NULL REFERENCES {partitions}(id),
                body_id      INTEGER NOT NULL REFERENCES {body_table}(id)
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {incoming} (
                id           BIGINT PRIMARY KEY,
                partition_id BIGINT NOT NULL,
                body_id      BIGINT NOT NULL,
                FOREIGN KEY (partition_id) REFERENCES {partitions}(id),
                FOREIGN KEY (body_id) REFERENCES {body_table}(id)
            )"
                )
            }
        },
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        format!(
            "CREATE INDEX {} ON {} (partition_id, id)",
            tables.idx_incoming_partition(),
            tables.incoming()
        ),
    ))
    .await?;

    // Index on body_id to accelerate FK constraint checks during
    // DELETE FROM outbox body WHERE id IN (...).
    conn.execute(Statement::from_string(
        backend,
        format!(
            "CREATE INDEX {} ON {} (body_id)",
            tables.idx_incoming_body_id(),
            tables.incoming()
        ),
    ))
    .await?;
    Ok(())
}

async fn create_outgoing(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let outgoing = tables.outgoing();
    let partitions = tables.partitions();
    let body_table = tables.body();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {outgoing} (
                id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                partition_id BIGINT NOT NULL REFERENCES {partitions}(id),
                body_id      BIGINT NOT NULL REFERENCES {body_table}(id),
                seq          BIGINT NOT NULL,
                sequenced_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {outgoing} (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                partition_id INTEGER NOT NULL REFERENCES {partitions}(id),
                body_id      INTEGER NOT NULL REFERENCES {body_table}(id),
                seq          INTEGER NOT NULL,
                sequenced_at TEXT    NOT NULL DEFAULT (datetime('now'))
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {outgoing} (
                id           BIGINT AUTO_INCREMENT PRIMARY KEY,
                partition_id BIGINT NOT NULL,
                body_id      BIGINT NOT NULL,
                seq          BIGINT NOT NULL,
                sequenced_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                FOREIGN KEY (partition_id) REFERENCES {partitions}(id),
                FOREIGN KEY (body_id) REFERENCES {body_table}(id)
            )"
                )
            }
        },
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        format!(
            "CREATE UNIQUE INDEX {} ON {} (partition_id, seq)",
            tables.idx_outgoing_partition_seq(),
            tables.outgoing()
        ),
    ))
    .await?;

    // Index on body_id to accelerate FK constraint checks during
    // DELETE FROM outbox body WHERE id IN (...).
    conn.execute(Statement::from_string(
        backend,
        format!(
            "CREATE INDEX {} ON {} (body_id)",
            tables.idx_outgoing_body_id(),
            tables.outgoing()
        ),
    ))
    .await?;
    Ok(())
}

async fn create_dead_letters(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let dead_letters = tables.dead_letters();
    let partitions = tables.partitions();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {dead_letters} (
                id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                partition_id BIGINT NOT NULL REFERENCES {partitions}(id),
                seq          BIGINT NOT NULL,
                payload      BYTEA  NOT NULL,
                payload_type VARCHAR(1024) NOT NULL,
                created_at   TIMESTAMPTZ NOT NULL,
                failed_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
                last_error   TEXT,
                attempts     SMALLINT NOT NULL,
                status       VARCHAR(32) NOT NULL DEFAULT 'pending',
                completed_at TIMESTAMPTZ,
                deadline     TIMESTAMPTZ
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {dead_letters} (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                partition_id INTEGER NOT NULL REFERENCES {partitions}(id),
                seq          INTEGER NOT NULL,
                payload      BLOB   NOT NULL,
                payload_type TEXT   NOT NULL,
                created_at   TEXT    NOT NULL,
                failed_at    TEXT    NOT NULL DEFAULT (datetime('now')),
                last_error   TEXT,
                attempts     INTEGER NOT NULL,
                status       TEXT    NOT NULL DEFAULT 'pending',
                completed_at TEXT,
                deadline     TEXT
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {dead_letters} (
                id           BIGINT AUTO_INCREMENT PRIMARY KEY,
                partition_id BIGINT NOT NULL,
                seq          BIGINT NOT NULL,
                payload      LONGBLOB NOT NULL,
                payload_type VARCHAR(1024) CHARACTER SET ascii NOT NULL,
                created_at   TIMESTAMP(6) NOT NULL,
                failed_at    TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                last_error   TEXT,
                attempts     SMALLINT NOT NULL,
                status       VARCHAR(32) NOT NULL DEFAULT 'pending',
                completed_at TIMESTAMP(6) NULL,
                deadline     TIMESTAMP(6) NULL,
                FOREIGN KEY (partition_id) REFERENCES {partitions}(id)
            )"
                )
            }
        },
    ))
    .await?;

    // Index for replay query (status = 'pending' OR (status = 'reprocessing' AND deadline < now()))
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE INDEX {} ON {} (status, deadline) WHERE status IN ('pending', 'reprocessing')",
                    tables.idx_dl_replayable(),
                    tables.dead_letters()
                )
            }
            DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                format!(
                    "CREATE INDEX {} ON {} (status, deadline)",
                    tables.idx_dl_status_deadline(),
                    tables.dead_letters()
                )
            }
        },
    ))
    .await?;

    // Index for list queries with status filter + ORDER BY failed_at DESC
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE INDEX {} ON {} (status, failed_at DESC)",
                    tables.idx_dl_status_failed(),
                    tables.dead_letters()
                )
            }
            DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                format!(
                    "CREATE INDEX {} ON {} (status, failed_at)",
                    tables.idx_dl_status_failed(),
                    tables.dead_letters()
                )
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_processor(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let processor = tables.processor();
    let partitions = tables.partitions();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {processor} (
                partition_id  BIGINT PRIMARY KEY REFERENCES {partitions}(id),
                processed_seq BIGINT   NOT NULL DEFAULT 0,
                attempts      SMALLINT NOT NULL DEFAULT 0,
                last_error    TEXT,
                locked_by     VARCHAR(1024),
                locked_until  TIMESTAMPTZ
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {processor} (
                partition_id  INTEGER PRIMARY KEY REFERENCES {partitions}(id),
                processed_seq INTEGER NOT NULL DEFAULT 0,
                attempts      INTEGER NOT NULL DEFAULT 0,
                last_error    TEXT,
                locked_by     TEXT,
                locked_until  TEXT
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {processor} (
                partition_id  BIGINT PRIMARY KEY,
                processed_seq BIGINT   NOT NULL DEFAULT 0,
                attempts      SMALLINT NOT NULL DEFAULT 0,
                last_error    TEXT,
                locked_by     VARCHAR(1024) CHARACTER SET ascii,
                locked_until  TIMESTAMP(6) NULL,
                FOREIGN KEY (partition_id) REFERENCES {partitions}(id)
            )"
                )
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_vacuum_counter(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    let vacuum_counter = tables.vacuum_counter();
    let partitions = tables.partitions();
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {vacuum_counter} (
                partition_id BIGINT PRIMARY KEY
                    REFERENCES {partitions}(id),
                counter      BIGINT NOT NULL DEFAULT 0
            )"
                )
            }
            DatabaseBackend::Sqlite => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {vacuum_counter} (
                partition_id INTEGER PRIMARY KEY
                    REFERENCES {partitions}(id),
                counter      INTEGER NOT NULL DEFAULT 0
            )"
                )
            }
            DatabaseBackend::MySql => {
                format!(
                    "CREATE TABLE IF NOT EXISTS {vacuum_counter} (
                partition_id BIGINT PRIMARY KEY,
                counter      BIGINT NOT NULL DEFAULT 0,
                FOREIGN KEY (partition_id) REFERENCES {partitions}(id)
            )"
                )
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_mysql_id_sequence_tables(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
    tables: &OutboxTables,
) -> Result<(), DbErr> {
    if backend != DatabaseBackend::MySql {
        return Ok(());
    }

    for table in [tables.body_id_sequence(), tables.incoming_id_sequence()] {
        conn.execute(Statement::from_string(
            backend,
            mysql_create_id_sequence_sql(table),
        ))
        .await?;
        conn.execute(Statement::from_string(
            backend,
            mysql_seed_id_sequence_sql(table),
        ))
        .await?;
    }

    Ok(())
}

fn mysql_create_id_sequence_sql(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
                slot    TINYINT PRIMARY KEY,
                next_id BIGINT NOT NULL
            )"
    )
}

fn mysql_seed_id_sequence_sql(table: &str) -> String {
    format!("INSERT IGNORE INTO {table} (slot, next_id) VALUES (1, 1)")
}

/// Returns all outbox migrations in dependency order.
#[must_use]
pub fn outbox_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(CreateOutboxSchema::default())]
}

/// Returns all outbox migrations for a custom table prefix in dependency order.
///
/// # Errors
///
/// Returns [`OutboxError::InvalidTablePrefix`] if the prefix cannot be used as
/// a portable SQL identifier.
pub fn outbox_migrations_with_prefix(
    prefix: impl Into<String>,
) -> Result<Vec<Box<dyn MigrationTrait>>, OutboxError> {
    let tables = OutboxTables::new(prefix)?;
    Ok(vec![Box::new(CreateOutboxSchema::new(tables))])
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::outbox::tables::DEFAULT_OUTBOX_MIGRATION_NAME;

    #[test]
    fn default_migration_name_is_preserved() {
        let migrations = outbox_migrations();

        assert_eq!(migrations[0].name(), DEFAULT_OUTBOX_MIGRATION_NAME);
    }

    #[test]
    fn prefixed_migration_name_is_deterministic_and_distinct() {
        let first = outbox_migrations_with_prefix("mini_chat_outbox").unwrap();
        let second = outbox_migrations_with_prefix("mini_chat_outbox").unwrap();

        assert_eq!(first[0].name(), second[0].name());
        assert_ne!(first[0].name(), DEFAULT_OUTBOX_MIGRATION_NAME);
        assert_eq!(
            first[0].name(),
            "m001_create_toolkit_outbox_schema__mini_chat_outbox"
        );
    }

    #[test]
    fn invalid_prefix_is_rejected() {
        let Err(err) = outbox_migrations_with_prefix("public.outbox") else {
            panic!("expected invalid prefix error")
        };

        assert!(
            matches!(err, OutboxError::InvalidTablePrefix(prefix) if prefix == "public.outbox")
        );
    }

    #[test]
    fn default_and_prefixed_migration_names_do_not_collide() {
        let default = outbox_migrations();
        let prefixed = outbox_migrations_with_prefix("mini_chat_outbox").unwrap();

        assert_ne!(default[0].name(), prefixed[0].name());
    }

    #[test]
    fn mysql_sequence_table_sql_uses_custom_prefix() {
        let tables = OutboxTables::new("mini_chat_outbox").unwrap();

        let body_create = mysql_create_id_sequence_sql(tables.body_id_sequence());
        let incoming_create = mysql_create_id_sequence_sql(tables.incoming_id_sequence());
        let body_seed = mysql_seed_id_sequence_sql(tables.body_id_sequence());
        let incoming_seed = mysql_seed_id_sequence_sql(tables.incoming_id_sequence());

        assert!(body_create.contains("mini_chat_outbox_body_id_sequence"));
        assert!(incoming_create.contains("mini_chat_outbox_incoming_id_sequence"));
        assert!(body_seed.contains("mini_chat_outbox_body_id_sequence"));
        assert!(incoming_seed.contains("mini_chat_outbox_incoming_id_sequence"));
        assert!(!body_create.contains("toolkit_outbox_body_id_sequence"));
        assert!(!incoming_create.contains("toolkit_outbox_incoming_id_sequence"));
    }

    #[test]
    fn mysql_sequence_table_sql_has_singleton_primary_key_and_seed() {
        let tables = OutboxTables::default();

        let create = mysql_create_id_sequence_sql(tables.body_id_sequence());
        let seed = mysql_seed_id_sequence_sql(tables.body_id_sequence());

        assert!(create.contains("slot    TINYINT PRIMARY KEY"));
        assert!(create.contains("next_id BIGINT NOT NULL"));
        assert_eq!(
            seed,
            "INSERT IGNORE INTO toolkit_outbox_body_id_sequence (slot, next_id) VALUES (1, 1)"
        );
    }
}
