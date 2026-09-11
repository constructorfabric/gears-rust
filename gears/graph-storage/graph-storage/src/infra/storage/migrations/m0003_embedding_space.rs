//! The embedding-space registry (`cpt-cf-graph-storage-dbtable-embedding-space`).
//!
//! DESIGN calls this "the canonical durable location of the embedding-space
//! identity": what readiness compares the active provider against, and what
//! `node.embedding_epoch` points at. Without it a same-dimension model swap is
//! invisible, which is the failure ADR-0005 exists to prevent.
//!
//! **One divergence from DESIGN's column list:** `tenant_id`, holding the nil
//! UUID. The table is deployment-wide, so DESIGN gives it no tenant column,
//! but every runtime read in this gear goes through the secure ORM and needs a
//! scopable entity. `graph_meta` already carries deployment-level keys the
//! same way, and `probe_pgq` already reads under a nil-tenant scope, so this
//! is the gear's existing device rather than a new one. Recorded in
//! the gear's development notes.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Epoch of the space a deployment opens on first boot.
pub const FIRST_EPOCH: i64 = 1;

/// `state` values (DESIGN § Table `embedding_space`). Only `active` is written
/// by this iteration: `migrating` and `retired` belong to the model-change
/// lifecycle, which is deferred.
pub const STATE_ACTIVE: &str = "active";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r"
CREATE TABLE IF NOT EXISTS embedding_space (
    tenant_id           UUID        NOT NULL,
    epoch               BIGINT      NOT NULL,
    identity_hash       TEXT        NOT NULL,
    model_artifact      TEXT        NOT NULL,
    tokenizer_artifact  TEXT        NOT NULL,
    preprocessing       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    pooling             JSONB       NOT NULL DEFAULT '{}'::jsonb,
    normalization       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    dimension           INTEGER     NOT NULL,
    state               TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    activated_at        TIMESTAMPTZ,
    PRIMARY KEY (tenant_id, epoch)
);

-- At most one active space per deployment. The single-embedding-space
-- constraint is a database fact, not a convention the boot path remembers to
-- uphold: two active rows would make `node.embedding_epoch` ambiguous and
-- similarity search would rank across incomparable vectors.
CREATE UNIQUE INDEX IF NOT EXISTS idx_embedding_space_one_active
    ON embedding_space (tenant_id) WHERE state = 'active';
                ",
            )
            .await?;

        // The vector arm serves only current vectors, and a vector whose input
        // changed while embedding was skipped is marked by a NULL epoch. Keep
        // those out of the index itself rather than filtering them after it:
        // the HNSW index decides how many rows the arm even considers, so a
        // stale row left inside it displaces a live one from the result.
        manager
            .get_connection()
            .execute_unprepared(
                r"
DROP INDEX IF EXISTS idx_node_embedding;
CREATE INDEX IF NOT EXISTS idx_node_embedding
    ON node USING hnsw (embedding vector_cosine_ops)
    WHERE deleted_at IS NULL
      AND embedding IS NOT NULL
      AND embedding_epoch IS NOT NULL;
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
DROP TABLE IF EXISTS embedding_space;
DROP INDEX IF EXISTS idx_node_embedding;
CREATE INDEX IF NOT EXISTS idx_node_embedding
    ON node USING hnsw (embedding vector_cosine_ops)
    WHERE deleted_at IS NULL AND embedding IS NOT NULL;
                ",
            )
            .await?;
        Ok(())
    }
}
