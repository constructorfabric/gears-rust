//! Create `bss.pricing_org_tier_taxonomy` — the organisation-tier value
//! universe (`design/04-currency-tax.md` §6, **D-120**).
//!
//! The fourth of the four, and D-120's second addition. Its argument is
//! `pricing_partner_taxonomy`'s exactly: before D-120 the `orgTier` overlay class had no
//! declared universe, so the axis selecting who receives an adjustment was a
//! free-form string.
//!
//! # The name is `org_tier` here and `orgTier` in the design set
//!
//! §6 spells the table `pricing_org_tier_taxonomy` and the scope class
//! `orgTier`, and both spellings are kept: the physical name follows the
//! Foundation §3.7 snake-case rule the whole chain follows, and the scope class
//! is stored as `org_tier` in `pricing_price_overlay.scope_class` for the same
//! reason. The **wire** is `snake_case` too (`toolkit_macros::api_dto` does not
//! rename), so `orgTier` survives only in the design set's prose, where it names
//! the concept rather than a column or a field.
//!
//! It carries no `tax_*` columns, for `pricing_brand_taxonomy`'s reason, and
//! `pricing_region_taxonomy`'s module doc carries everything else the four share.
//!
//! **Backend differences.** As `pricing_region_taxonomy`.
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

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_org_tier_taxonomy (
            tenant_id    uuid NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active'::text,
            CONSTRAINT chk_pricing_org_tier_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_org_tier_taxonomy_value_present CHECK ((length(btrim(value, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0)),
            CONSTRAINT pricing_org_tier_taxonomy_pkey PRIMARY KEY (tenant_id, value)
        )",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_org_tier_taxonomy"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE pricing_org_tier_taxonomy (
            tenant_id    text NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active',
            PRIMARY KEY (tenant_id, value),
            CONSTRAINT chk_pricing_org_tier_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_org_tier_taxonomy_value_present CHECK (length(trim(value, char(9,10,11,12,13,32))) > 0)
        )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_org_tier_taxonomy"];

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
