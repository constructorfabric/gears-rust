use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &'static str {
        "migration_000005_definition_registry"
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
        manager.get_connection().execute_unprepared(&format!(
            "CREATE TABLE durable_definitions (id {uuid} PRIMARY KEY, name TEXT NOT NULL UNIQUE, revision BIGINT NOT NULL, state {bytes} NOT NULL);
             ALTER TABLE durable_runs ADD COLUMN registration_generation BIGINT NOT NULL DEFAULT 0;
             CREATE INDEX ix_durable_runs_definition_generation ON durable_runs(definition, registration_generation, status);"
        )).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "definition generations are forward-only".into(),
        ))
    }
}
