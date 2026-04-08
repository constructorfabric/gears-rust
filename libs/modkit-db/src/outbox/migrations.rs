use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};
use sea_orm_migration::prelude::*;

/// Single migration that creates the entire outbox schema from scratch.
///
/// Tables are created in FK dependency order:
/// body → partitions → incoming → outgoing → dead-letters → processor
///
/// # Idempotency
///
/// All `CREATE TABLE` statements use `IF NOT EXISTS` so the migration is safe
/// to re-run (e.g., after a partial failure).  `CREATE INDEX` statements do
/// **not** — `MySQL` has no `IF NOT EXISTS` syntax for indexes, and keeping the
/// Pg/SQLite paths consistent avoids a false sense of safety on only some
/// backends.  Because each index immediately follows its `CREATE TABLE IF NOT
/// EXISTS`, the index can only pre-exist if the migration crashed between the
/// two statements *and* the migration runner retries without rolling back.
/// The sea-orm migration framework tracks completed migrations, so this edge
/// case requires a crash mid-transaction — acceptable for a `preview-outbox`
/// alpha feature with no production deployments.
struct CreateOutboxSchema;

impl MigrationName for CreateOutboxSchema {
    fn name(&self) -> &'static str {
        "m001_create_modkit_outbox_schema"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateOutboxSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();

        create_body(conn, backend).await?;
        create_partitions(conn, backend).await?;
        create_incoming(conn, backend).await?;
        create_outgoing(conn, backend).await?;
        create_dead_letters(conn, backend).await?;
        create_processor(conn, backend).await?;
        create_vacuum_counter(conn, backend).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();

        for table in [
            "modkit_outbox_vacuum_counter",
            "modkit_outbox_processor",
            "modkit_outbox_dead_letters",
            "modkit_outbox_outgoing",
            "modkit_outbox_incoming",
            "modkit_outbox_partitions",
            "modkit_outbox_body",
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

async fn create_body(conn: &dyn ConnectionTrait, backend: DatabaseBackend) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_body (
                id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                payload       BYTEA  NOT NULL,
                payload_type  TEXT   NOT NULL,
                created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_body (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                payload       BLOB   NOT NULL,
                payload_type  TEXT   NOT NULL,
                created_at    TEXT   NOT NULL DEFAULT (datetime('now'))
            )"
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_body (
                id            BIGINT AUTO_INCREMENT PRIMARY KEY,
                payload       LONGBLOB NOT NULL,
                payload_type  TEXT     NOT NULL,
                created_at    TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)
            )"
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_partitions(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_partitions (
                id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                queue     TEXT     NOT NULL,
                partition SMALLINT NOT NULL,
                sequence  BIGINT   NOT NULL DEFAULT 0,
                UNIQUE (queue, partition)
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_partitions (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                queue     TEXT    NOT NULL,
                partition INTEGER NOT NULL,
                sequence  INTEGER NOT NULL DEFAULT 0,
                UNIQUE (queue, partition)
            )"
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_partitions (
                id        BIGINT AUTO_INCREMENT PRIMARY KEY,
                queue     VARCHAR(255) NOT NULL,
                `partition` SMALLINT NOT NULL,
                sequence  BIGINT   NOT NULL DEFAULT 0,
                UNIQUE KEY (queue, `partition`)
            )"
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_incoming(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_incoming (
                id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                partition_id BIGINT   NOT NULL REFERENCES modkit_outbox_partitions(id),
                body_id      BIGINT   NOT NULL REFERENCES modkit_outbox_body(id)
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_incoming (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                partition_id INTEGER NOT NULL REFERENCES modkit_outbox_partitions(id),
                body_id      INTEGER NOT NULL REFERENCES modkit_outbox_body(id)
            )"
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_incoming (
                id           BIGINT AUTO_INCREMENT PRIMARY KEY,
                partition_id BIGINT NOT NULL,
                body_id      BIGINT NOT NULL,
                FOREIGN KEY (partition_id) REFERENCES modkit_outbox_partitions(id),
                FOREIGN KEY (body_id) REFERENCES modkit_outbox_body(id)
            )"
            }
        },
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres | DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                "CREATE INDEX idx_modkit_outbox_incoming_partition \
             ON modkit_outbox_incoming (partition_id, id)"
            }
        },
    ))
    .await?;

    // Index on body_id to accelerate FK constraint checks during
    // DELETE FROM modkit_outbox_body WHERE id IN (...).
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres | DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                "CREATE INDEX idx_modkit_outbox_incoming_body_id \
             ON modkit_outbox_incoming (body_id)"
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_outgoing(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_outgoing (
                id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                partition_id BIGINT NOT NULL REFERENCES modkit_outbox_partitions(id),
                body_id      BIGINT NOT NULL REFERENCES modkit_outbox_body(id),
                seq          BIGINT NOT NULL,
                sequenced_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_outgoing (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                partition_id INTEGER NOT NULL REFERENCES modkit_outbox_partitions(id),
                body_id      INTEGER NOT NULL REFERENCES modkit_outbox_body(id),
                seq          INTEGER NOT NULL,
                sequenced_at TEXT    NOT NULL DEFAULT (datetime('now'))
            )"
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_outgoing (
                id           BIGINT AUTO_INCREMENT PRIMARY KEY,
                partition_id BIGINT NOT NULL,
                body_id      BIGINT NOT NULL,
                seq          BIGINT NOT NULL,
                sequenced_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                FOREIGN KEY (partition_id) REFERENCES modkit_outbox_partitions(id),
                FOREIGN KEY (body_id) REFERENCES modkit_outbox_body(id)
            )"
            }
        },
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres | DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                "CREATE UNIQUE INDEX idx_modkit_outbox_outgoing_partition_seq \
             ON modkit_outbox_outgoing (partition_id, seq)"
            }
        },
    ))
    .await?;

    // Index on body_id to accelerate FK constraint checks during
    // DELETE FROM modkit_outbox_body WHERE id IN (...).
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres | DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                "CREATE INDEX idx_modkit_outbox_outgoing_body_id \
             ON modkit_outbox_outgoing (body_id)"
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_dead_letters(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_dead_letters (
                id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                partition_id BIGINT NOT NULL REFERENCES modkit_outbox_partitions(id),
                seq          BIGINT NOT NULL,
                payload      BYTEA  NOT NULL,
                payload_type TEXT   NOT NULL,
                created_at   TIMESTAMPTZ NOT NULL,
                failed_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
                last_error   TEXT,
                attempts     SMALLINT NOT NULL,
                status       TEXT NOT NULL DEFAULT 'pending',
                completed_at TIMESTAMPTZ,
                deadline     TIMESTAMPTZ
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_dead_letters (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                partition_id INTEGER NOT NULL REFERENCES modkit_outbox_partitions(id),
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
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_dead_letters (
                id           BIGINT AUTO_INCREMENT PRIMARY KEY,
                partition_id BIGINT NOT NULL,
                seq          BIGINT NOT NULL,
                payload      LONGBLOB NOT NULL,
                payload_type TEXT     NOT NULL,
                created_at   TIMESTAMP(6) NOT NULL,
                failed_at    TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                last_error   TEXT,
                attempts     SMALLINT NOT NULL,
                status       VARCHAR(20) NOT NULL DEFAULT 'pending',
                completed_at TIMESTAMP(6) NULL,
                deadline     TIMESTAMP(6) NULL,
                FOREIGN KEY (partition_id) REFERENCES modkit_outbox_partitions(id)
            )"
            }
        },
    ))
    .await?;

    // Index for replay query (status = 'pending' OR (status = 'reprocessing' AND deadline < now()))
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE INDEX idx_modkit_outbox_dl_replayable \
             ON modkit_outbox_dead_letters (status, deadline) \
             WHERE status IN ('pending', 'reprocessing')"
            }
            DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                "CREATE INDEX idx_modkit_outbox_dl_status_deadline \
             ON modkit_outbox_dead_letters (status, deadline)"
            }
        },
    ))
    .await?;

    // Index for list queries with status filter + ORDER BY failed_at DESC
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE INDEX idx_modkit_outbox_dl_status_failed \
             ON modkit_outbox_dead_letters (status, failed_at DESC)"
            }
            DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
                "CREATE INDEX idx_modkit_outbox_dl_status_failed \
             ON modkit_outbox_dead_letters (status, failed_at)"
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_processor(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_processor (
                partition_id  BIGINT PRIMARY KEY REFERENCES modkit_outbox_partitions(id),
                processed_seq BIGINT   NOT NULL DEFAULT 0,
                attempts      SMALLINT NOT NULL DEFAULT 0,
                last_error    TEXT,
                locked_by     TEXT,
                locked_until  TIMESTAMPTZ
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_processor (
                partition_id  INTEGER PRIMARY KEY REFERENCES modkit_outbox_partitions(id),
                processed_seq INTEGER NOT NULL DEFAULT 0,
                attempts      INTEGER NOT NULL DEFAULT 0,
                last_error    TEXT,
                locked_by     TEXT,
                locked_until  TEXT
            )"
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_processor (
                partition_id  BIGINT PRIMARY KEY,
                processed_seq BIGINT   NOT NULL DEFAULT 0,
                attempts      SMALLINT NOT NULL DEFAULT 0,
                last_error    TEXT,
                locked_by     TEXT,
                locked_until  TIMESTAMP(6) NULL,
                FOREIGN KEY (partition_id) REFERENCES modkit_outbox_partitions(id)
            )"
            }
        },
    ))
    .await?;
    Ok(())
}

async fn create_vacuum_counter(
    conn: &dyn ConnectionTrait,
    backend: DatabaseBackend,
) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        backend,
        match backend {
            DatabaseBackend::Postgres => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_vacuum_counter (
                partition_id BIGINT PRIMARY KEY
                    REFERENCES modkit_outbox_partitions(id),
                counter      BIGINT NOT NULL DEFAULT 0
            )"
            }
            DatabaseBackend::Sqlite => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_vacuum_counter (
                partition_id INTEGER PRIMARY KEY
                    REFERENCES modkit_outbox_partitions(id),
                counter      INTEGER NOT NULL DEFAULT 0
            )"
            }
            DatabaseBackend::MySql => {
                "CREATE TABLE IF NOT EXISTS modkit_outbox_vacuum_counter (
                partition_id BIGINT PRIMARY KEY,
                counter      BIGINT NOT NULL DEFAULT 0,
                FOREIGN KEY (partition_id) REFERENCES modkit_outbox_partitions(id)
            )"
            }
        },
    ))
    .await?;
    Ok(())
}

/// Returns all outbox migrations in dependency order.
#[must_use]
pub fn outbox_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(CreateOutboxSchema)]
}
