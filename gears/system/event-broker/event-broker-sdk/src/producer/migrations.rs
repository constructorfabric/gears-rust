use toolkit_db::sea_orm_migration::prelude::*;
use toolkit_db::sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

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
            match backend {
                DatabaseBackend::Postgres => {
                    "CREATE TABLE IF NOT EXISTS event_broker_producer_registrations (
                        registration_key VARCHAR(255) NOT NULL,
                        queue VARCHAR(255) NOT NULL,
                        outbox_partition BIGINT NOT NULL,
                        producer_id UUID NOT NULL,
                        mode VARCHAR(32) NOT NULL,
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
                        mode TEXT NOT NULL,
                        client_agent TEXT NOT NULL,
                        generation INTEGER NOT NULL,
                        created_at TEXT NOT NULL DEFAULT (datetime('now')),
                        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                        PRIMARY KEY (registration_key, queue, outbox_partition)
                    )"
                }
                DatabaseBackend::MySql => {
                    "CREATE TABLE IF NOT EXISTS event_broker_producer_registrations (
                        registration_key VARCHAR(255) NOT NULL,
                        queue VARCHAR(255) NOT NULL,
                        outbox_partition BIGINT NOT NULL,
                        producer_id CHAR(36) NOT NULL,
                        mode VARCHAR(32) NOT NULL,
                        client_agent VARCHAR(256) NOT NULL,
                        generation BIGINT NOT NULL,
                        created_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                        updated_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
                        PRIMARY KEY (registration_key, queue, outbox_partition)
                    )"
                }
                other => {
                    return Err(DbErr::Custom(format!(
                        "producer registration schema has no DDL for database backend {other:?}"
                    )));
                }
            },
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

pub fn producer_registration_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(CreateProducerRegistrationSchema)]
}
