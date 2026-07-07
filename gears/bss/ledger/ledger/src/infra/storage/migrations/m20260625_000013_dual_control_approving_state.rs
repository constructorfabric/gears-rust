//! Forward-fix for the dual-control `APPROVING` latch (H2) + the comment
//! append-only trigger (Z9-1) on clusters where `m20260624_000012` was ALREADY
//! applied with its original shape.
//!
//! `m20260624_000012` was amended in place to (a) add `APPROVING` to the
//! `ledger_approval.state` CHECK + the active-uniqueness partial index, and (b)
//! add the `ledger_approval_comment` append-only trigger. In-place edits to an
//! already-run migration do NOT re-apply (migrations run once, by name), so a
//! cluster that ran the original `000012` keeps the OLD CHECK — and the new
//! `approve` flow's `PENDING → APPROVING` transition then trips that CHECK (a 500).
//! This migration brings such a cluster up to date; it is **idempotent** (DROP …
//! IF EXISTS + re-create), so on a FRESH cluster — where `000012`'s amended form
//! already created the final shape — it is a harmless no-op re-statement.
//!
//! **Postgres-only.** `SQLite` is non-production and always migrates fresh from
//! the amended `000012` (which already carries the `APPROVING` CHECK + index; it
//! has no append-only trigger, by the same convention as the journal tables), so
//! the `SQLite` branch here is intentionally empty.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    // (Z9-1) Append-only trigger on the decision-audit comment thread — the sole
    // tamper-evident store of dual-control reasons until Slice 6 (mirrors the
    // journal tables' `bss.reject_mutation()` trigger). DROP IF EXISTS first so a
    // fresh cluster (where amended 000012 already created it) re-creates cleanly.
    "DROP TRIGGER IF EXISTS trg_ledger_approval_comment_append_only ON bss.ledger_approval_comment",
    "CREATE TRIGGER trg_ledger_approval_comment_append_only
        BEFORE UPDATE OR DELETE ON bss.ledger_approval_comment
        FOR EACH ROW EXECUTE FUNCTION bss.reject_mutation()",
    // (H2) Add the transient `APPROVING` latch state to the CHECK.
    "ALTER TABLE bss.ledger_approval DROP CONSTRAINT IF EXISTS chk_ledger_approval_state",
    "ALTER TABLE bss.ledger_approval ADD CONSTRAINT chk_ledger_approval_state CHECK (state IN
        ('PENDING','APPROVING','APPROVED','REJECTED','NEEDS_REWORK','CANCELLED','EXPIRED'))",
    // (H2) The one-live idempotency guard must also hold the `APPROVING` row so an
    // in-flight approve keeps the active-uniqueness slot.
    "DROP INDEX IF EXISTS bss.uq_ledger_approval_active",
    "CREATE UNIQUE INDEX uq_ledger_approval_active
        ON bss.ledger_approval (tenant_id, kind, business_key)
        WHERE state IN ('PENDING','NEEDS_REWORK','APPROVING')",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TRIGGER IF EXISTS trg_ledger_approval_comment_append_only ON bss.ledger_approval_comment",
    "ALTER TABLE bss.ledger_approval DROP CONSTRAINT IF EXISTS chk_ledger_approval_state",
    "ALTER TABLE bss.ledger_approval ADD CONSTRAINT chk_ledger_approval_state CHECK (state IN
        ('PENDING','APPROVED','REJECTED','NEEDS_REWORK','CANCELLED','EXPIRED'))",
    "DROP INDEX IF EXISTS bss.uq_ledger_approval_active",
    "CREATE UNIQUE INDEX uq_ledger_approval_active
        ON bss.ledger_approval (tenant_id, kind, business_key)
        WHERE state IN ('PENDING','NEEDS_REWORK')",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        // Postgres-only: SQLite migrates fresh from the amended 000012.
        if backend != sea_orm::DatabaseBackend::Postgres {
            return Ok(());
        }
        for sql in PG_UP_STATEMENTS {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();
        if backend != sea_orm::DatabaseBackend::Postgres {
            return Ok(());
        }
        for sql in PG_DOWN_STATEMENTS {
            conn.execute(Statement::from_string(backend, (*sql).to_owned()))
                .await?;
        }
        Ok(())
    }
}
