//! `pricing_policy_object` — the tenant policy objects
//! (`design/01-foundation.md` §3.7): the tax-display policy, the optional tenant
//! **default rounding policy**, the enforced-migration notice period, and
//! **every other per-tenant configurable this gear promises** (D-152).
//!
//! **Not the approval threshold.** A threshold is scoped, versioned and
//! tombstoned, and none of those can be carried by a column on a
//! one-row-per-tenant object — so it is `pricing_approval_threshold`, a table of
//! its own. Read that table, not this one.
//!
//! Every default here is the **fail-safe** one, and the nullability encodes it.
//! `tax_display_policy_mode` defaults to `fail_closed` because a tenant who has
//! not chosen must not have a display mode chosen quietly on their behalf; `warn`
//! is the deliberate opt-out. `default_rounding_policy_ref` is nullable for the
//! mirror-image reason — a tenant without one simply requires every published row
//! to carry its own `rounding_policy_ref`, and an unresolved policy fails publish
//! with `ROUNDING_POLICY_UNRESOLVED` rather than picking a rounding mode quietly
//! (PRD §17.4).
//!
//! The physical guard is the notice-period floor
//! (`enforced_migration_notice_days >= 60`, D-49). Slice 11 validates it again at
//! scheduling, but a floor that lives only in application code is one migration
//! script away from being bypassed.
//!
//! # The five D-152 columns, and the defect they close
//!
//! Four ratified numbers (PRD §7.1 `nfr-size-limits` — 100 tier bands per row,
//! 500 price rows per plan, 366 days and 24 months of custom interval) and the
//! descriptor required-set extension (S2 P5 / `inst-ds-sufficient`) were each
//! described as **tenant-configurable** while naming no store at all. The only
//! carrier the code had was the gear's configuration section, which is per
//! **deployment**: every tenant of a deployment shared one cap, and the
//! descriptor set's "config-extensible without a schema change" promise had
//! nowhere to declare the extension, so a fourth required key could only arrive
//! as a migration against the very table Billing's countersign is still pending
//! on. Sharing one required-set across a deployment is the sharper half — it
//! would let one deployment's tenants share a Billing contract they do not
//! share.
//!
//! **The four cap columns are nullable and the absent reading is the ratified
//! launch value**, taken from the deployment section, so nothing about the
//! ratified numbers moves for a tenant that configures nothing. Each carries a
//! positivity `CHECK` for the reason the configuration section refuses a zero at
//! boot: a zero band or row cap makes every plan unpublishable and a zero
//! interval cap makes every custom frequency unpublishable, and a cap that
//! rejects everything looks exactly like a cap that is switched on.
//!
//! `additional_required_descriptors` is a JSON **array of key names**, matched
//! against `pricing_plan_descriptor_set.additional_fields` and additive over the
//! pinned v1 three — `jsonb` on Postgres and `text` on `SQLite`, the same
//! transform the add-on edge sets and `included_allowance` take. It is
//! `NOT NULL DEFAULT '[]'` because an empty array is what "no extension" is, and
//! a nullable column would give that state two spellings. There is deliberately
//! no column for the v1 three: they are pinned by D-48, the extension is
//! additive-only, and a stored set that could *drop* one would let a tenant
//! publish past a pinned element of Billing's contract.
//!
//! **The carrier is provisional** (D-152's veto confirmation, 2026-08-03). These
//! five columns live in a pricing table because there is no settings gear in
//! this repository to hold them; `gears/simple-user-settings` is not that gear
//! (its rows are keyed per **user**, so a tenant-wide cap has no row to occupy).
//! Expect the move, and do not read these columns as the claim that a per-tenant
//! cap belongs in a pricing gear's policy table.
//!
//! **Backend differences.** None beyond the systematic type mirror
//! (`uuid` -> `text`, `timestamptz` -> `text`, `jsonb` -> `text`).
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_policy_object (
            tenant_id                       uuid        NOT NULL,
            additional_required_descriptors jsonb       NOT NULL DEFAULT '[]'::jsonb,
            default_rounding_policy_ref     text,
            enforced_migration_notice_days  integer     NOT NULL DEFAULT 60,
            max_custom_interval_days        integer,
            max_custom_interval_months      integer,
            max_price_rows_per_plan         integer,
            max_tier_bands_per_row          integer,
            tax_display_policy_mode         text        NOT NULL DEFAULT 'fail_closed'::text,
            updated_at_utc                  timestamptz NOT NULL DEFAULT now(),
            updated_by                      uuid        NOT NULL,
            CONSTRAINT chk_pricing_policy_object_interval_days_cap CHECK (max_custom_interval_days IS NULL OR max_custom_interval_days > 0),
            CONSTRAINT chk_pricing_policy_object_interval_months_cap CHECK (max_custom_interval_months IS NULL OR max_custom_interval_months > 0),
            CONSTRAINT chk_pricing_policy_object_notice_floor CHECK (enforced_migration_notice_days >= 60),
            CONSTRAINT chk_pricing_policy_object_price_row_cap CHECK (max_price_rows_per_plan IS NULL OR max_price_rows_per_plan > 0),
            CONSTRAINT chk_pricing_policy_object_tax_display_policy CHECK (tax_display_policy_mode IN ('fail_closed', 'warn')),
            CONSTRAINT chk_pricing_policy_object_tier_band_cap CHECK (max_tier_bands_per_row IS NULL OR max_tier_bands_per_row > 0),
            CONSTRAINT pricing_policy_object_pkey PRIMARY KEY (tenant_id)
        )",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_policy_object"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_policy_object (
            tenant_id                       text    NOT NULL,
            additional_required_descriptors text    NOT NULL DEFAULT '[]',
            default_rounding_policy_ref     text,
            enforced_migration_notice_days  integer NOT NULL DEFAULT 60,
            max_custom_interval_days        integer,
            max_custom_interval_months      integer,
            max_price_rows_per_plan         integer,
            max_tier_bands_per_row          integer,
            tax_display_policy_mode         text    NOT NULL DEFAULT 'fail_closed',
            updated_at_utc                  text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            updated_by                      text    NOT NULL,
            PRIMARY KEY (tenant_id),
            CONSTRAINT chk_pricing_policy_object_interval_days_cap CHECK (max_custom_interval_days IS NULL OR max_custom_interval_days > 0),
            CONSTRAINT chk_pricing_policy_object_interval_months_cap CHECK (max_custom_interval_months IS NULL OR max_custom_interval_months > 0),
            CONSTRAINT chk_pricing_policy_object_notice_floor CHECK (enforced_migration_notice_days >= 60),
            CONSTRAINT chk_pricing_policy_object_price_row_cap CHECK (max_price_rows_per_plan IS NULL OR max_price_rows_per_plan > 0),
            CONSTRAINT chk_pricing_policy_object_tax_display_policy CHECK (tax_display_policy_mode IN ('fail_closed', 'warn')),
            CONSTRAINT chk_pricing_policy_object_tier_band_cap CHECK (max_tier_bands_per_row IS NULL OR max_tier_bands_per_row > 0)
        )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_policy_object"];

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
