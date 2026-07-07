//! Create the three ASC 606 revenue-recognition tables in schema `bss`
//! (Slice 4): `ledger_recognition_schedule` (the documented release plan for one
//! single-revenue-stream deferred Contract-liability balance, keyed by
//! `(tenant_id, schedule_id)`), `ledger_recognition_segment` (one time- or
//! milestone-slice of a schedule — the **at-most-once unit**, one per
//! `(schedule, period)`, keyed by `(tenant_id, schedule_id, segment_no)`), and
//! `ledger_recognition_run` (an orchestration wrapper that releases due segments
//! for a period — **not** itself the dedup key, keyed by
//! `(tenant_id, period_id, run_id)`).
//!
//! `recognized_minor <= total_deferred_minor` on the schedule is the
//! **authoritative** in-transaction over-recognition guard (design §7); the
//! `RecognitionRunner` (Phase 2) bumps `recognized_minor` by an in-place delta
//! under the lock order and the CHECK is evaluated post-delta. The partial
//! `UNIQUE (tenant_id, source_invoice_id, source_invoice_item_ref,
//! revenue_stream) WHERE status='ACTIVE'` is the **at-most-one-live** guard (one
//! current schedule per business key); build-idempotency is decoupled from
//! `status` and lives in `idempotency_dedup` (Rev3 / S4-F2), so a terminal
//! `COMPLETED` schedule is archivable without re-opening a duplicate-build hole.
//! The `segment_no` is immutable and 1:1 with `period_id`
//! (`UNIQUE (tenant_id, schedule_id, period_id)`), so the dedup grain and the
//! UNIQUE grain are provably identical (design §4.1 / §7).
//!
//! **Lock order.** A recognition post first locks the `CONTRACT_LIABILITY` +
//! `REVENUE` `account_balance` rows via the Slice 1 projection (the existing
//! balance-grain order), then the stamp sidecar takes `recognition_schedule`
//! (the `recognized_minor` delta) BEFORE `recognition_segment` (the `DONE`
//! stamp) — one consistent order across all recognition posts, enforced
//! PROCEDURALLY by the sidecar's call order. These tables are NOT projector
//! balance grains, so they carry no `GrainTable` rank (the projector ranks stay
//! balance-only; see `grain_lock_order_ranks_are_pinned`).
//!
//! All CHECKs are created in final form up-front (Foundation §7.2). `SQLite`
//! mirrors the same shape with the systematic transforms (`uuid`→`text`,
//! `timestamptz`→`text`); the CHECKs + PKs + indexes (incl. the partial UNIQUE,
//! which both backends support via `CREATE UNIQUE INDEX … WHERE`) are preserved.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_recognition_schedule (
        tenant_id               uuid          NOT NULL,
        schedule_id             varchar(128)  NOT NULL,
        payer_tenant_id         uuid          NOT NULL,
        source_invoice_id       varchar(128)  NOT NULL,
        source_invoice_item_ref varchar(128)  NOT NULL,
        po_allocation_group     varchar(128),
        subscription_ref        varchar(128),
        revenue_stream          varchar(64)   NOT NULL,
        currency                varchar(16)   NOT NULL,
        total_deferred_minor    bigint        NOT NULL,
        recognized_minor        bigint        NOT NULL DEFAULT 0,
        policy_ref              varchar(256)  NOT NULL,
        ssp_snapshot_ref        varchar(256),
        vc_estimate_ref         varchar(256),
        vc_method_ref           varchar(256),
        status                  varchar(16)   NOT NULL,
        version                 bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, schedule_id),
        CONSTRAINT chk_ledger_recognition_schedule_recognized_nonneg
            CHECK (recognized_minor >= 0),
        CONSTRAINT chk_ledger_recognition_schedule_deferred_nonneg
            CHECK (total_deferred_minor >= 0),
        CONSTRAINT chk_ledger_recognition_schedule_recognized_le_deferred
            CHECK (recognized_minor <= total_deferred_minor),
        CONSTRAINT chk_ledger_recognition_schedule_status
            CHECK (status IN ('ACTIVE','COMPLETED','REPLACED','CANCELLED'))
    )",
    // Partial UNIQUE — the at-most-one-live guard (one current schedule per
    // business key); `REPLACED`/`COMPLETED`/`CANCELLED` rows keep history
    // without colliding. Build-idempotency lives in `idempotency_dedup`, not
    // `status` (Rev3 / S4-F2).
    "CREATE UNIQUE INDEX ledger_recognition_schedule_live_idx
        ON bss.ledger_recognition_schedule
            (tenant_id, source_invoice_id, source_invoice_item_ref, revenue_stream)
        WHERE status = 'ACTIVE'",
    "CREATE TABLE bss.ledger_recognition_segment (
        tenant_id     uuid          NOT NULL,
        schedule_id   varchar(128)  NOT NULL,
        segment_no    integer       NOT NULL,
        period_id     varchar(64)   NOT NULL,
        amount_minor  bigint        NOT NULL,
        status        varchar(16)   NOT NULL,
        recognized_at timestamptz,
        run_id        uuid,
        PRIMARY KEY (tenant_id, schedule_id, segment_no),
        CONSTRAINT chk_ledger_recognition_segment_amount_nonneg
            CHECK (amount_minor >= 0),
        CONSTRAINT chk_ledger_recognition_segment_status
            CHECK (status IN ('PENDING','QUEUED','DONE'))
    )",
    // `segment_no` is 1:1 with `period_id` — the dedup grain ≡ the UNIQUE grain.
    "CREATE UNIQUE INDEX ledger_recognition_segment_period_idx
        ON bss.ledger_recognition_segment (tenant_id, schedule_id, period_id)",
    "CREATE TABLE bss.ledger_recognition_run (
        tenant_id      uuid          NOT NULL,
        run_id         uuid          NOT NULL,
        period_id      varchar(64)   NOT NULL,
        started_at_utc timestamptz   NOT NULL,
        status         varchar(16)   NOT NULL,
        PRIMARY KEY (tenant_id, period_id, run_id),
        CONSTRAINT chk_ledger_recognition_run_status
            CHECK (status IN ('RUNNING','DONE','FAILED'))
    )",
    // Run-trigger dedup `(tenant, period_id, run_id)` + the single-active-run
    // advisory lock live at the orchestration layer (Phase 2); this index serves
    // the per-`(tenant, period_id)` scan that selects in-flight runs.
    "CREATE INDEX ledger_recognition_run_period_idx
        ON bss.ledger_recognition_run (tenant_id, period_id)",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_recognition_run",
    "DROP TABLE IF EXISTS bss.ledger_recognition_segment",
    "DROP TABLE IF EXISTS bss.ledger_recognition_schedule",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`→`text`,
// `timestamptz`→`text`; all CHECKs + PKs + indexes (incl. the partial UNIQUE)
// preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_recognition_schedule (
        tenant_id               text          NOT NULL,
        schedule_id             varchar(128)  NOT NULL,
        payer_tenant_id         text          NOT NULL,
        source_invoice_id       varchar(128)  NOT NULL,
        source_invoice_item_ref varchar(128)  NOT NULL,
        po_allocation_group     varchar(128),
        subscription_ref        varchar(128),
        revenue_stream          varchar(64)   NOT NULL,
        currency                varchar(16)   NOT NULL,
        total_deferred_minor    bigint        NOT NULL,
        recognized_minor        bigint        NOT NULL DEFAULT 0,
        policy_ref              varchar(256)  NOT NULL,
        ssp_snapshot_ref        varchar(256),
        vc_estimate_ref         varchar(256),
        vc_method_ref           varchar(256),
        status                  varchar(16)   NOT NULL,
        version                 bigint        NOT NULL DEFAULT 0,
        PRIMARY KEY (tenant_id, schedule_id),
        CONSTRAINT chk_ledger_recognition_schedule_recognized_nonneg
            CHECK (recognized_minor >= 0),
        CONSTRAINT chk_ledger_recognition_schedule_deferred_nonneg
            CHECK (total_deferred_minor >= 0),
        CONSTRAINT chk_ledger_recognition_schedule_recognized_le_deferred
            CHECK (recognized_minor <= total_deferred_minor),
        CONSTRAINT chk_ledger_recognition_schedule_status
            CHECK (status IN ('ACTIVE','COMPLETED','REPLACED','CANCELLED'))
    )",
    "CREATE UNIQUE INDEX ledger_recognition_schedule_live_idx
        ON ledger_recognition_schedule
            (tenant_id, source_invoice_id, source_invoice_item_ref, revenue_stream)
        WHERE status = 'ACTIVE'",
    "CREATE TABLE ledger_recognition_segment (
        tenant_id     text          NOT NULL,
        schedule_id   varchar(128)  NOT NULL,
        segment_no    integer       NOT NULL,
        period_id     varchar(64)   NOT NULL,
        amount_minor  bigint        NOT NULL,
        status        varchar(16)   NOT NULL,
        recognized_at text,
        run_id        text,
        PRIMARY KEY (tenant_id, schedule_id, segment_no),
        CONSTRAINT chk_ledger_recognition_segment_amount_nonneg
            CHECK (amount_minor >= 0),
        CONSTRAINT chk_ledger_recognition_segment_status
            CHECK (status IN ('PENDING','QUEUED','DONE'))
    )",
    "CREATE UNIQUE INDEX ledger_recognition_segment_period_idx
        ON ledger_recognition_segment (tenant_id, schedule_id, period_id)",
    "CREATE TABLE ledger_recognition_run (
        tenant_id      text          NOT NULL,
        run_id         text          NOT NULL,
        period_id      varchar(64)   NOT NULL,
        started_at_utc text          NOT NULL,
        status         varchar(16)   NOT NULL,
        PRIMARY KEY (tenant_id, period_id, run_id),
        CONSTRAINT chk_ledger_recognition_run_status
            CHECK (status IN ('RUNNING','DONE','FAILED'))
    )",
    "CREATE INDEX ledger_recognition_run_period_idx
        ON ledger_recognition_run (tenant_id, period_id)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_recognition_run",
    "DROP TABLE IF EXISTS ledger_recognition_segment",
    "DROP TABLE IF EXISTS ledger_recognition_schedule",
];

// ---------------------------------------------------------------------------
// Migration dispatch.
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_UP_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_UP_STATEMENTS,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        let statements: &[&str] = match backend {
            sea_orm::DatabaseBackend::Postgres => PG_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::Sqlite => SQLITE_DOWN_STATEMENTS,
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Migration(
                    "MySQL not supported for bss-ledger".to_owned(),
                ));
            }
        };
        for sql in statements {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
