//! Migration `m0004`: the counter and replay tables of the
//! consumption-operations feature (DESIGN section 3.7,
//! `quota_consumption_counters` and `idempotency_records`).
//!
//! `qe_quota_consumption_counters` holds one row per `(Quota, period)`. The
//! unique key on `(quota_id, period_start)` is both the current-period index
//! and the arbiter of concurrent materialization: two transactions opening the
//! same period race on it, and the loser reads the winner's row.
//! `period_end` is `NOT NULL`; a one-time Quota stores the open-ended sentinel,
//! so the "period closed" predicate stays `now >= period_end` everywhere
//! instead of branching on a nullable column.
//!
//! `qe_idempotency_records` is keyed by the full four-component scope, which is
//! also what serializes two writers that share a key but lock disjoint Quota
//! rows: the loser's insert violates this primary key, rolls its transaction
//! back, and resolves into a replay or a payload mismatch. There is
//! deliberately no key-only index: every lookup, rollback's original included,
//! carries the whole scope.
//!
//! `attribution_hash` is what a rollback must present to reverse a debit: the
//! scope proves the tenant and subjects, this proves the metric and resource.
//! `applied_entries` is plugin-private: the per-Quota amounts and their
//! acquisition periods, which is what rollback reverses (I5).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str = "quota-enforcement-storage-plugin: MySQL is not supported; \
    this migration set targets PostgreSQL and SQLite";

/// Drop order: children before the tables they reference.
const TABLES: [&str; 2] = ["qe_idempotency_records", "qe_quota_consumption_counters"];

const POSTGRES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS qe_quota_consumption_counters ( \
        period_id UUID PRIMARY KEY, \
        quota_id UUID NOT NULL REFERENCES qe_quotas(id) ON DELETE RESTRICT, \
        tenant_id UUID NOT NULL, \
        period_start TIMESTAMPTZ NOT NULL, \
        period_end TIMESTAMPTZ NOT NULL, \
        consumed BIGINT NOT NULL CHECK (consumed >= 0), \
        highest_crossed_threshold_pct SMALLINT NULL, \
        is_settled BOOLEAN NOT NULL, \
        record_version INTEGER NOT NULL CHECK (record_version >= 1), \
        created_at TIMESTAMPTZ NOT NULL, \
        updated_at TIMESTAMPTZ NOT NULL, \
        CONSTRAINT uq_qe_consumption_period UNIQUE (quota_id, period_start) \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_consumption_unsettled \
        ON qe_quota_consumption_counters (quota_id, is_settled, period_end);",
    "CREATE TABLE IF NOT EXISTS qe_idempotency_records ( \
        tenant_id UUID NOT NULL, \
        subject_key BYTEA NOT NULL, \
        operation_type TEXT NOT NULL, \
        idem_key TEXT NOT NULL, \
        payload_hash BYTEA NOT NULL, \
        decision_blob TEXT NOT NULL, \
        applied_entries TEXT NULL, \
        attribution_hash BYTEA NULL, \
        reversed_by_key TEXT NULL, \
        engine_id TEXT NULL, \
        policy_id TEXT NULL, \
        policy_version INTEGER NULL, \
        created_at TIMESTAMPTZ NOT NULL, \
        expires_at TIMESTAMPTZ NOT NULL, \
        PRIMARY KEY (tenant_id, subject_key, operation_type, idem_key) \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_idempotency_expires \
        ON qe_idempotency_records (expires_at);",
    "ALTER TABLE qe_quota_allocation_counters \
        ADD COLUMN IF NOT EXISTS highest_crossed_threshold_pct SMALLINT NULL;",
];

const SQLITE: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS qe_quota_consumption_counters ( \
        period_id TEXT PRIMARY KEY NOT NULL, \
        quota_id TEXT NOT NULL REFERENCES qe_quotas(id) ON DELETE RESTRICT, \
        tenant_id TEXT NOT NULL, \
        period_start TEXT NOT NULL, \
        period_end TEXT NOT NULL, \
        consumed INTEGER NOT NULL CHECK (consumed >= 0), \
        highest_crossed_threshold_pct INTEGER NULL, \
        is_settled BOOLEAN NOT NULL, \
        record_version INTEGER NOT NULL CHECK (record_version >= 1), \
        created_at TEXT NOT NULL, \
        updated_at TEXT NOT NULL, \
        CONSTRAINT uq_qe_consumption_period UNIQUE (quota_id, period_start) \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_consumption_unsettled \
        ON qe_quota_consumption_counters (quota_id, is_settled, period_end);",
    "CREATE TABLE IF NOT EXISTS qe_idempotency_records ( \
        tenant_id TEXT NOT NULL, \
        subject_key BLOB NOT NULL, \
        operation_type TEXT NOT NULL, \
        idem_key TEXT NOT NULL, \
        payload_hash BLOB NOT NULL, \
        decision_blob TEXT NOT NULL, \
        applied_entries TEXT NULL, \
        attribution_hash BLOB NULL, \
        reversed_by_key TEXT NULL, \
        engine_id TEXT NULL, \
        policy_id TEXT NULL, \
        policy_version INTEGER NULL, \
        created_at TEXT NOT NULL, \
        expires_at TEXT NOT NULL, \
        PRIMARY KEY (tenant_id, subject_key, operation_type, idem_key) \
    );",
    "CREATE INDEX IF NOT EXISTS idx_qe_idempotency_expires \
        ON qe_idempotency_records (expires_at);",
    "ALTER TABLE qe_quota_allocation_counters \
        ADD COLUMN highest_crossed_threshold_pct INTEGER NULL;",
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
        // The added column stays: SQLite cannot drop one before 3.35, and an
        // unused nullable column is harmless.
        Ok(())
    }
}
