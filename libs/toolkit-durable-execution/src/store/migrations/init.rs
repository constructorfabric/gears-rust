//! The journal schema. The stored format is unreleased, so the journal tables are
//! created in one forward-only step; the delivery outbox has its own migrations.
use sea_orm_migration::prelude::*;

/// Creates the journal tables.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &'static str {
        "m0001_durable_execution__init"
    }
}

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
        // Payloads contain safe execution input, not user credentials.
        manager
            .get_connection()
            .execute_unprepared(&format!(
                r"
CREATE TABLE durable_execution__runs (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 definition TEXT NOT NULL, fingerprint TEXT NOT NULL, revision BIGINT NOT NULL, status TEXT NOT NULL,
 progress {bytes} NOT NULL, progress_status TEXT NOT NULL,
 journal {bytes} NOT NULL, lease_until {timestamp}, due_at {timestamp},
 created_at {timestamp} NOT NULL, updated_at {timestamp} NOT NULL,
 storage_version INTEGER NOT NULL DEFAULT 1, activity_count INTEGER NOT NULL DEFAULT 0,
 registration_generation BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX durable_execution__runs_due_ix ON durable_execution__runs(status, due_at);
CREATE INDEX durable_execution__runs_owner_ix ON durable_execution__runs(tenant_id, owner_id);
CREATE INDEX durable_execution__runs_generation_ix
 ON durable_execution__runs(definition, registration_generation, status);
CREATE TABLE durable_execution__delivery_intents (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_execution__runs(id), generation BIGINT NOT NULL,
 due_at {timestamp} NOT NULL, delivered_at {timestamp},
 forwarded BOOLEAN NOT NULL DEFAULT FALSE,
 UNIQUE(run_id, generation)
);
CREATE INDEX durable_execution__delivery_intents_due_ix
 ON durable_execution__delivery_intents(delivered_at, due_at);
CREATE TABLE durable_execution__start_keys (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 definition TEXT NOT NULL, key_hash TEXT NOT NULL, input_hash TEXT NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_execution__runs(id), generation BIGINT NOT NULL,
 UNIQUE(tenant_id, owner_id, definition, key_hash)
);
CREATE TABLE durable_execution__coalescing (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 definition TEXT NOT NULL, key_hash TEXT NOT NULL, input_hash TEXT NOT NULL, revision BIGINT NOT NULL,
 active_run_id {uuid} REFERENCES durable_execution__runs(id),
 successor_run_id {uuid} REFERENCES durable_execution__runs(id),
 requested_generation BIGINT NOT NULL,
 UNIQUE(tenant_id, owner_id, definition, key_hash)
);
CREATE TABLE durable_execution__activities (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_execution__runs(id), activity_id TEXT NOT NULL,
 template_id TEXT NOT NULL, item_index INTEGER CHECK(item_index >= 0),
 phase TEXT NOT NULL CHECK(phase IN ('forward', 'compensation')),
 position INTEGER NOT NULL, stage INTEGER, epoch BIGINT NOT NULL, status TEXT NOT NULL,
 fence BIGINT NOT NULL, lease_until {timestamp}, due_at {timestamp}, state {bytes} NOT NULL,
 UNIQUE(run_id, activity_id), UNIQUE(run_id, position)
);
CREATE INDEX durable_execution__activities_claims_ix
 ON durable_execution__activities(run_id, stage, phase, status);
CREATE INDEX durable_execution__activities_lease_ix
 ON durable_execution__activities(status, lease_until);
CREATE TABLE durable_execution__attempts (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_execution__runs(id), activity_id TEXT NOT NULL,
 number BIGINT NOT NULL, epoch BIGINT NOT NULL, state {bytes} NOT NULL,
 UNIQUE(run_id, activity_id, number)
);
CREATE TABLE durable_execution__epochs (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_execution__runs(id), epoch BIGINT NOT NULL,
 state {bytes} NOT NULL,
 UNIQUE(run_id, epoch)
);
CREATE TABLE durable_execution__events (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_execution__runs(id), sequence BIGINT NOT NULL,
 state {bytes} NOT NULL,
 UNIQUE(run_id, sequence)
);
CREATE TABLE durable_execution__definitions (
 id {uuid} PRIMARY KEY, name TEXT NOT NULL UNIQUE, revision BIGINT NOT NULL, state {bytes} NOT NULL
);
"
            ))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "durable journal is forward-only: rolling back would discard pending work".into(),
        ))
    }
}
