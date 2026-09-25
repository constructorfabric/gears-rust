//! Migration `m0005`: the lease tables of the lease-operations feature
//! (DESIGN section 3.7, `leases`, `lease_holds`, `lease_capacity_counters`).
//!
//! `qe_leases` carries the state machine and, with it, the two values a
//! settlement cannot take from its caller: `subject_key`, the idempotency
//! subject the acquisition fingerprinted, and `attribution_hash`, the
//! attribution the PDP authorized it under. Commit and release complete their
//! scope from the first and a rollback of a commit must present the second.
//! `reserved_amount` is what the acquisition asked for, which the commit's
//! share is measured against; the plan's holds need not sum to it.
//!
//! `qe_lease_holds` is one row per Quota in the plan, rather than an array on
//! the lease, so a Quota's holds are reachable by index from the Quota side:
//! the deactivation cascade and the expired-hold reconciliation both start
//! there. `returned_at` is that reconciliation's arbiter — an expired lease is
//! released the moment its TTL passes (I4), but its capacity sits in the
//! counter until a writer or the sweeper gives it back, and exactly one of them
//! may do so. The index leads with `(quota_id, period_id, returned_at)` because
//! every one of those readers asks the same question: what does this counter
//! row still owe?
//!
//! `qe_lease_capacity_counters` is the serialization point of the
//! per-`(tenant, metric)` cap (I7), not its source of truth: `active_count` is
//! maintained for diagnostics, while admission counts live leases under the
//! row lock, so an expired lease never occupies the cap. A pair's row is created
//! with its first Quota, so an acquisition only ever locks it; the backfill here
//! gives every pair that already has a Quota its row. It copies the earliest
//! `created_at` rather than taking `now()`, so the stored timestamp has exactly
//! the encoding the ORM itself writes on both backends.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "quota-enforcement-storage-plugin: MySQL is not supported; \
    this migration set targets PostgreSQL and SQLite";

/// Drop order: children before the tables they reference.
const TABLES: [&str; 3] = ["qe_lease_holds", "qe_leases", "qe_lease_capacity_counters"];

const POSTGRES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS qe_leases ( \
        token UUID PRIMARY KEY, \
        tenant_id UUID NOT NULL, \
        metric TEXT NOT NULL, \
        subject_key BYTEA NOT NULL, \
        attribution_hash BYTEA NOT NULL, \
        idem_key TEXT NOT NULL, \
        state TEXT NOT NULL CHECK (state IN \
            ('active', 'committed', 'released', 'auto_released', 'resolved_by_deactivation')), \
        reserved_amount BIGINT NOT NULL CHECK (reserved_amount >= 0), \
        acquired_at TIMESTAMPTZ NOT NULL, \
        expiry_at TIMESTAMPTZ NOT NULL, \
        resolved_at TIMESTAMPTZ NULL, \
        record_version INTEGER NOT NULL CHECK (record_version >= 1), \
        created_at TIMESTAMPTZ NOT NULL, \
        updated_at TIMESTAMPTZ NOT NULL \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_leases_cap \
        ON qe_leases (tenant_id, metric, state, expiry_at);",
    "CREATE INDEX IF NOT EXISTS idx_qe_leases_expiry ON qe_leases (state, expiry_at);",
    "CREATE TABLE IF NOT EXISTS qe_lease_holds ( \
        lease_token UUID NOT NULL REFERENCES qe_leases(token) ON DELETE CASCADE, \
        quota_id UUID NOT NULL REFERENCES qe_quotas(id) ON DELETE RESTRICT, \
        tenant_id UUID NOT NULL, \
        held_amount BIGINT NOT NULL CHECK (held_amount >= 0), \
        period_id UUID NULL REFERENCES qe_quota_consumption_counters(period_id) ON DELETE RESTRICT, \
        returned_at TIMESTAMPTZ NULL, \
        PRIMARY KEY (lease_token, quota_id) \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_lease_holds_counter \
        ON qe_lease_holds (quota_id, period_id, returned_at);",
    "CREATE TABLE IF NOT EXISTS qe_lease_capacity_counters ( \
        tenant_id UUID NOT NULL, \
        metric TEXT NOT NULL, \
        active_count INTEGER NOT NULL CHECK (active_count >= 0), \
        record_version INTEGER NOT NULL CHECK (record_version >= 1), \
        updated_at TIMESTAMPTZ NOT NULL, \
        PRIMARY KEY (tenant_id, metric) \
    );",
    "INSERT INTO qe_lease_capacity_counters \
        (tenant_id, metric, active_count, record_version, updated_at) \
        SELECT tenant_id, metric, 0, 1, MIN(created_at) FROM qe_quotas \
        GROUP BY tenant_id, metric \
        ON CONFLICT (tenant_id, metric) DO NOTHING;",
];

const SQLITE: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS qe_leases ( \
        token TEXT PRIMARY KEY NOT NULL, \
        tenant_id TEXT NOT NULL, \
        metric TEXT NOT NULL, \
        subject_key BLOB NOT NULL, \
        attribution_hash BLOB NOT NULL, \
        idem_key TEXT NOT NULL, \
        state TEXT NOT NULL CHECK (state IN \
            ('active', 'committed', 'released', 'auto_released', 'resolved_by_deactivation')), \
        reserved_amount INTEGER NOT NULL CHECK (reserved_amount >= 0), \
        acquired_at TEXT NOT NULL, \
        expiry_at TEXT NOT NULL, \
        resolved_at TEXT NULL, \
        record_version INTEGER NOT NULL CHECK (record_version >= 1), \
        created_at TEXT NOT NULL, \
        updated_at TEXT NOT NULL \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_leases_cap \
        ON qe_leases (tenant_id, metric, state, expiry_at);",
    "CREATE INDEX IF NOT EXISTS idx_qe_leases_expiry ON qe_leases (state, expiry_at);",
    "CREATE TABLE IF NOT EXISTS qe_lease_holds ( \
        lease_token TEXT NOT NULL REFERENCES qe_leases(token) ON DELETE CASCADE, \
        quota_id TEXT NOT NULL REFERENCES qe_quotas(id) ON DELETE RESTRICT, \
        tenant_id TEXT NOT NULL, \
        held_amount INTEGER NOT NULL CHECK (held_amount >= 0), \
        period_id TEXT NULL REFERENCES qe_quota_consumption_counters(period_id) ON DELETE RESTRICT, \
        returned_at TEXT NULL, \
        PRIMARY KEY (lease_token, quota_id) \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_lease_holds_counter \
        ON qe_lease_holds (quota_id, period_id, returned_at);",
    "CREATE TABLE IF NOT EXISTS qe_lease_capacity_counters ( \
        tenant_id TEXT NOT NULL, \
        metric TEXT NOT NULL, \
        active_count INTEGER NOT NULL CHECK (active_count >= 0), \
        record_version INTEGER NOT NULL CHECK (record_version >= 1), \
        updated_at TEXT NOT NULL, \
        PRIMARY KEY (tenant_id, metric) \
    );",
    "INSERT OR IGNORE INTO qe_lease_capacity_counters \
        (tenant_id, metric, active_count, record_version, updated_at) \
        SELECT tenant_id, metric, 0, 1, MIN(created_at) FROM qe_quotas \
        GROUP BY tenant_id, metric;",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let statements = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES,
            sea_orm::DatabaseBackend::Sqlite => SQLITE,
            _ => return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned())),
        };
        let conn = manager.get_connection();
        for sql in statements {
            conn.execute_unprepared(sql).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if matches!(
            manager.get_database_backend(),
            sea_orm::DatabaseBackend::MySql
        ) {
            return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
        }
        let conn = manager.get_connection();
        for table in TABLES {
            conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table};"))
                .await?;
        }
        Ok(())
    }
}
