//! Indexes the edge listing pages through.
//!
//! `list_node_edges` reads `tenant AND src_node_id = $node AND deleted_at IS
//! NULL AND id > $after ORDER BY id LIMIT n`, and the same on `dst_node_id`.
//! The endpoint indexes of m0009 do not yield rows in id order, so for a hub
//! whose edges are a small share of the table the planner walks the primary
//! key instead and filters row by row: on a stand table of 2 million edges, a
//! page of a 10 000-edge hub took 120 to 220 ms, later pages costing more.
//! `(tenant_id, src_node_id, id)` returns the page directly -- the same
//! statement took 0.1 ms -- and likewise for the incoming side.
//!
//! The listing reads the two sides as two statements and merges them by id,
//! because an `OR` across the two columns cannot use either index for the
//! ordering and falls back to the primary-key walk.
//!
//! Partial on `deleted_at IS NULL`, as every listing reads only live edges.
//! The cost of building them on a populated graph is m0009's: a `SHARE` lock
//! on `edge` for the build, which blocks writes and not reads.

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
CREATE INDEX IF NOT EXISTS idx_edge_src_page
    ON edge (tenant_id, src_node_id, id)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_edge_dst_page
    ON edge (tenant_id, dst_node_id, id)
    WHERE deleted_at IS NULL;
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r"
DROP INDEX IF EXISTS idx_edge_dst_page;
DROP INDEX IF EXISTS idx_edge_src_page;
                ",
            )
            .await?;
        Ok(())
    }
}
