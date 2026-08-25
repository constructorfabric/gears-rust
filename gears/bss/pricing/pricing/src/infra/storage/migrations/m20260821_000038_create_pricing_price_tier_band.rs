//! Create `bss.pricing_price_tier_band` — the **authored** tier bands of a
//! `graduated` / `volume` price row (`design/03-price-structure.md` §6). The
//! first Slice-3-owned table of the chain, and the only part of a price row
//! that is many-per-row and therefore cannot live on the row.
//!
//! **Authored bands only (D-130).** The D-45 allowance compile is a
//! *projection*: it materializes the `$0` first band, the offset bands and the
//! allowance marker into `pricing_read_model` / the snapshot, and it never
//! inserts, offsets or deletes a row here. Nothing else may either. The
//! pre-D-130 in-place rewrite destroyed its own input — after the first publish
//! the authored bounds were unrecoverable, so a re-publish, a supersession, a
//! repricing or a clone of an allowance row had nothing left to recompile
//! *from*, and `inst-ac-deterministic`'s "re-publish recompiles identically"
//! was unsatisfiable. This table holding exactly what the operator authored is
//! what makes the compile idempotent by construction.
//!
//! The key is `(price_id, from_qty)` and there is deliberately **no ordinal
//! column**: a band's identity is where it starts. `domain::rules::tier_bands`
//! judges the set sorted by `from_qty` for the same reason — authoring order
//! does not survive a round trip through this table, and a rule that read it
//! would let a row validate at save and fail the identical re-validation at
//! publish.
//!
//! **Ordering and contiguity are not constraints here, and must not become
//! ones.** Ascending order, gaplessness, non-overlap and the always-open top
//! are properties of the band set *as a sequence*: each is a statement about a
//! row and its neighbour, and neither a row CHECK nor a unique index can see a
//! neighbour. The `TierBandValidator` (`domain::rules::tier_bands`) owns them at
//! publish, where the whole set is in hand and every violation can be reported
//! at once. A constraint added here could only ever express a weaker rule
//! while looking like the real one. The two properties that *are* per-row —
//! non-negative bounds and a non-zero width — are CHECKs, because those a row
//! can answer alone.
//!
//! **Structural exclusivity is a trigger, not a CHECK** (§6). "Band rows are
//! forbidden unless `model_kind IN ('graduated','volume')`" reads the *parent*,
//! and a row CHECK may not read another table. Without it a `flat` row could
//! accumulate bands that no rule ever looks at and no rating ever applies —
//! silent, and indistinguishable from a correctly priced row until an invoice
//! is wrong.
//!
//! **And it is guarded from the parent side too**, which is why this migration
//! puts a trigger on `pricing_price`. The child-side arms only judge a band as
//! it arrives; nothing in them stops a **draft** parent's `model_kind` flipping
//! from `graduated` to `flat` while bands hang off it, which reaches the same
//! forbidden pair from the other end and leaves it there. It is unreachable
//! through `PriceRepo` — an update replaces the band set in the same
//! transaction, so the INSERT arm re-judges — but the ground under every
//! physical guard in this gear is that the engine is not the only thing that can
//! reach the table.
//!
//! §6 offers "a trigger **or a composite FK on `(price_id, model_kind)`**" for
//! this rule, and the FK is **not** taken. It would need `model_kind`
//! denormalized onto every band row — a column §6's own band-table shape does
//! not list — kept in step with the parent by the repository, and a unique index
//! on `(price_id, model_kind)` in the parent to be referenced. That is three new
//! things to keep true in order to say one thing a trigger says directly, and
//! the trigger route is available identically on both backends, so the two stay
//! mirrored rather than one carrying an FK the other emulates.
//!
//! The parent-side guard is what obliges `PriceRepo::update_draft` to replace
//! the band set **before** it moves the row: a legitimate edit that turns a
//! banded `graduated` row into a bandless `flat` one would otherwise pass
//! through the state this trigger forbids, and an authoring mistake nobody made
//! would surface as a storage failure.
//!
//! **Append-only with the parent** (`design/01-foundation.md` §3.7, the
//! 2026-07-31c L-2 fix: every revision-scoped child table carries the same
//! discipline as its parent — child rows are *immutable* once their revision
//! publishes). Bands have no `lifecycle_state` of their own — the parent price
//! row's is the referent — so **all three** DML verbs are rejected once the
//! parent leaves `draft`. Otherwise the band set of a frozen row could be
//! rewritten under an unchanged `pricing_price`, and the projector's warm
//! re-drive, which reads truth rows (§4.4), would quietly re-materialize a
//! `CatalogVersion` at different money — the same argument that put the
//! whitelist trigger on `pricing_price`, applied to the half of the row that
//! lives here.
//!
//! Immutable includes **no rows appended**, which is why INSERT is guarded and
//! not only UPDATE and DELETE: an INSERT is the one verb that adds money to a
//! frozen row, and the kind trigger — which does fire on INSERT — reads only
//! the parent's `model_kind`, so a `graduated` row that had already published
//! would have accepted a new band from any caller.
//!
//! An UPDATE is checked against **both** ends for the same reason. The band's
//! current parent (`OLD.price_id`) must be draft because the band is being
//! mutated out from under it; its prospective parent (`NEW.price_id`) must be
//! draft because re-pointing a band is how you would otherwise append one to a
//! frozen set without ever issuing an INSERT. The kind rule, by contrast, cares
//! only about where the band *ends up*, so it reads `NEW.price_id` alone.
//!
//! **Backend differences.** Postgres carries both rules as PL/pgSQL trigger
//! functions with the offending value interpolated; `SQLite` has no procedural
//! language and `RAISE(ABORT, ...)` takes a literal message, so each rule
//! becomes fixed-message triggers whose parent lookup is a `WHERE NOT EXISTS`
//! subquery in the trigger body rather than a `WHEN` clause. `uuid` becomes
//! `text` and the `bss.` qualification is dropped, as elsewhere in this chain.
//!
//! # `unit_price_nano` is a rate column, and the name is the unit
//!
//! It is a `bigint` counting **10⁻⁹ minor units**, not minor units. The distinction
//! is D-311's one level down: `amount_minor` on `pricing_price` once documented
//! itself as *"the single amount on `flat`, the unit price on `per_unit`"* — one
//! column meaning two things by `model_kind` — and the repair was to give the rate
//! its own column in its own scale. Keeping "an amount column holds amounts" true is
//! what the split buys; storing everything in the rate scale instead would have put
//! the invoice sum in the rate type and dissolved the distinction.
//!
//! `chk_pricing_price_tier_band_unit_price` (`unit_price_nano >= 0`) is this
//! column's own rule at the schema layer. What the schema does **not** hold is the
//! placement rule — nothing here ties a `model_kind` to the column it must price, on
//! either engine. That lives in `domain::rules` as `AMOUNT_PLACEMENT_INVALID`, and
//! saying otherwise here would credit the schema with a guard it does not have.
//!
//! **Splitting a column splits its rules**, and the frozen-column guard is the one
//! that must not be left behind: a price column outside the append-only whitelist is
//! a published row whose price can move.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_price_tier_band (
            tenant_id       uuid   NOT NULL,
            band_id         uuid   NOT NULL,
            from_qty        bigint NOT NULL,
            price_id        uuid   NOT NULL,
            to_qty          bigint,
            unit_price_nano bigint NOT NULL,
            CONSTRAINT chk_pricing_price_tier_band_from_qty CHECK (from_qty >= 0),
            CONSTRAINT chk_pricing_price_tier_band_unit_price CHECK (unit_price_nano >= 0),
            CONSTRAINT chk_pricing_price_tier_band_width CHECK (to_qty IS NULL OR to_qty > from_qty),
            CONSTRAINT fk_pricing_price_tier_band_price FOREIGN KEY (price_id) REFERENCES bss.pricing_price(price_id),
            CONSTRAINT pricing_price_tier_band_pkey PRIMARY KEY (band_id),
            CONSTRAINT uq_pricing_price_tier_band_lower_bound UNIQUE (price_id, from_qty)
        )",
    "CREATE INDEX idx_pricing_price_tier_band_price ON bss.pricing_price_tier_band USING btree (tenant_id, price_id)",
    "CREATE OR REPLACE FUNCTION bss.pricing_price_tier_band_append_only() RETURNS trigger AS $$
        DECLARE
          parent_state text;
        BEGIN
          IF TG_OP <> 'INSERT' THEN
            SELECT lifecycle_state INTO parent_state
              FROM bss.pricing_price WHERE price_id = OLD.price_id;
            IF parent_state IS DISTINCT FROM 'draft' THEN
              RAISE EXCEPTION
                'pricing_price_tier_band: % of a band under a % price row is not permitted',
                TG_OP, coalesce(parent_state, 'missing');
            END IF;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RETURN OLD;
          END IF;

          SELECT lifecycle_state INTO parent_state
            FROM bss.pricing_price WHERE price_id = NEW.price_id;
          IF parent_state IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION
              'pricing_price_tier_band: % of a band under a % price row is not permitted',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE OR REPLACE FUNCTION bss.pricing_price_tier_band_kind() RETURNS trigger AS $$
        DECLARE
          parent_kind text;
        BEGIN
          SELECT model_kind INTO parent_kind
            FROM bss.pricing_price WHERE price_id = NEW.price_id;
          IF parent_kind IS NULL OR parent_kind NOT IN ('graduated','volume') THEN
            RAISE EXCEPTION
              'pricing_price_tier_band: band rows are forbidden on a % price row',
              coalesce(parent_kind, 'kindless');
          END IF;
          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_price_tier_band_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_price_tier_band FOR EACH ROW EXECUTE FUNCTION bss.pricing_price_tier_band_append_only()",
    "CREATE TRIGGER trg_pricing_price_tier_band_kind BEFORE INSERT OR UPDATE ON bss.pricing_price_tier_band FOR EACH ROW EXECUTE FUNCTION bss.pricing_price_tier_band_kind()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_price_tier_band",
    "DROP FUNCTION IF EXISTS bss.pricing_price_tier_band_append_only()",
    "DROP FUNCTION IF EXISTS bss.pricing_price_tier_band_kind()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_price_tier_band (
            tenant_id       text   NOT NULL,
            band_id         text   NOT NULL,
            from_qty        bigint NOT NULL,
            price_id        text   NOT NULL,
            to_qty          bigint,
            unit_price_nano bigint NOT NULL,
            PRIMARY KEY (band_id),
            CONSTRAINT chk_pricing_price_tier_band_from_qty CHECK (from_qty >= 0),
            CONSTRAINT chk_pricing_price_tier_band_unit_price CHECK (unit_price_nano >= 0),
            CONSTRAINT chk_pricing_price_tier_band_width CHECK (to_qty IS NULL OR to_qty > from_qty),
            CONSTRAINT fk_pricing_price_tier_band_price FOREIGN KEY (price_id) REFERENCES pricing_price(price_id),
            CONSTRAINT uq_pricing_price_tier_band_lower_bound UNIQUE (price_id, from_qty)
        )",
    "CREATE INDEX idx_pricing_price_tier_band_price ON pricing_price_tier_band (tenant_id, price_id)",
    "CREATE TRIGGER trg_pricing_price_tier_band_kind_insert BEFORE INSERT ON pricing_price_tier_band FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_tier_band: band rows are permitted only on a graduated or volume price row') WHERE NOT EXISTS (SELECT 1 FROM pricing_price WHERE price_id = NEW.price_id AND model_kind IN ('graduated','volume')); END",
    "CREATE TRIGGER trg_pricing_price_tier_band_kind_update BEFORE UPDATE ON pricing_price_tier_band FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_tier_band: band rows are permitted only on a graduated or volume price row') WHERE NOT EXISTS (SELECT 1 FROM pricing_price WHERE price_id = NEW.price_id AND model_kind IN ('graduated','volume')); END",
    "CREATE TRIGGER trg_pricing_price_tier_band_no_delete BEFORE DELETE ON pricing_price_tier_band FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_tier_band: DELETE of a band under a non-draft price row is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price WHERE price_id = OLD.price_id AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_tier_band_no_insert BEFORE INSERT ON pricing_price_tier_band FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_tier_band: INSERT of a band under a non-draft price row is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price WHERE price_id = NEW.price_id AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_tier_band_no_update BEFORE UPDATE ON pricing_price_tier_band FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_tier_band: UPDATE of a band under a non-draft price row is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price WHERE price_id = OLD.price_id AND lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_price WHERE price_id = NEW.price_id AND lifecycle_state = 'draft'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_price_tier_band"];

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
