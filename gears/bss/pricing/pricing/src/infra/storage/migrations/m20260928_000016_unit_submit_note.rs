//! D-445: an approval unit carries its submitter's note. A FORWARD migration (D-427 closed D-412:
//! the chain is deployed and no shipped migration is edited again).
//!
//! The approval library's `ddl::up()` is the body of `m20260926_000002` and stays as it shipped;
//! this migration runs the library's separate step, `bss_approval::ddl::apply_add_submit_note`:
//! `pricing_approval_unit.submit_note`, nullable `text`, no default, no CHECK, by `ADD COLUMN` on
//! both dialects (Postgres `IF NOT EXISTS`; on `SQLite` the catalog is read first), so a replay
//! changes nothing. A unit written before it reads null. Pricing's submit doors take no note
//! today, so every pricing unit reads null; the column keeps the one unit shape the two gears share
//! (products fills it, P-D-219).
//!
//! `down` drops the column the same way.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_add_submit_note(manager, "pricing_", Some("bss"))
            .await
            .map_err(|e| DbErr::Migration(format!("{}: {e}", self.name())))
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        bss_approval::ddl::apply_drop_submit_note(manager, "pricing_", Some("bss"))
            .await
            .map_err(|e| DbErr::Migration(format!("{}: {e}", self.name())))
    }
}
