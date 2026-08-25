//! Create `bss.pricing_region_taxonomy` — the region value universe
//! (`design/04-currency-tax.md` §6, D-01).
//!
//! The first of the four scope-value taxonomies §6 declares. It is **Slice 4's
//! table**, created on the Slice 9 chain, and the reason is
//! `inst-plv-scope`: every non-`global` `PriceOverlay` scope value validates
//! against its declared universe, and without these four tables none of those
//! universes exists anywhere in the crate. A validation rule whose universe is
//! absent is the defect D-211 paid an entry for — `inst-bc-coverage` delegating
//! the currency axis to a `CurrencyBindingChecker` that no crate declares, so an
//! implementer either calls into thin air or silently duplicates the check.
//! Building the universe was the alternative to shipping that shape again.
//!
//! Slice 4 inherits this table and its three siblings unchanged. What Slice 4
//! still owes them is the `GET/PUT /bss-pricing/v1/config/*-taxonomy` surface
//! and the **price-row** arm of the retire guard; the overlay arm of that guard
//! is `overlay_repo`'s and lands with this slice.
//!
//! # `tax_category` and `tax_rate_present` are on this table alone
//!
//! §6 is explicit — *"the `tax_*` columns below are region-only"* (D-01) — and
//! the three sibling migrations therefore do not carry them. That is a fact
//! about the schema rather than about a comment, so
//! `sqlite_taxonomy_store::the_other_three_taxonomies_carry_no_tax_columns`
//! asserts the absence by selecting the columns and requiring the parser to
//! refuse: a column's absence is not observable through any constraint.
//!
//! `tax_rate_present` defaults to **false**, which is the fail-closed reading of
//! the MVP `RegionTaxReadiness` source: a region nobody has declared a rate for
//! is a region with no rate, not a region with an unknown one.
//!
//! # A blank value is refused, and that is not tidiness
//!
//! `pricing_price_overlay` renders the **absence** of a scope value as the empty
//! string — the `global` class carries `scope_value = ''` and every other class
//! carries a non-empty one, under one biconditional CHECK. A blank taxonomy
//! value would make that sentinel forgeable: a `brand` overlay carrying `''`
//! would validate against a declared universe *and* read as classless. The
//! empty string denotes "no scope value" and nothing else may render it, which
//! is `pricing_price`'s scope-key argument for `COALESCE(meter, '')` applied one
//! table over.
//!
//! # No index beyond the primary key
//!
//! The only read `inst-plv-scope` makes is a point read on `(tenant_id, value)`,
//! which the primary key serves, and enumeration is a full scan of a table with
//! one row per declared region. An index nothing takes is a second thing to keep
//! true.
//!
//! **Backend differences.** `uuid` becomes `text`, `boolean` keeps its name but
//! takes `0`/`1`, and the `bss.` qualification is dropped, as elsewhere in this
//! chain. Every `CHECK` and the primary key are preserved on both sides. No
//! append-only trigger: a taxonomy value is editable in place by design —
//! `retired -> active` re-activation is an explicitly legal audited move (§6).
//!
//! # The value predicate is D-242's, and `length(value) > 0` is the wrong one
//!
//! `'   '` satisfies the loose form, so the store would admit a value the domain
//! refuses: `ScopeValue::new` **trims** before it decides. Measured on both engines
//! rather than reasoned — the register entry began as a Postgres case asserting a
//! refusal, and the insert landed.
//!
//! What a whitespace value costs is one level up from the sentinel above.
//! `TaxonomyRepo::list` maps a value `ScopeValue` refuses to `RepoError::CorruptRow`,
//! so **one** such row makes `GET /config/taxonomies/{class}` fail for **every** value
//! in that class, and the only remedy is direct SQL — the `PUT` cannot round-trip a
//! list it cannot read. The predicate stops the row existing rather than coping with
//! it, which is what makes the store agree with the domain type.
//!
//! It is a different hole from the one the constraint was written for. `'   '` was
//! never the classless sentinel — the empty string is rendered exactly — so
//! `inst-plv-scope` was never exposed by it.
//!
//! ## What the predicate strips, and what only the domain catches
//!
//! Both arms name the character set explicitly, and it is ASCII whitespace entire:
//! `chr(9)`/`char(9)` horizontal tab, `10` line feed, `11` vertical tab, `12` form
//! feed, `13` carriage return, `32` space. A value made only of those is refused;
//! `' EU '` lands, because the predicate asks whether a non-blank character survives
//! the trim rather than whether the value needed one.
//!
//! **The residue is non-ASCII whitespace, and it is the domain's alone.**
//! `ScopeValue::new` is Rust's `str::trim`, which strips every character carrying the
//! Unicode `White_Space` property — `U+0085`, `U+00A0`, `U+1680`, `U+2000`–`U+200A`,
//! `U+2028`, `U+2029`, `U+202F`, `U+205F`, `U+3000` beyond the set above. A value of
//! nothing but those satisfies these `CHECK`s and `ScopeValue::new` still refuses it,
//! so the `CorruptRow` D-242 records stays reachable by a database edited out of band
//! through exactly that residue. No writer in this gear can reach it: the REST `PUT`
//! refuses a blank value and the repo writes only an already-trimmed `ScopeValue`.
//!
//! Spelling the whole `White_Space` property into the predicate closes the residue and
//! costs a frozen copy of a Unicode table the Rust side re-reads from the standard
//! library at every toolchain bump — the two drift apart silently on a version bump
//! that touches neither arm, and the `CHECK` becomes the weaker of the pair again with
//! no edit to point at. `ZERO WIDTH SPACE` (`U+200B`) is not `White_Space` and is
//! admitted by both, which is agreement rather than residue.
//!
//! **The trim function is not spelled the same on the two backends.** Postgres has
//! `btrim(text, text)`; `SQLite` has no `btrim` at all and spells the same operation
//! `trim(X, Y)`, so the decision's `btrim` in both arms would give a `SQLite` chain
//! that fails at `CREATE TABLE` with `no such function`. The two spellings must stay
//! apart for the schema oracle as well: Postgres renders `trim(x)` as
//! `TRIM(BOTH FROM x)` and `btrim(x)` as itself, so one text for both engines moves
//! one engine's schema.
//!
//! The set is built out of `chr`/`char` calls rather than written as an escape
//! literal for the oracle's sake too: `pg_get_constraintdef` re-renders `E' \t\n\r'`
//! with the control characters themselves, which puts a raw tab and a raw newline
//! inside a constraint line of `tests/schema_golden/postgres.txt`.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &["CREATE TABLE bss.pricing_region_taxonomy (
            tenant_id        uuid    NOT NULL,
            value            text    NOT NULL,
            display_name     text    NOT NULL,
            state            text    NOT NULL DEFAULT 'active'::text,
            tax_category     text,
            tax_rate_present boolean NOT NULL DEFAULT false,
            CONSTRAINT chk_pricing_region_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_region_taxonomy_value_present CHECK ((length(btrim(value, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0)),
            CONSTRAINT pricing_region_taxonomy_pkey PRIMARY KEY (tenant_id, value)
        )"];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_region_taxonomy"];

const SQLITE_UP_STATEMENTS: &[&str] = &["CREATE TABLE pricing_region_taxonomy (
            tenant_id        text    NOT NULL,
            value            text    NOT NULL,
            display_name     text    NOT NULL,
            state            text    NOT NULL DEFAULT 'active',
            tax_category     text,
            tax_rate_present boolean NOT NULL DEFAULT 0,
            PRIMARY KEY (tenant_id, value),
            CONSTRAINT chk_pricing_region_taxonomy_state CHECK (state IN ('active', 'retired')),
            CONSTRAINT chk_pricing_region_taxonomy_value_present CHECK (length(trim(value, char(9,10,11,12,13,32))) > 0)
        )"];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_region_taxonomy"];

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
