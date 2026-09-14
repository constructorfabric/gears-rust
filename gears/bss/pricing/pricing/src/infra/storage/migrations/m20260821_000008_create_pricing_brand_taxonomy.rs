//! Create `bss.pricing_brand_taxonomy` — the brand value universe
//! (`design/04-currency-tax.md` §6).
//!
//! The second of the four scope-value taxonomies, and the one the design set
//! already referenced from Slice 9: `inst-plv-scope`'s first clause sends
//! `brand` to *"the Slice 4 brand taxonomy"*, and this is the table that sentence
//! names. `pricing_region_taxonomy`'s module doc carries the whole argument for
//! why the four are built here; this file states only what is its own.
//!
//! # It carries no `tax_*` columns, and the absence is the point
//!
//! §6 declares the four tables together and then narrows: *"the `tax_*` columns
//! below are region-only"* (D-01). A brand is a commercial label and has no
//! default tax category — a tax category rides the price row's
//! `tax_category_ref` (D-110) or the region's readiness, and a third place for
//! one to live is the cardinality error D-110 removed from
//! `pricing_plan_descriptor_set`.
//!
//! Everything else — the `(tenant_id, value)` key, the `active | retired` state,
//! the non-blank value, the absence of an append-only trigger — is
//! `pricing_region_taxonomy`'s and is stated there. `sqlite_taxonomy_store` proves each
//! rule against **all four** tables in one loop rather than asserting it of one
//! and assuming it of the rest, which is what keeps four near-identical
//! migrations from drifting.
//!
//! **Backend differences.** As `pricing_region_taxonomy`: `uuid` becomes `text` and the
//! `bss.` qualification is dropped. Every `CHECK` and the primary key are
//! preserved on both sides.
//!
//! # The value predicate is D-242's
//!
//! `length(btrim(value, <ascii whitespace>)) > 0` on Postgres,
//! `length(trim(value, <ascii whitespace>)) > 0` on `SQLite` — **not**
//! `length(value) > 0`, which admits `'   '`, and not the one-argument trim, which
//! strips spaces alone and admits a tab. `ScopeValue::new` refuses every one of
//! them. `pricing_region_taxonomy`'s doc carries the argument, the character set,
//! the residue only the domain catches, and the reason the two engines need two
//! spellings.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.pricing_brand_taxonomy (
            tenant_id    uuid NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active'::text,
            CONSTRAINT chk_pricing_brand_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_brand_taxonomy_value_present CHECK ((length(btrim(value, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0)),
            CONSTRAINT pricing_brand_taxonomy_pkey PRIMARY KEY (tenant_id, value)
        )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_brand_taxonomy"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE pricing_brand_taxonomy (
            tenant_id    text NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active',
            PRIMARY KEY (tenant_id, value),
            CONSTRAINT chk_pricing_brand_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_brand_taxonomy_value_present CHECK (length(trim(value, char(9,10,11,12,13,32))) > 0)
        )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_brand_taxonomy"];

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
