//! Migration for the `modkit_outbox_events` infrastructure table.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

/// Creates the shared `modkit_outbox_events` table with indexes.
pub struct CreateOutboxTable;

impl MigrationName for CreateOutboxTable {
    fn name(&self) -> &'static str {
        "m001_create_outbox_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateOutboxTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let db_backend = conn.get_database_backend();

        match db_backend {
            DatabaseBackend::Postgres => up_postgres(conn).await,
            DatabaseBackend::Sqlite => up_sqlite(conn).await,
            DatabaseBackend::MySql => Err(DbErr::Custom(
                "Outbox migration is not supported for this database backend".into(),
            )),
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let db_backend = conn.get_database_backend();

        let sql = match db_backend {
            DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
                r#"DROP TABLE IF EXISTS "modkit_outbox_events""#
            }
            DatabaseBackend::MySql => {
                return Err(DbErr::Custom(
                    "Outbox migration is not supported for this database backend".into(),
                ));
            }
        };

        conn.execute(Statement::from_string(db_backend, sql))
            .await?;
        Ok(())
    }
}

async fn up_postgres(conn: &dyn ConnectionTrait) -> Result<(), DbErr> {
    let backend = DatabaseBackend::Postgres;

    // Create the table.
    conn.execute(Statement::from_string(
        backend,
        r#"
        CREATE TABLE IF NOT EXISTS "modkit_outbox_events" (
            id              UUID        PRIMARY KEY,
            namespace       TEXT        NOT NULL,
            topic           TEXT        NOT NULL,
            tenant_id       UUID,
            dedupe_key      TEXT,
            payload         JSONB       NOT NULL,
            status          TEXT        NOT NULL DEFAULT 'pending',
            attempts        INT         NOT NULL DEFAULT 0,
            next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            locked_by       UUID,
            locked_until    TIMESTAMPTZ,
            last_error      TEXT,
            created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    ))
    .await?;

    // Index: dispatcher poll query (namespace-scoped claim).
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE INDEX IF NOT EXISTS "idx_outbox_ns_status_next_attempt"
           ON "modkit_outbox_events" (namespace, status, next_attempt_at)"#,
    ))
    .await?;

    // Index: lease expiry scan.
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE INDEX IF NOT EXISTS "idx_outbox_locked_until"
           ON "modkit_outbox_events" (locked_until)"#,
    ))
    .await?;

    // Partial unique index for dedupe.
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx_outbox_dedupe"
           ON "modkit_outbox_events" (namespace, topic, dedupe_key)
           WHERE dedupe_key IS NOT NULL"#,
    ))
    .await?;

    Ok(())
}

async fn up_sqlite(conn: &dyn ConnectionTrait) -> Result<(), DbErr> {
    let backend = DatabaseBackend::Sqlite;

    conn.execute(Statement::from_string(
        backend,
        r#"
        CREATE TABLE IF NOT EXISTS "modkit_outbox_events" (
            id              TEXT        PRIMARY KEY,
            namespace       TEXT        NOT NULL,
            topic           TEXT        NOT NULL,
            tenant_id       TEXT,
            dedupe_key      TEXT,
            payload         TEXT        NOT NULL,
            status          TEXT        NOT NULL DEFAULT 'pending',
            attempts        INTEGER     NOT NULL DEFAULT 0,
            next_attempt_at TEXT        NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f','now')),
            locked_by       TEXT,
            locked_until    TEXT,
            last_error      TEXT,
            created_at      TEXT        NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f','now')),
            updated_at      TEXT        NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f','now'))
        )
        "#,
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        r#"CREATE INDEX IF NOT EXISTS "idx_outbox_ns_status_next_attempt"
           ON "modkit_outbox_events" (namespace, status, next_attempt_at)"#,
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        r#"CREATE INDEX IF NOT EXISTS "idx_outbox_locked_until"
           ON "modkit_outbox_events" (locked_until)"#,
    ))
    .await?;

    conn.execute(Statement::from_string(
        backend,
        r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx_outbox_dedupe"
           ON "modkit_outbox_events" (namespace, topic, dedupe_key)
           WHERE dedupe_key IS NOT NULL"#,
    ))
    .await?;

    Ok(())
}

/// Return the outbox migration(s) for use with [`run_migrations_for_module`](crate::migration_runner::run_migrations_for_module).
///
/// # Example
///
/// ```ignore
/// use modkit_db::outbox::outbox_migrations;
/// use modkit_db::migration_runner::run_migrations_for_module;
///
/// run_migrations_for_module(&db, "modkit_outbox", outbox_migrations()).await?;
/// ```
#[must_use]
pub fn outbox_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(CreateOutboxTable)]
}

/// Convenience function to set up the outbox infrastructure table.
///
/// Equivalent to `run_migrations_for_module(&db, "modkit_outbox", outbox_migrations())`.
///
/// # Errors
///
/// Returns `MigrationError` if the migration fails.
pub async fn setup_outbox_table(
    db: &crate::Db,
) -> Result<crate::migration_runner::MigrationResult, crate::migration_runner::MigrationError> {
    crate::migration_runner::run_migrations_for_module(db, "modkit_outbox", outbox_migrations())
        .await
}
