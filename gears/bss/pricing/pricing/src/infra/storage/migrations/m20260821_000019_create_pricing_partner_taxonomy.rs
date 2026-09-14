//! Create `bss.pricing_partner_taxonomy` — the partner value universe
//! (`design/04-currency-tax.md` §6, **D-120**).
//!
//! The third of the four, and one of the two D-120 added. What it closes is
//! stated in `inst-plv-scope` in as many words: *"before this the two classes
//! had no value universe anywhere: free-form strings on the axis that selects
//! who receives an adjustment"*. A `partner` overlay is how one reseller's book
//! is discounted and another's is not, and until D-120 the discriminator was
//! whatever string the author typed — so `acme`, `Acme` and `acme ` were three
//! partners, two of which no payer would ever match, and the overlay would
//! simply never fire rather than fail.
//!
//! `pricing_region_taxonomy`'s module doc carries the argument for building the four
//! here rather than waiting for Slice 4; this file states only what is its own.
//!
//! # The payer → partner resolution is deliberately **not** here
//!
//! D-120's third clause — who supplies the payer's partner standing at
//! evaluation — is a registered needs-decision on the Tariffs contract, and this
//! table does not presume an answer to it. What it declares is the universe an
//! **authored** scope value is validated against; matching a payer to a member
//! of that universe is evaluation, which is Tariffs'. The table would be the
//! same table under either answer, which is why it can land ahead of the
//! decision.
//!
//! It carries no `tax_*` columns, for `pricing_brand_taxonomy`'s reason.
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
    "CREATE TABLE bss.pricing_partner_taxonomy (
            tenant_id    uuid NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active'::text,
            CONSTRAINT chk_pricing_partner_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_partner_taxonomy_value_present CHECK ((length(btrim(value, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0)),
            CONSTRAINT pricing_partner_taxonomy_pkey PRIMARY KEY (tenant_id, value)
        )",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_partner_taxonomy"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE pricing_partner_taxonomy (
            tenant_id    text NOT NULL,
            value        text NOT NULL,
            display_name text NOT NULL,
            state        text NOT NULL DEFAULT 'active',
            PRIMARY KEY (tenant_id, value),
            CONSTRAINT chk_pricing_partner_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_partner_taxonomy_value_present CHECK (length(trim(value, char(9,10,11,12,13,32))) > 0)
        )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_partner_taxonomy"];

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
