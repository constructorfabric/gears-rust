//! Formerly the SQL/PGQ property graph `kb`; now a no-op.
//!
//! This migration created `kb` over `node` and `edge` on a server whose
//! `server_version_num` was 190000 or more, and skipped the DDL below that.
//! `PostgreSQL` 19 beta 4 reverted SQL/PGQ and 19 GA ships without it, while
//! still reporting 190000, so the DDL failed there and the gear could not
//! start on any stock `PostgreSQL` 19 (gears-rust #5267). The gear no longer
//! uses SQL/PGQ, so the migration creates nothing on any server.
//!
//! It stays in the list under its name: the migration history records it as
//! applied on every database that ran it, and removing it would read as an
//! unknown applied migration. A graph an earlier release created is dropped
//! by `m0010_drop_property_graph`.

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
