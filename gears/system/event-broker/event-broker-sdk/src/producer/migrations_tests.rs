//! Pin the exact producer registration DDL each backend's migration emits.

use super::*;
use sea_orm::{MockDatabase, MockExecResult, Transaction};

async fn logged_up_ddl(backend: DatabaseBackend) -> Vec<Transaction> {
    let db = MockDatabase::new(backend)
        .append_exec_results([MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }])
        .into_connection();
    CreateProducerRegistrationSchema
        .up(&SchemaManager::new(&db))
        .await
        .unwrap();
    db.into_transaction_log()
}

#[tokio::test]
async fn the_mysql_registration_table_stores_uuids_datetimes_and_case_sensitive_keys() {
    assert_eq!(
        logged_up_ddl(DatabaseBackend::MySql).await,
        vec![Transaction::one(Statement::from_string(
            DatabaseBackend::MySql,
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
                    )",
        ))]
    );
}

#[tokio::test]
async fn the_postgres_and_sqlite_registration_tables_size_the_mode_column() {
    assert_eq!(
        logged_up_ddl(DatabaseBackend::Postgres).await,
        vec![Transaction::one(Statement::from_string(
            DatabaseBackend::Postgres,
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
                    )",
        ))]
    );
    assert_eq!(
        logged_up_ddl(DatabaseBackend::Sqlite).await,
        vec![Transaction::one(Statement::from_string(
            DatabaseBackend::Sqlite,
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
                    )",
        ))]
    );
}
