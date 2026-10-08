//! Approvals schema, following Products.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_up(manager, "pricing_", Some("bss")).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_down(manager, "pricing_", Some("bss")).await
    }
}
