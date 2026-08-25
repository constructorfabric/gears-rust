//! Add `terminal_failure_at` column to `tenants` for the
//! provisioning-reaper terminal-failure handling per
//! `algo-tenant-hierarchy-management-provisioning-reaper-compensation`.
//!
//! When the `IdP` plugin returns
//! [`account_management_sdk::IdpDeprovisionFailure::Terminal`], the SDK
//! contract says the tenant cannot be deprovisioned by the provider
//! and operator intervention is required. The reaper used to map
//! `Terminal` to `ReaperOutcome::Defer` which released the claim,
//! letting `scan_stuck_provisioning` re-pick the row on the very next
//! tick — producing an indefinite reissue loop and never surfacing
//! the operator-action-required signal.
//!
//! This migration adds `terminal_failure_at`. Once stamped, the row
//! is filtered out of `scan_stuck_provisioning` and stays out of the
//! automatic retry loop until an operator clears the column (or
//! deletes the row outright). The metric
//! `am.tenant_retention{outcome=terminal}` and the new
//! `ReaperResult::terminal` counter give the operator-visible signal
//! that a row needs intervention.

use sea_orm_migration::prelude::*;

const MYSQL_NOT_SUPPORTED: &str = "account-management migrations: MySQL is not supported \
    (this migration set targets PostgreSQL/SQLite); add a dedicated MySQL migration set \
    before running against MySQL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = match backend {
            sea_orm::DatabaseBackend::Postgres => vec![
                "ALTER TABLE tenants ADD COLUMN IF NOT EXISTS terminal_failure_at TIMESTAMPTZ NULL;",
                // Partial index so the `scan_stuck_provisioning`
                // filter (`status = 0 AND terminal_failure_at IS NULL`)
                // still uses an index on the hot path; rows with the
                // column set are operator-action-required and not on
                // the scan path, so they don't need to be indexed.
                "CREATE INDEX IF NOT EXISTS idx_tenants_provisioning_active_scan ON tenants (created_at) WHERE status = 0 AND terminal_failure_at IS NULL;",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                "ALTER TABLE tenants ADD COLUMN terminal_failure_at TEXT NULL;",
                "CREATE INDEX IF NOT EXISTS idx_tenants_provisioning_active_scan ON tenants (created_at) WHERE status = 0 AND terminal_failure_at IS NULL;",
            ],
            _ => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.into()));
            }
        };

        for stmt in statements {
            conn.execute_unprepared(stmt).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let statements: Vec<&str> = match backend {
            sea_orm::DatabaseBackend::Postgres => vec![
                "DROP INDEX IF EXISTS idx_tenants_provisioning_active_scan;",
                "ALTER TABLE tenants DROP COLUMN IF EXISTS terminal_failure_at;",
            ],
            sea_orm::DatabaseBackend::Sqlite => vec![
                "DROP INDEX IF EXISTS idx_tenants_provisioning_active_scan;",
                // SQLite supports DROP COLUMN since 3.35 (2021-03);
                // every supported AM SQLite target satisfies that.
                "ALTER TABLE tenants DROP COLUMN terminal_failure_at;",
            ],
            _ => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.into()));
            }
        };

        for stmt in statements {
            conn.execute_unprepared(stmt).await?;
        }
        Ok(())
    }
}
