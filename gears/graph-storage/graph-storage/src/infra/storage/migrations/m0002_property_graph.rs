//! A no-op. It created the SQL/PGQ property graph `kb` (gears-rust #5267).
//!
//! It stays in the list because databases record it as applied; removing it
//! would read as an unknown applied migration. `m0010_drop_property_graph`
//! drops a graph it left behind.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
