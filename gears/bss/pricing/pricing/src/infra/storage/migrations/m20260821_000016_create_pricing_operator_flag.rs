//! Create `bss.pricing_operator_flag` — the operator-plane drift / divergence
//! flags, keyed `(tenant_id, subject_ref, flag)`
//! (`design/01-foundation.md` §3.7).
//!
//! This table exists to keep operator-plane state **out of the versioned read
//! model** (D-85). A drift flag has no publish unit: consumers keep resolving
//! the frozen values, and writing the flag into `pricing_read_model` would be
//! an in-place mutation of an already-frozen `CatalogVersion` — exactly the
//! thing D-85 / D-99 forbid. Operators read the flags through the authoring
//! surfaces (`plan x read`) and the existing alarms instead.
//!
//! Clearing a flag is a **row DELETE**, not a status column: a flag is either
//! raised or it is not, and a `cleared` tombstone would make "is this subject
//! divergent" a question about the newest of several rows rather than about the
//! presence of one. The audit trail of raise/clear lives in
//! `pricing_audit_log`, which is where a durable history belongs.
//!
//! The physical guard is the enumeration `CHECK` on `flag`: the set is
//! slice-owned but closed, and a typo'd flag name would raise a divergence
//! nothing ever clears.
//!
//! **Backend differences.** None beyond the systematic type mirror.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_operator_flag (
            tenant_id   uuid        NOT NULL,
            subject_ref text        NOT NULL,
            flag        text        NOT NULL,
            detail      jsonb       NOT NULL DEFAULT '{}'::jsonb,
            set_at      timestamptz NOT NULL DEFAULT now(),
            set_by      uuid        NOT NULL,
            CONSTRAINT chk_pricing_operator_flag_name CHECK (flag IN ( 'tier_divergent', 'grants_divergent', 'tax_readiness_divergent', 'meter_binding_divergent')),
            CONSTRAINT pricing_operator_flag_pkey PRIMARY KEY (tenant_id, subject_ref, flag)
        )",
    "CREATE INDEX idx_pricing_operator_flag_by_flag ON bss.pricing_operator_flag USING btree (tenant_id, flag, set_at)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_operator_flag"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_operator_flag (
            tenant_id   text NOT NULL,
            subject_ref text NOT NULL,
            flag        text NOT NULL,
            detail      text NOT NULL DEFAULT '{}',
            set_at      text NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            set_by      text NOT NULL,
            PRIMARY KEY (tenant_id, subject_ref, flag),
            CONSTRAINT chk_pricing_operator_flag_name CHECK (flag IN ( 'tier_divergent', 'grants_divergent', 'tax_readiness_divergent', 'meter_binding_divergent'))
        )",
    "CREATE INDEX idx_pricing_operator_flag_by_flag ON pricing_operator_flag (tenant_id, flag, set_at)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_operator_flag"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP_STATEMENTS, SQLITE_UP_STATEMENTS).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            PG_DOWN_STATEMENTS,
            SQLITE_DOWN_STATEMENTS,
        )
        .await
    }
}
