//! Indexes the two hot read paths could not use.
//!
//! **Edge endpoints, not partial.** Added for the SQL/PGQ hop (gears-rust
//! #5267), whose pattern could not state `deleted_at IS NULL`. Kept: a read of
//! `edge` by endpoint that does not name `deleted_at` uses them, and dropping
//! them is a measured decision of its own.
//!
//! **A projection filtered on a payload path.** The statement is `tenant AND
//! deleted_at IS NULL AND gts_node_type_id IN (…) AND payload @> … ORDER BY
//! node_key LIMIT n`. At a middling selectivity the planner walks
//! `(tenant_id, node_key)` for the ordering and filters row by row -- 81 581
//! rows discarded, 69 ms, for a type of 40 000 in a tenant of 240 000 --
//! because neither the payload GIN nor the type index yields rows in key
//! order. `(tenant_id, gts_node_type_id, node_key)` does: the same statement
//! took 0.6 ms, and through REST the listing went from 44 to 299 requests a
//! second at 32 clients.
//!
//! **Cost of running this on a populated graph.** The migration runner wraps
//! every migration in a transaction (README § Known limitations), so these are
//! plain `CREATE INDEX`es: each holds a `SHARE` lock on its table for its
//! build, which blocks writes to that table and not reads. B-tree builds over
//! the columns named here are fast -- on the 638 000-edge stand graph the edge
//! pair took about a second -- but a deployment far larger than that should
//! run this migration in a quiet window.

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
CREATE INDEX IF NOT EXISTS idx_edge_src_any ON edge (tenant_id, src_node_id);
CREATE INDEX IF NOT EXISTS idx_edge_dst_any ON edge (tenant_id, dst_node_id);
CREATE INDEX IF NOT EXISTS idx_node_type_key
    ON node (tenant_id, gts_node_type_id, node_key)
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
DROP INDEX IF EXISTS idx_node_type_key;
DROP INDEX IF EXISTS idx_edge_dst_any;
DROP INDEX IF EXISTS idx_edge_src_any;
                ",
            )
            .await?;
        Ok(())
    }
}
