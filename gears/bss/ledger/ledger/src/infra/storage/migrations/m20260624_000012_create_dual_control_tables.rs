//! Create the dual-control governance tables in schema `bss` (VHP-1852):
//! `ledger_approval` (one row per governed mutation that crossed a policy
//! threshold — the `PENDING → APPROVED | REJECTED | NEEDS_REWORK | CANCELLED |
//! EXPIRED` state machine + the deterministic `intent`), `ledger_dual_control_policy`
//! (per-tenant append-only effective-dated D2/A6/TTL thresholds), and
//! `ledger_approval_comment` (the append-only preparer↔approver thread + decision
//! reasons).
//!
//! Lock order (§4.3): `ledger_approval` is NOT a balance cache, so it carries no
//! projector `table_rank`; when locked in the decision txn it sits in the
//! pre-balance sequence `idempotency_dedup → ledger_approval → fiscal_period →
//! balance grains`. The comment table is never locked in the posting txn.
//!
//! Migration number `000012` deliberately skips `000011`, which is reserved by
//! Slice 4 (recognition) on the parallel branch, to avoid a merge collision.
//!
//! All CHECKs are created in final form up-front (Foundation §7.2). A partial
//! UNIQUE index `(tenant_id, kind, business_key) WHERE state IN ('PENDING',
//! 'NEEDS_REWORK')` is the idempotency guard (DC13): a preparer retry returns the
//! existing active record rather than a duplicate. `SQLite` mirrors the shape with
//! the systematic transforms (`uuid`→`text`, `timestamptz`→`text`, `jsonb`→`text`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// ---------------------------------------------------------------------------
// Postgres variant — canonical production schema (bss-qualified DDL).
// ---------------------------------------------------------------------------

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.ledger_approval (
        approval_id          uuid          NOT NULL,
        tenant_id            uuid          NOT NULL,
        kind                 varchar(32)   NOT NULL,
        state                varchar(16)   NOT NULL,
        revision             integer       NOT NULL DEFAULT 0,
        business_key         varchar(256)  NOT NULL,
        intent               jsonb         NOT NULL,
        amount_usd_eq_minor  bigint,
        threshold_snapshot   jsonb         NOT NULL,
        reason_code          varchar(128)  NOT NULL,
        prepared_by          uuid          NOT NULL,
        prepared_at          timestamptz   NOT NULL,
        approved_by          uuid,
        decided_at           timestamptz,
        correlation_id       uuid          NOT NULL,
        expires_at           timestamptz   NOT NULL,
        PRIMARY KEY (approval_id),
        CONSTRAINT chk_ledger_approval_kind CHECK (kind IN
            ('REVERSE','MATERIAL_BACKDATING','CREDIT_GRANT','CHARGEBACK_LOSS','PAYER_CLOSURE','PERIOD_REOPEN','RECOGNITION_SCHEDULE_CHANGE','REFUND','MANUAL_ADJUSTMENT','CREDIT_NOTE','DEBIT_NOTE')),
        CONSTRAINT chk_ledger_approval_state CHECK (state IN
            ('PENDING','APPROVING','APPROVED','REJECTED','NEEDS_REWORK','CANCELLED','EXPIRED')),
        CONSTRAINT chk_ledger_approval_revision_nonneg CHECK (revision >= 0),
        CONSTRAINT chk_ledger_approval_approver_distinct
            CHECK (approved_by IS NULL OR approved_by <> prepared_by)
    )",
    // The one-live idempotency guard (DC13) ALSO covers the transient `APPROVING`
    // latch (H2): while an approve is executing the mutation, the slot stays held
    // so no second active record for the same business key can be prepared.
    "CREATE UNIQUE INDEX uq_ledger_approval_active
        ON bss.ledger_approval (tenant_id, kind, business_key)
        WHERE state IN ('PENDING','NEEDS_REWORK','APPROVING')",
    "CREATE INDEX ix_ledger_approval_queue
        ON bss.ledger_approval (tenant_id, state, kind)",
    "CREATE INDEX ix_ledger_approval_expiry
        ON bss.ledger_approval (tenant_id, expires_at)
        WHERE state IN ('PENDING','NEEDS_REWORK')",
    "CREATE TABLE bss.ledger_dual_control_policy (
        tenant_id              uuid         NOT NULL,
        version                bigint       NOT NULL,
        effective_from         timestamptz  NOT NULL,
        d2_threshold_minor     bigint       NOT NULL,
        a6_backdating_biz_days integer      NOT NULL,
        pending_ttl_seconds    bigint       NOT NULL,
        created_at_utc         timestamptz  NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_ledger_dcpolicy_version_nonneg CHECK (version >= 0),
        CONSTRAINT chk_ledger_dcpolicy_d2_range
            CHECK (d2_threshold_minor BETWEEN 10000 AND 100000000),
        CONSTRAINT chk_ledger_dcpolicy_a6_range
            CHECK (a6_backdating_biz_days BETWEEN 1 AND 30),
        CONSTRAINT chk_ledger_dcpolicy_ttl_pos CHECK (pending_ttl_seconds > 0)
    )",
    "CREATE INDEX ix_ledger_dcpolicy_effective
        ON bss.ledger_dual_control_policy (tenant_id, effective_from)",
    "CREATE TABLE bss.ledger_approval_comment (
        comment_id    uuid          NOT NULL,
        approval_id   uuid          NOT NULL,
        tenant_id     uuid          NOT NULL,
        revision      integer       NOT NULL,
        author_actor  uuid          NOT NULL,
        body          text          NOT NULL,
        created_at    timestamptz   NOT NULL,
        PRIMARY KEY (comment_id),
        CONSTRAINT fk_ledger_approval_comment_approval
            FOREIGN KEY (approval_id) REFERENCES bss.ledger_approval (approval_id)
    )",
    "CREATE INDEX ix_ledger_approval_comment_thread
        ON bss.ledger_approval_comment (tenant_id, approval_id, created_at)",
    // Append-only enforcement (Postgres-only; mirrors the journal tables'
    // `bss.reject_mutation()` trigger in m20260619_000002). The comment thread is
    // the sole tamper-evident store of the dual-control decision reasons + the
    // preparer↔approver dialogue (the DC7 `secured_audit_record` stand-in until
    // Slice 6), so UPDATE/DELETE must be refused at the DB level, not merely
    // absent from the repo. The trigger function is created by the journal
    // migration (which runs earlier in the same gear), so we reference it here.
    "CREATE TRIGGER trg_ledger_approval_comment_append_only
        BEFORE UPDATE OR DELETE ON bss.ledger_approval_comment
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.ledger_approval_comment",
    "DROP TABLE IF EXISTS bss.ledger_dual_control_policy",
    "DROP TABLE IF EXISTS bss.ledger_approval",
];

// ---------------------------------------------------------------------------
// SQLite variant — non-production schema (unqualified; `uuid`/`timestamptz`/
// `jsonb`→`text`; CHECKs + PKs + indexes + partial-unique preserved).
// ---------------------------------------------------------------------------

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE ledger_approval (
        approval_id          text          NOT NULL,
        tenant_id            text          NOT NULL,
        kind                 varchar(32)   NOT NULL,
        state                varchar(16)   NOT NULL,
        revision             integer       NOT NULL DEFAULT 0,
        business_key         varchar(256)  NOT NULL,
        intent               text          NOT NULL,
        amount_usd_eq_minor  bigint,
        threshold_snapshot   text          NOT NULL,
        reason_code          varchar(128)  NOT NULL,
        prepared_by          text          NOT NULL,
        prepared_at          text          NOT NULL,
        approved_by          text,
        decided_at           text,
        correlation_id       text          NOT NULL,
        expires_at           text          NOT NULL,
        PRIMARY KEY (approval_id),
        CONSTRAINT chk_ledger_approval_kind CHECK (kind IN
            ('REVERSE','MATERIAL_BACKDATING','CREDIT_GRANT','CHARGEBACK_LOSS','PAYER_CLOSURE','PERIOD_REOPEN','RECOGNITION_SCHEDULE_CHANGE','REFUND','MANUAL_ADJUSTMENT','CREDIT_NOTE','DEBIT_NOTE')),
        CONSTRAINT chk_ledger_approval_state CHECK (state IN
            ('PENDING','APPROVING','APPROVED','REJECTED','NEEDS_REWORK','CANCELLED','EXPIRED')),
        CONSTRAINT chk_ledger_approval_revision_nonneg CHECK (revision >= 0),
        CONSTRAINT chk_ledger_approval_approver_distinct
            CHECK (approved_by IS NULL OR approved_by <> prepared_by)
    )",
    "CREATE UNIQUE INDEX uq_ledger_approval_active
        ON ledger_approval (tenant_id, kind, business_key)
        WHERE state IN ('PENDING','NEEDS_REWORK','APPROVING')",
    "CREATE INDEX ix_ledger_approval_queue
        ON ledger_approval (tenant_id, state, kind)",
    "CREATE INDEX ix_ledger_approval_expiry
        ON ledger_approval (tenant_id, expires_at)
        WHERE state IN ('PENDING','NEEDS_REWORK')",
    "CREATE TABLE ledger_dual_control_policy (
        tenant_id              text         NOT NULL,
        version                bigint       NOT NULL,
        effective_from         text         NOT NULL,
        d2_threshold_minor     bigint       NOT NULL,
        a6_backdating_biz_days integer      NOT NULL,
        pending_ttl_seconds    bigint       NOT NULL,
        created_at_utc         text         NOT NULL,
        PRIMARY KEY (tenant_id, version),
        CONSTRAINT chk_ledger_dcpolicy_version_nonneg CHECK (version >= 0),
        CONSTRAINT chk_ledger_dcpolicy_d2_range
            CHECK (d2_threshold_minor BETWEEN 10000 AND 100000000),
        CONSTRAINT chk_ledger_dcpolicy_a6_range
            CHECK (a6_backdating_biz_days BETWEEN 1 AND 30),
        CONSTRAINT chk_ledger_dcpolicy_ttl_pos CHECK (pending_ttl_seconds > 0)
    )",
    "CREATE INDEX ix_ledger_dcpolicy_effective
        ON ledger_dual_control_policy (tenant_id, effective_from)",
    "CREATE TABLE ledger_approval_comment (
        comment_id    text          NOT NULL,
        approval_id   text          NOT NULL,
        tenant_id     text          NOT NULL,
        revision      integer       NOT NULL,
        author_actor  text          NOT NULL,
        body          text          NOT NULL,
        created_at    text          NOT NULL,
        PRIMARY KEY (comment_id),
        CONSTRAINT fk_ledger_approval_comment_approval
            FOREIGN KEY (approval_id) REFERENCES ledger_approval (approval_id)
    )",
    "CREATE INDEX ix_ledger_approval_comment_thread
        ON ledger_approval_comment (tenant_id, approval_id, created_at)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS ledger_approval_comment",
    "DROP TABLE IF EXISTS ledger_dual_control_policy",
    "DROP TABLE IF EXISTS ledger_approval",
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
