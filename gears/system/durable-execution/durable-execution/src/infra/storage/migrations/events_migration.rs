use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &'static str {
        "migration_000004_events"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let (uuid, bytes) = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => ("UUID", "BYTEA"),
            sea_orm::DatabaseBackend::Sqlite => ("TEXT", "BLOB"),
            _ => return Err(DbErr::Custom("unsupported durable journal database".into())),
        };
        manager.get_connection().execute_unprepared(&format!("CREATE TABLE durable_events (id {uuid} PRIMARY KEY, tenant_id {uuid} NOT NULL, owner_id {uuid} NOT NULL, run_id {uuid} NOT NULL REFERENCES durable_runs(id), sequence BIGINT NOT NULL, state {bytes} NOT NULL, UNIQUE(run_id, sequence));")).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("event history is forward-only".into()))
    }
}
