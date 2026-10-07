use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let (uuid, bytes, timestamp) = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => ("UUID", "BYTEA", "TIMESTAMPTZ"),
            sea_orm::DatabaseBackend::Sqlite => ("TEXT", "BLOB", "TEXT"),
            _ => {
                return Err(DbErr::Custom(
                    "durable execution requires PostgreSQL or SQLite".into(),
                ));
            }
        };
        // Separate migration history is supplied by the gear runtime. Payloads
        // contain safe execution input, not user credentials.
        manager.get_connection().execute_unprepared(&format!(r"
CREATE TABLE durable_runs (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 definition TEXT NOT NULL, revision BIGINT NOT NULL, status TEXT NOT NULL,
 journal {bytes} NOT NULL, lease_until {timestamp}, due_at {timestamp},
 created_at {timestamp} NOT NULL, updated_at {timestamp} NOT NULL
);
CREATE INDEX ix_durable_runs_due ON durable_runs(status, due_at);
CREATE INDEX ix_durable_runs_owner ON durable_runs(tenant_id, owner_id);
CREATE TABLE durable_outbox (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_runs(id), generation BIGINT NOT NULL,
 due_at {timestamp} NOT NULL, delivered_at {timestamp},
 UNIQUE(run_id, generation)
);
CREATE INDEX ix_durable_outbox_due ON durable_outbox(delivered_at, due_at);
CREATE TABLE durable_start_keys (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 definition TEXT NOT NULL, key_hash TEXT NOT NULL, input_hash TEXT NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_runs(id), generation BIGINT NOT NULL,
 UNIQUE(tenant_id, owner_id, definition, key_hash)
);
CREATE TABLE durable_coalescing (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 definition TEXT NOT NULL, key_hash TEXT NOT NULL, input_hash TEXT NOT NULL, revision BIGINT NOT NULL,
 active_run_id {uuid} REFERENCES durable_runs(id), successor_run_id {uuid} REFERENCES durable_runs(id),
 requested_generation BIGINT NOT NULL,
 UNIQUE(tenant_id, owner_id, definition, key_hash)
);
")).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "durable journal is forward-only: rolling back would discard pending work".into(),
        ))
    }
}
