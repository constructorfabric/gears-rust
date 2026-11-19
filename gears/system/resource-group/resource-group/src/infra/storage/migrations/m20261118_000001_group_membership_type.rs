//! Bind schema-validated groups to a membership definition in the local GTS dictionary.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE resource_group ADD COLUMN membership_type_id SMALLINT CONSTRAINT fk_rg_membership_type REFERENCES gts_type(id) ON DELETE RESTRICT",
        ).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "Cannot roll back stored membership schema bindings".to_owned(),
        ))
    }
}
