//! A payload index for tombstoned nodes, so a scope replacement finds its
//! tombstoned members without reading the tenant.
//!
//! A replacement removes tombstoned members of its scope as well as live
//! ones (gears-rust #5194): a tombstone keeps its payload, search text and
//! vector, and an empty replacement is the scope's erasure. Every index on
//! `node` that serves the member query -- the payload GIN of m0005, the type
//! index of m0001 -- is partial on `deleted_at IS NULL`, and PostgreSQL uses a
//! partial index only for a query that states its condition. So the
//! replacement reads live and tombstoned members in two statements, and this
//! is the index for the second; tombstones are a small share of the rows, so
//! it stays small.

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
CREATE INDEX IF NOT EXISTS idx_node_payload_tombstoned
    ON node USING gin (payload jsonb_path_ops) WHERE deleted_at IS NOT NULL;
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS idx_node_payload_tombstoned;")
            .await?;
        Ok(())
    }
}
