//! A registered type gains a revision counter and an update timestamp.
//!
//! Before type updates existed a `gts_type` row was write-once, so
//! `created_at` was the whole of its history. An identifier whose definition
//! can be replaced in place needs to say *which* definition is in force —
//! ADR-0005 calls each admitted definition a retained revision — and needs to
//! record when it last moved. The retained revisions themselves are not stored
//! yet: this is the counter, not the history table.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r"
ALTER TABLE gts_type
    ADD COLUMN IF NOT EXISTS revision integer NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS updated_at timestamptz NOT NULL DEFAULT now();
UPDATE gts_type SET updated_at = created_at WHERE updated_at < created_at;
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE gts_type DROP COLUMN IF EXISTS revision, \
                 DROP COLUMN IF EXISTS updated_at;",
            )
            .await?;
        Ok(())
    }
}
