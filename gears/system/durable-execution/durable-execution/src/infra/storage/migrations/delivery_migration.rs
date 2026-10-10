//! Additive adoption of framework Outbox; old intents remain recoverable.
use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &'static str {
        "migration_000002_framework_outbox"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let migrations =
            toolkit_db::outbox::outbox_migrations_with_prefix(crate::infra::outbox::PREFIX)
                .map_err(|e| DbErr::Custom(e.to_string()))?;
        let migration = migrations
            .into_iter()
            .find(|m| m.name() == "m001_create_toolkit_outbox_schema__durable_delivery_outbox")
            .ok_or_else(|| DbErr::Custom("required Outbox migration missing".into()))?;
        migration.up(manager).await?;
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE durable_outbox ADD COLUMN forwarded BOOLEAN NOT NULL DEFAULT FALSE",
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "durable delivery migration is forward-only".into(),
        ))
    }
}
