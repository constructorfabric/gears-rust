//! Indexes the two hot read paths could not use.
//!
//! **The SQL/PGQ hop.** Every edge index is partial on `deleted_at IS NULL`,
//! and the pattern cannot say `deleted_at IS NULL`: the column is outside the
//! edge element's `PROPERTIES`, so `MATCH` does not see it, and the tombstone
//! filter is applied afterwards by an ordinary scoped read. A predicate the
//! planner never sees cannot license a partial index, so each hop through
//! `GRAPH_TABLE` read the whole edge table (a parallel sequential scan) to
//! find a frontier's few edges. Measured on the Studio stand (2026-09-27/28):
//! 39 / 100 / 144 ms at depth 1 / 2 / 3 against 4 / 6 / 8 ms for the
//! two-query hop over the same graph, and 537 ms for one hop from a hub of a
//! 638 000-edge graph. With the endpoint indexes below, not partial, that hop
//! is an index scan (0.4 ms). The partial ones stay: the two-query hop and
//! adjacency reads name `deleted_at` and keep using them.
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
