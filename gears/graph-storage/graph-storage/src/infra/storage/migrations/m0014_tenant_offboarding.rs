//! Tenant offboarding (`cpt-cf-graph-storage-fr-tenant-offboarding`).
//!
//! One row per offboarded tenant. Its presence is the fence: every operation
//! for the tenant is refused from the moment it commits. It stays after the
//! tenant's rows are gone, as the permanent record that the deletion ran, so
//! a late write cannot recreate the tenant and a repeated call answers
//! "done". It holds the tenant's id, dates and counts, none of its content.

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
CREATE TABLE IF NOT EXISTS tenant_offboarding (
    tenant_id            UUID        NOT NULL PRIMARY KEY,
    deletion_generation  BIGINT,
    fenced_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at         TIMESTAMPTZ,
    removed              JSONB       NOT NULL DEFAULT '{}'::jsonb,
    failures             INTEGER     NOT NULL DEFAULT 0,
    last_failure         TEXT,
    last_failure_at      TIMESTAMPTZ
);

-- The worker asks which deletions are still running; this answers it
-- without a scan of every tenant ever offboarded.
CREATE INDEX IF NOT EXISTS idx_tenant_offboarding_pending
    ON tenant_offboarding (fenced_at) WHERE completed_at IS NULL;
                ",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS tenant_offboarding;")
            .await?;
        Ok(())
    }
}
