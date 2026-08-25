//! Create `bss.pricing_read_model` — the published, frozen read model, stored
//! as **per-subject deltas** (`design/01-foundation.md` §3.7, D-86 / D-91) and
//! keyed `(tenant_id, catalog_version, subject_kind, subject_ref)`. A version's
//! rows are exactly the subjects of the publish units that produced it, never a
//! full tenant copy: interactive publishes coalesce on a 5s SLO, and a whole
//! catalog per version would multiply a tenant's catalog by its publish rate.
//!
//! `warm_completed` is a **per-row** marker (D-86/D-91), not a per-version one:
//! a version's row is ignored until both `CatalogVersionPublished` and that
//! row's marker are present.
//!
//! `idx_pricing_read_model_resolve` is the index resolution runs on:
//! `(tenant_id, subject_kind, subject_ref, catalog_version DESC)` turns
//! "resolve `(pin, subject)`" — the greatest completed version at or below the
//! pin — into a single indexed read, which is what keeps it inside the
//! p95 < 100ms order-time budget.
//!
//! There is deliberately **no** append-only trigger here. This table is a
//! projection, not truth: the projector rebuilds a row on a degraded-publish
//! re-drive, and the frozen-content guarantee is enforced upstream on the truth
//! tables it reads (`pricing_plan`, `pricing_price`). What it does carry is a
//! `CHECK` tying the completion marker to its timestamp, so a row can never be
//! warm-complete with no record of when it completed.
//!
//! **Backend differences.** `jsonb` becomes `text` on `SQLite` (its JSON1
//! functions read text fine, but there is no binary JSON type and no
//! jsonb-specific indexing); `boolean` is a `SQLite` affinity over 0/1 integers.
//! Both backends support the descending index column and the partial index.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_read_model (
            tenant_id         uuid        NOT NULL,
            catalog_version   bigint      NOT NULL,
            subject_kind      text        NOT NULL,
            subject_ref       text        NOT NULL,
            payload           jsonb       NOT NULL,
            projected_at      timestamptz NOT NULL DEFAULT now(),
            warm_completed    boolean     NOT NULL DEFAULT false,
            warm_completed_at timestamptz,
            CONSTRAINT chk_pricing_read_model_catalog_version CHECK (catalog_version >= 0),
            CONSTRAINT chk_pricing_read_model_subject_kind CHECK (subject_kind IN ('plan','price_overlay','overlay_index','group_membership')),
            CONSTRAINT chk_pricing_read_model_warm_marker CHECK (warm_completed = (warm_completed_at IS NOT NULL)),
            CONSTRAINT pricing_read_model_pkey PRIMARY KEY (tenant_id, catalog_version, subject_kind, subject_ref)
        )",
    "CREATE INDEX idx_pricing_read_model_resolve ON bss.pricing_read_model USING btree (tenant_id, subject_kind, subject_ref, catalog_version DESC)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_read_model"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_read_model (
            tenant_id         text    NOT NULL,
            catalog_version   bigint  NOT NULL,
            subject_kind      text    NOT NULL,
            subject_ref       text    NOT NULL,
            payload           text    NOT NULL,
            projected_at      text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            warm_completed    boolean NOT NULL DEFAULT false,
            warm_completed_at text,
            PRIMARY KEY (tenant_id, catalog_version, subject_kind, subject_ref),
            CONSTRAINT chk_pricing_read_model_catalog_version CHECK (catalog_version >= 0),
            CONSTRAINT chk_pricing_read_model_subject_kind CHECK (subject_kind IN ('plan','price_overlay','overlay_index','group_membership')),
            CONSTRAINT chk_pricing_read_model_warm_marker CHECK (warm_completed = (warm_completed_at IS NOT NULL))
        )",
    "CREATE INDEX idx_pricing_read_model_resolve ON pricing_read_model (tenant_id, subject_kind, subject_ref, catalog_version DESC)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_read_model"];

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
