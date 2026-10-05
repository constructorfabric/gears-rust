//! Which scope declared a static edge
//! (`cpt-cf-graph-storage-fr-scope-replacement`).
//!
//! A scope replacement is a declarative snapshot: what the batch does not name
//! is gone. Nodes carried that from the first migration, because membership is
//! a payload attribute and a node either has it or does not. Edges had no such
//! mark, so the replacement reckoned about them through their endpoints — and
//! an edge the producer dropped while keeping both endpoints was never stale,
//! never removed, and stayed visible for good. Replaying the same snapshot
//! could not repair it.
//!
//! Endpoint membership cannot stand in for ownership either. Two scopes may
//! share endpoint nodes — they are different payload attributes, and a node
//! can satisfy both — so "every edge between nodes of this scope" would take
//! edges another producer declared under another scope.
//!
//! Hence the mark itself. `NULL` means no scope has declared this edge: a
//! replacement then leaves it alone unless one of its endpoints is departing,
//! which is the behaviour edges written before this migration keep.

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
ALTER TABLE edge ADD COLUMN IF NOT EXISTS scope_attribute TEXT;
ALTER TABLE edge ADD COLUMN IF NOT EXISTS scope_value     TEXT;

-- Partial: only declared edges are ever looked up this way, and they are the
-- minority of a graph that also holds analysis edges and unscoped writes.
CREATE INDEX IF NOT EXISTS edge_scope_idx
    ON edge (tenant_id, scope_attribute, scope_value)
    WHERE scope_attribute IS NOT NULL;
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
DROP INDEX IF EXISTS edge_scope_idx;
ALTER TABLE edge DROP COLUMN IF EXISTS scope_value;
ALTER TABLE edge DROP COLUMN IF EXISTS scope_attribute;
                ",
            )
            .await?;
        Ok(())
    }
}
