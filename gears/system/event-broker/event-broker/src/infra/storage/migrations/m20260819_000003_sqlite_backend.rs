//! The SQLite `EventBrokerBackend`'s tables (eb-single-process-implementation
//! D3/D4): `event_broker_event` (the log) and `event_broker_partition_state`
//! (next-sequence + outbox-retry dedup bookkeeping). SQLite only (decision
//! log entry 7); no other backend branch needed.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE IF NOT EXISTS event_broker_event (
    id             TEXT PRIMARY KEY NOT NULL,
    type_id        TEXT NOT NULL,
    topic          TEXT NOT NULL,
    tenant_id      TEXT NOT NULL,
    source         TEXT NOT NULL,
    subject        TEXT NOT NULL,
    subject_type   TEXT NOT NULL,
    partition_key  TEXT,
    occurred_at    TEXT NOT NULL,
    trace_parent   TEXT,
    data           TEXT NOT NULL,
    partition      INTEGER NOT NULL,
    sequence       INTEGER NOT NULL,
    sequence_time  TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS event_broker_event_topic_partition_sequence_idx
    ON event_broker_event (topic, partition, sequence);

CREATE INDEX IF NOT EXISTS event_broker_event_tenant_idx
    ON event_broker_event (tenant_id);

CREATE TABLE IF NOT EXISTS event_broker_partition_state (
    topic                 TEXT NOT NULL,
    partition             INTEGER NOT NULL,
    next_sequence         INTEGER NOT NULL,
    last_chain_sequence   INTEGER,
    PRIMARY KEY (topic, partition)
);
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS event_broker_partition_state;")
            .await?;
        conn.execute_unprepared("DROP TABLE IF EXISTS event_broker_event;")
            .await?;
        Ok(())
    }
}
