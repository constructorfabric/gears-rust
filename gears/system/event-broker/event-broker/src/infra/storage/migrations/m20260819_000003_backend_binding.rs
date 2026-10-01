//! The ingest side's bookkeeping toward its storage backends: the identity
//! every ingest outbox fences its appends with (feature 0005 §2.5) and each
//! topic's provisioning record (feature 0006 §2.2). `SQLite` only, like the
//! rest of this database.

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
                r"
CREATE TABLE IF NOT EXISTS event_broker__outbox_identity (
    queue      VARCHAR(255) PRIMARY KEY NOT NULL,  -- toolkit-db outbox queue name
    outbox_id  BIGINT NOT NULL                     -- random 64 bits, stored as signed
);

CREATE TABLE IF NOT EXISTS event_broker__topic_binding (
    topic_uuid  VARCHAR(36) PRIMARY KEY NOT NULL,  -- GtsId::to_uuid(topic_id)
    topic_id    VARCHAR(1024) NOT NULL,            -- for operators, not indexed
    log_id      VARCHAR(36) NOT NULL,              -- the current attempt's, then the log's
    state       VARCHAR(16) NOT NULL,              -- 'provisioning' | 'provisioned'
    attempt_at  VARCHAR(32) NOT NULL,              -- RFC 3339 UTC, database time
    partitions  BIGINT                             -- the count the backend holds, once provisioned
);
",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS event_broker__topic_binding;")
            .await?;
        conn.execute_unprepared("DROP TABLE IF EXISTS event_broker__outbox_identity;")
            .await?;
        Ok(())
    }
}
