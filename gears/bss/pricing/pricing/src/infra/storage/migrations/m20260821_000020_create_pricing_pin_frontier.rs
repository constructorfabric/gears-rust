//! Create `bss.pricing_pin_frontier` — the **materialized pin-eligibility
//! watermark**, one row per tenant (`design/01-foundation.md` §3.7 / §4.4,
//! D-136).
//!
//! Pin-eligibility as stated by D-101 + D-114 is a predicate over every subject
//! row of every version, and its prefix clause makes it recursive; evaluated
//! literally on the read path it is a scan of the delta store on a
//! p95 < 100ms budget, and `pricing.readmodel.pin_eligibility_overdue` would
//! have nothing to evaluate at all. So the frontier is stored, not recomputed:
//! the `ReadModelProjector` advances it in the **same transaction** that sets
//! the last outstanding `warm_completed` marker of the frontier's next version
//! in order. A later version's completion never advances it past a gap — that
//! is the D-114 prefix, enforced by construction rather than re-derived.
//!
//! The physical guard is `chk_pricing_pin_frontier_version`, and the forward-
//! only rule itself lives in the repository's conditional UPDATE
//! (`WHERE catalog_version < :to`), so even a lost race cannot walk the
//! watermark backwards. Backwards is the failure that matters: a receding
//! frontier would let one pin resolve two different contents over time, which
//! is the entire reason it is materialized.
//!
//! There is no append-only trigger: this row is a watermark and is *meant* to
//! be updated. What must never happen is a **decrease**, which a whitelist
//! trigger cannot express any better than the repository's guarded UPDATE does,
//! and which the repository additionally reports as a typed refusal so the
//! ordering bug behind it surfaces instead of being swallowed.
//!
//! **Backend differences.** None beyond the systematic type mirror.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.pricing_pin_frontier (
            tenant_id       uuid        NOT NULL,
            advanced_at     timestamptz NOT NULL,
            catalog_version bigint      NOT NULL,
            CONSTRAINT chk_pricing_pin_frontier_version CHECK (catalog_version >= 0),
            CONSTRAINT pricing_pin_frontier_pkey PRIMARY KEY (tenant_id)
        )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_pin_frontier"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE pricing_pin_frontier (
            tenant_id       text   NOT NULL,
            advanced_at     text   NOT NULL,
            catalog_version bigint NOT NULL,
            PRIMARY KEY (tenant_id),
            CONSTRAINT chk_pricing_pin_frontier_version CHECK (catalog_version >= 0)
        )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_pin_frontier"];

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
