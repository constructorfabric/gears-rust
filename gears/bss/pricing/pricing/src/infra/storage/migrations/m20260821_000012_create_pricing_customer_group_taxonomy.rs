//! Create `bss.pricing_customer_group_taxonomy` — the BSS customer-group value
//! universe (`design/09-price-overlays.md` §3 `inst-cg-taxonomy`, §6).
//!
//! # This is Slice 4's four shape, on Slice 9's own route
//!
//! `inst-cg-taxonomy` states the rule in one line: *"`GroupTaxonomy` is BSS-owned
//! and governed like region/brand (values validated at authoring; retire guarded
//! by referential checks)"*. This table is therefore the four Slice 4 taxonomies'
//! shape exactly — `pricing_org_tier_taxonomy`'s — and it carries no `tax_*` columns
//! for the reason `pricing_brand_taxonomy`'s doc records.
//!
//! What is **not** shared is the route. `design/09-price-overlays.md` §5 gives
//! this taxonomy its own pair, `GET/PUT /bss-pricing/v1/customer-groups/taxonomy`,
//! and `design/05-governance.md` (around the endpoint-mapping table) is explicit
//! that it is **not** filed under `config × write/read` with its four siblings:
//! *"the customer-group taxonomy is **not** here: it lives at
//! `/bss-pricing/v1/customer-groups/taxonomy` under `customer_group` (more
//! sensitive)"*. Per-payer membership is payer-level commercial data, and a
//! table addressable through the shared `{class}` route would be reachable by
//! every holder of `config × write` — exactly the widening the design set
//! segregates against. So `TaxonomyClass` (`crate::domain::taxonomy`) gains no
//! fifth arm; this table is validated and written through its own repository
//! arm and its own route (`crate::api::rest::customer_groups`).
//!
//! `ScopeClass::CustomerGroup` (`crate::domain::overlay`) already names this
//! table as its taxonomy (`ScopeClass::taxonomy_table`), which is what
//! `inst-plv-scope`'s validation and `inst-tx-mutation`'s retire guard both key
//! on. `overlay_repo`'s `declares` carries a `ScopeClass::CustomerGroup` arm that
//! reads this table, so the class resolves against a real universe rather than the
//! unconditional `false` D-223 recorded while it had none.
//!
//! **Backend differences.** As `pricing_region_taxonomy`: `bss.`-qualified DDL on
//! Postgres, unqualified on `SQLite`, both `CREATE TABLE` bodies otherwise
//! identical **but for the one predicate `SQLite` cannot spell the same way** —
//! `btrim(X, Y)` on Postgres, `trim(X, Y)` on `SQLite`, for the reason below.
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
    "CREATE TABLE bss.pricing_customer_group_taxonomy (
            tenant_id    uuid NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active'::text,
            CONSTRAINT chk_pricing_customer_group_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_customer_group_taxonomy_value_present CHECK ((length(btrim(value, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0)),
            CONSTRAINT pricing_customer_group_taxonomy_pkey PRIMARY KEY (tenant_id, value)
        )",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_customer_group_taxonomy"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_customer_group_taxonomy (
            tenant_id    text NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active',
            PRIMARY KEY (tenant_id, value),
            CONSTRAINT chk_pricing_customer_group_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_customer_group_taxonomy_value_present CHECK (length(trim(value, char(9,10,11,12,13,32))) > 0)
        )",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_customer_group_taxonomy"];

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
