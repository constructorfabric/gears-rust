use toolkit_db::sea_orm_migration::prelude::*;
use toolkit_db::sea_orm_migration::sea_orm::{
    ConnectionTrait, DatabaseBackend, DbErr, Statement, Value,
};

/// Creates the registration table.
///
/// Never change this migration again. Releases v0.1 to v0.3 created one row per
/// registration key, and v0.4 a table keyed per outbox partition with a
/// `VARCHAR(32)` `mode` and, on `MySQL`, `CHAR(36)` producer ids, all under this
/// name; a database that ran one of them never runs this again, so a schema change
/// goes into a new migration, as `m002` does.
struct CreateProducerRegistrationSchema;

impl MigrationName for CreateProducerRegistrationSchema {
    fn name(&self) -> &'static str {
        "m001_create_event_broker_producer_registrations"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateProducerRegistrationSchema {
    // Signature must mirror `MigrationTrait`; spelling `SchemaManager<'_>` changes lifetime binding.
    #[allow(elided_lifetimes_in_paths)]
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_string(
            backend,
            create_registrations(backend)?,
        ))
        .await?;
        Ok(())
    }

    // Signature must mirror `MigrationTrait`; spelling `SchemaManager<'_>` changes lifetime binding.
    #[allow(elided_lifetimes_in_paths)]
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_string(
            backend,
            "DROP TABLE IF EXISTS event_broker_producer_registrations",
        ))
        .await?;
        Ok(())
    }
}

/// The registration table as the SDK reads and writes it. `m001` creates it on an
/// empty database, `m002` over a table of an earlier shape it drops.
fn create_registrations(backend: DatabaseBackend) -> Result<&'static str, DbErr> {
    Ok(match backend {
        DatabaseBackend::Postgres => {
            "CREATE TABLE IF NOT EXISTS event_broker_producer_registrations (
                registration_key VARCHAR(255) NOT NULL,
                queue VARCHAR(255) NOT NULL,
                outbox_partition BIGINT NOT NULL,
                producer_id UUID NOT NULL,
                mode VARCHAR(9) NOT NULL,
                client_agent VARCHAR(256) NOT NULL,
                generation BIGINT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                PRIMARY KEY (registration_key, queue, outbox_partition)
            )"
        }
        DatabaseBackend::Sqlite => {
            "CREATE TABLE IF NOT EXISTS event_broker_producer_registrations (
                registration_key TEXT NOT NULL,
                queue TEXT NOT NULL,
                outbox_partition INTEGER NOT NULL,
                producer_id TEXT NOT NULL,
                mode VARCHAR(9) NOT NULL,
                client_agent TEXT NOT NULL,
                generation INTEGER NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (registration_key, queue, outbox_partition)
            )"
        }
        // The column defaults are only a fallback: the SDK writes
        // `created_at`/`updated_at` itself, in UTC.
        DatabaseBackend::MySql => {
            "CREATE TABLE IF NOT EXISTS event_broker_producer_registrations (
                registration_key VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
                queue VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
                outbox_partition BIGINT NOT NULL,
                producer_id BINARY(16) NOT NULL,
                mode VARCHAR(9) NOT NULL,
                client_agent VARCHAR(256) NOT NULL,
                generation BIGINT NOT NULL,
                created_at DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                updated_at DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                PRIMARY KEY (registration_key, queue, outbox_partition)
            )"
        }
        other => {
            return Err(DbErr::Custom(format!(
                "producer registration schema has no DDL for database backend {other:?}"
            )));
        }
    })
}

/// Brings a registration table an earlier release's `m001` created to the shape
/// [`create_registrations`] makes.
///
/// A table keyed per outbox partition is kept with its rows: on `PostgreSQL` its
/// `mode` column is narrowed to `VARCHAR(9)`, on `SQLite` it already matches. Any
/// other table is dropped and created afresh, because the SDK cannot read its rows:
/// one row per registration key names no queue or outbox partition, and v0.4 bound
/// a producer id to `MySQL`'s `CHAR(36)` as its 16 raw bytes, which that column
/// cannot hold as a readable id, so there is none to convert. A producer whose
/// registration is dropped registers again on its next start, as on an empty
/// database.
struct ConvergeProducerRegistrationSchema;

impl MigrationName for ConvergeProducerRegistrationSchema {
    fn name(&self) -> &'static str {
        "m002_converge_event_broker_producer_registrations"
    }
}

/// Whether the registration table has `column`, of `data_type` when one is given.
async fn has_column(
    conn: &impl ConnectionTrait,
    column: &str,
    data_type: Option<&str>,
) -> Result<bool, DbErr> {
    let backend = conn.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Postgres => {
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = 'event_broker_producer_registrations'
               AND column_name = $1 AND ($2::TEXT IS NULL OR data_type = $2)"
        }
        DatabaseBackend::MySql => {
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = DATABASE()
               AND table_name = 'event_broker_producer_registrations'
               AND column_name = ? AND (? IS NULL OR data_type = ?)"
        }
        DatabaseBackend::Sqlite => {
            "SELECT COUNT(*) FROM pragma_table_info('event_broker_producer_registrations')
             WHERE name = ? AND (? IS NULL OR type = ?)"
        }
        other => {
            return Err(DbErr::Custom(format!(
                "producer registration schema has no DDL for database backend {other:?}"
            )));
        }
    };
    let values: Vec<Value> = match backend {
        DatabaseBackend::Postgres => vec![column.into(), data_type.into()],
        _ => vec![column.into(), data_type.into(), data_type.into()],
    };
    let count: i64 = conn
        .query_one_raw(Statement::from_sql_and_values(backend, sql, values))
        .await?
        .ok_or_else(|| DbErr::Custom("COUNT(*) returned no row".to_owned()))?
        .try_get_by_index(0)?;
    Ok(count > 0)
}

#[async_trait::async_trait]
impl MigrationTrait for ConvergeProducerRegistrationSchema {
    // Signature must mirror `MigrationTrait`; spelling `SchemaManager<'_>` changes lifetime binding.
    #[allow(elided_lifetimes_in_paths)]
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        let readable = has_column(conn, "queue", None).await?
            && (backend != DatabaseBackend::MySql
                || has_column(conn, "producer_id", Some("binary")).await?);
        if readable {
            if backend == DatabaseBackend::Postgres {
                conn.execute_raw(Statement::from_string(
                    backend,
                    "ALTER TABLE event_broker_producer_registrations
                     ALTER COLUMN mode TYPE VARCHAR(9)",
                ))
                .await?;
            }
            return Ok(());
        }
        conn.execute_raw(Statement::from_string(
            backend,
            "DROP TABLE IF EXISTS event_broker_producer_registrations",
        ))
        .await?;
        conn.execute_raw(Statement::from_string(
            backend,
            create_registrations(backend)?,
        ))
        .await?;
        Ok(())
    }

    // Signature must mirror `MigrationTrait`; spelling `SchemaManager<'_>` changes lifetime binding.
    #[allow(elided_lifetimes_in_paths)]
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The earlier shapes are not restored: no release reads them any more.
        Ok(())
    }
}

pub fn producer_registration_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![
        Box::new(CreateProducerRegistrationSchema),
        Box::new(ConvergeProducerRegistrationSchema),
    ]
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
