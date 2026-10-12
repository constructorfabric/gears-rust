//! Shared approval engine tables.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_up(manager, "products_", Some("bss")).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_down(manager, "products_", Some("bss")).await
    }
}

#[cfg(test)]
#[path = "m20260925_000003_create_products_approvals_tests.rs"]
mod tests;
