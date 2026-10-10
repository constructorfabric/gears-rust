use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &'static str {
        "migration_000003_activity_journal"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let (uuid, bytes, timestamp) = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => ("UUID", "BYTEA", "TIMESTAMPTZ"),
            sea_orm::DatabaseBackend::Sqlite => ("TEXT", "BLOB", "TEXT"),
            _ => return Err(DbErr::Custom("unsupported durable journal database".into())),
        };
        manager
            .get_connection()
            .execute_unprepared(&format!(
                r"
ALTER TABLE durable_runs ADD COLUMN storage_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE durable_runs ADD COLUMN activity_count INTEGER NOT NULL DEFAULT 0;
CREATE TABLE durable_activities (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_runs(id), activity_id TEXT NOT NULL,
 position INTEGER NOT NULL, stage INTEGER, epoch BIGINT NOT NULL, status TEXT NOT NULL,
 fence BIGINT NOT NULL, lease_until {timestamp}, due_at {timestamp}, state {bytes} NOT NULL,
 UNIQUE(run_id, activity_id), UNIQUE(run_id, position)
);
CREATE INDEX ix_durable_activities_lease ON durable_activities(status, lease_until);
CREATE TABLE durable_attempts (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_runs(id), activity_id TEXT NOT NULL,
 number BIGINT NOT NULL, epoch BIGINT NOT NULL, state {bytes} NOT NULL,
 UNIQUE(run_id, activity_id, number)
);
CREATE TABLE durable_epochs (
 id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL,
 run_id {uuid} NOT NULL REFERENCES durable_runs(id), epoch BIGINT NOT NULL, state {bytes} NOT NULL,
 UNIQUE(run_id, epoch)
);
"
            ))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("journal migration is forward-only".into()))
    }
}
