//! One GIN over every node payload (`jsonb_path_ops`).
//!
//! The projection admits `$filter` over the payload paths a type declares in
//! its `index` trait (DEVIATIONS D-104). The design asks for a B-tree over
//! each declared path's extraction expression, which needs `CREATE INDEX` at
//! registration time — and the platform's secure ORM exposes no statement
//! surface a gear could run DDL through, by design. What a static migration
//! *can* provide is one containment index: equality on any path, nested or
//! not, is written as `payload @> '{"a":{"b":"x"}}'` and served from here.
//! Range comparison and ordering read the extraction expression over the rows
//! the type index already narrowed, without an index of their own; the
//! deviation entry records the gap and what closes it.

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
CREATE INDEX IF NOT EXISTS idx_node_payload
    ON node USING gin (payload jsonb_path_ops) WHERE deleted_at IS NULL;
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_node_payload;")
            .await?;
        Ok(())
    }
}
