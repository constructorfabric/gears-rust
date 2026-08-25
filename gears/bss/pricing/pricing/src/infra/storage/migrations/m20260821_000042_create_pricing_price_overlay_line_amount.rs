//! Create `bss.pricing_price_overlay_line_amount` — the per-currency values of
//! an amount-based line (`design/09-price-overlays.md` §6, D-08, D-67).
//!
//! An **amount-based** magnitude is money and exists only per currency (D-08,
//! no-implicit-FX): the catalog never converts, so a line whose magnitude is
//! absolute carries one value per currency its resolved target scope sells, and
//! a missing one fails save and publish (`ADJUSTMENT_CURRENCY_NOT_COVERED`).
//! A **percent** magnitude is currency-neutral and lives on the line itself as
//! `adjustment_value`, so a percent line has no rows here at all — which is what
//! `chk_pricing_price_overlay_line_magnitude_pairing` makes physical one table
//! up.
//!
//! # The key carries the revision, because §6 says this table rides one
//!
//! §6 says `UNIQUE (line_id, currency)` and, in the same sentence one table up,
//! that the amount table *"rides the same revision through its line"*. Once
//! `pricing_price_overlay_line` is keyed `(line_id, overlay_revision)` — see
//! that migration's doc for why §6's `PK line_id` is not buildable beside D-92's
//! stable line identity — the pair alone cannot reference a line, and a value
//! set that did not carry the revision would be **shared** by every revision of
//! its line rather than riding one. So the key is
//! `(line_id, overlay_revision, currency)` and the foreign key is the pair.
//!
//! It is spelled as the **primary key** rather than as a surrogate plus a unique
//! index, because the triple *is* the row's identity: there is no such thing as
//! two values of one line's revision in one currency, and a surrogate would
//! invite a second row to exist and be ignored. Nothing references this table,
//! so nothing needs a narrower handle.
//!
//! # `value_minor >= 0` is D-67 and not the money rule
//!
//! `DESIGN` §2's `>= 0` binds authored **price rows**; D-67 states the overlay
//! line's own bound, and this `CHECK` is its physical half — *"amount magnitudes
//! `>= 0` at the currency's ISO 4217 minor unit"*. **Zero is admitted**, and
//! deliberately: a `fixed 0` line is how a market is priced at nothing, which is
//! a real authoring act, while the bp magnitudes one table up are refused at
//! zero because a `markup` of 0 bp adjusts nothing at all. The two bounds differ
//! because the two kinds mean different things by zero.
//!
//! **The minor-unit precision itself is not checkable here.** `value_minor` is
//! already an integer count of minor units, so every integer satisfies "at the
//! currency's ISO 4217 minor unit" at every precision — the same observation
//! `DomainError::ThresholdInvalid` records about §6's `absolute_minor` rule.
//! What *is* checkable is that the currency is a currency, and
//! `chk_..._currency` holds the ISO 4217 shape; the code's validity is
//! `domain::money::CurrencyCode`'s and is checked at the authoring edge.
//!
//! # The `OLD` arm keys on the **pair**, and the revision half is not decoration
//!
//! Every arm of every trigger here resolves its parent by
//! `(line_id, overlay_revision)`, never by `line_id` alone. D-92's
//! copy-on-new-revision deliberately reuses one `line_id` across revisions, so
//! *"some revision of this line sits under a draft"* and *"this row's revision
//! is a draft"* are different questions and only the second is the rule.
//!
//! The `UPDATE` trigger's `OLD` arm is the one place where dropping the half is
//! invisible: the row's *destination* is a legal draft, so the `NEW` arm passes
//! and the whole guard rests on the arm asking where the row comes **from**. With
//! `line_id` alone, `UPDATE … SET overlay_revision = <the open draft>` moves a
//! **published** revision's money off it, and the overlay then resolves without
//! that line for those currencies (D-42's amount-incomplete fallback) at a
//! frozen `CatalogVersion`.
//!
//! `sqlite_overlay_store::a_published_revisions_amounts_cannot_be_re_pointed_onto_a_draft`
//! and its Postgres twin are that statement, and they are the only cases the
//! `OLD` arm alone can refuse.
//!
//! # The append-only trigger reaches its parent through the line
//!
//! An amount rides the same revision its line does (§6: *"the amount table below
//! rides the same revision through its line"*), so the freeze is the line's and
//! the lookup is one join longer than `pricing_price_overlay_line`'s. Without it the
//! line's freeze would be worth nothing — the line would be immutable and its
//! money still editable.
//!
//! # The key and the guard are both tenant-scoped, and neither was obvious
//!
//! The key is `(tenant_id, overlay_revision, line_id, currency)` and the foreign key
//! `(tenant_id, overlay_revision, line_id)` — both widened by D-340 in the same act
//! that widened the parent line's. A narrower key here would have collided on two
//! tenants' amounts the moment their lines stopped colliding, which is the condition
//! review A1-4 records as arming `overlay_repo`'s untyped insert catch-all.
//!
//! **`line_id` is client-supplied, and supplying it is the documented usage.**
//! `api/rest/overlays.rs` renders it `request.line_id.unwrap_or_else(Uuid::now_v7)`
//! and the route description names the read-modify-write round trip as the intended
//! flow, because that is what makes D-92's *"the identity is stable"* true of an
//! authored edit and not only of the server's copy-forward. It is one of exactly
//! three client-supplied ids in this schema, beside `phase_id` and `composite_id`.
//! Under a narrow key the refusal here is already **typed** —
//! `overlay_repo::is_line_identity_collision` gates on `is_unique_violation()` and
//! the caller gets `ValueOutOfRange` rather than a `500` — but a 4xx that
//! discriminates is still an oracle: the difference between it and a `200` answers
//! *is this line id in use somewhere I cannot read*. And the first tenant to take a
//! line id at a revision number would hold it against every other tenant, at that
//! number, permanently.
//!
//! **The trigger bodies carry the tenant conjunct too**, which no other key in this
//! chain needs. The guard resolves its parent line by `(line_id, overlay_revision)`,
//! and that pair is unambiguous only while it is globally unique: without
//! `l.tenant_id = …`, an amount under a *published* line would find another tenant's
//! *draft* line carrying the same pair and be admitted. All three `SQLite` bodies and
//! the Postgres function carry it.
//!
//! Dependency level 2.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_price_overlay_line_amount (
            tenant_id        uuid   NOT NULL,
            overlay_revision bigint NOT NULL,
            line_id          uuid   NOT NULL,
            currency         text   NOT NULL,
            value_minor      bigint NOT NULL,
            CONSTRAINT chk_pricing_price_overlay_line_amount_currency CHECK (length(currency) = 3),
            CONSTRAINT chk_pricing_price_overlay_line_amount_value_minor CHECK (value_minor >= 0),
            CONSTRAINT fk_pricing_price_overlay_line_amount_line FOREIGN KEY (tenant_id, overlay_revision, line_id) REFERENCES bss.pricing_price_overlay_line(tenant_id, overlay_revision, line_id),
            CONSTRAINT pricing_price_overlay_line_amount_pkey PRIMARY KEY (tenant_id, overlay_revision, line_id, currency)
        )",
    "CREATE INDEX idx_pricing_price_overlay_line_amount_tenant ON bss.pricing_price_overlay_line_amount USING btree (tenant_id, line_id)",
    "CREATE OR REPLACE FUNCTION bss.pricing_price_overlay_line_amount_append_only()
        RETURNS trigger AS $$
        DECLARE
          parent_state text;
        BEGIN
          IF TG_OP <> 'INSERT' THEN
            SELECT o.lifecycle_state INTO parent_state
              FROM bss.pricing_price_overlay_line l
              JOIN bss.pricing_price_overlay o
                ON o.price_overlay_id = l.price_overlay_id
               AND o.revision = l.overlay_revision
             WHERE l.line_id = OLD.line_id AND l.overlay_revision = OLD.overlay_revision
               AND l.tenant_id = OLD.tenant_id;
            IF parent_state IS DISTINCT FROM 'draft' THEN
              RAISE EXCEPTION
                'pricing_price_overlay_line_amount: % of a value under a non-draft overlay revision is not permitted (state %)',
                TG_OP, coalesce(parent_state, 'missing');
            END IF;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RETURN OLD;
          END IF;

          SELECT o.lifecycle_state INTO parent_state
            FROM bss.pricing_price_overlay_line l
            JOIN bss.pricing_price_overlay o
              ON o.price_overlay_id = l.price_overlay_id
             AND o.revision = l.overlay_revision
           WHERE l.line_id = NEW.line_id AND l.overlay_revision = NEW.overlay_revision
             AND l.tenant_id = NEW.tenant_id;
          IF parent_state IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION
              'pricing_price_overlay_line_amount: % of a value under a non-draft overlay revision is not permitted (state %)',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_price_overlay_line_amount_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_price_overlay_line_amount FOR EACH ROW EXECUTE FUNCTION bss.pricing_price_overlay_line_amount_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_price_overlay_line_amount",
    "DROP FUNCTION IF EXISTS bss.pricing_price_overlay_line_amount_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_price_overlay_line_amount (
            tenant_id        text   NOT NULL,
            overlay_revision bigint NOT NULL,
            line_id          text   NOT NULL,
            currency         text   NOT NULL,
            value_minor      bigint NOT NULL,
            PRIMARY KEY (tenant_id, overlay_revision, line_id, currency),
            CONSTRAINT chk_pricing_price_overlay_line_amount_currency CHECK (length(currency) = 3),
            CONSTRAINT chk_pricing_price_overlay_line_amount_value_minor CHECK (value_minor >= 0),
            CONSTRAINT fk_pricing_price_overlay_line_amount_line FOREIGN KEY (tenant_id, overlay_revision, line_id) REFERENCES pricing_price_overlay_line(tenant_id, overlay_revision, line_id)
        )",
    "CREATE INDEX idx_pricing_price_overlay_line_amount_tenant ON pricing_price_overlay_line_amount (tenant_id, line_id)",
    "CREATE TRIGGER trg_pricing_price_overlay_line_amount_no_delete BEFORE DELETE ON pricing_price_overlay_line_amount FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line_amount: DELETE of a value under a non-draft overlay revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price_overlay_line l JOIN pricing_price_overlay o ON o.price_overlay_id = l.price_overlay_id AND o.revision = l.overlay_revision WHERE l.line_id = OLD.line_id AND l.overlay_revision = OLD.overlay_revision AND l.tenant_id = OLD.tenant_id AND o.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_line_amount_no_insert BEFORE INSERT ON pricing_price_overlay_line_amount FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line_amount: INSERT of a value under a non-draft overlay revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price_overlay_line l JOIN pricing_price_overlay o ON o.price_overlay_id = l.price_overlay_id AND o.revision = l.overlay_revision WHERE l.line_id = NEW.line_id AND l.overlay_revision = NEW.overlay_revision AND l.tenant_id = NEW.tenant_id AND o.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_line_amount_no_update BEFORE UPDATE ON pricing_price_overlay_line_amount FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line_amount: UPDATE of a value under a non-draft overlay revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price_overlay_line l JOIN pricing_price_overlay o ON o.price_overlay_id = l.price_overlay_id AND o.revision = l.overlay_revision WHERE l.line_id = OLD.line_id AND l.overlay_revision = OLD.overlay_revision AND l.tenant_id = OLD.tenant_id AND o.lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_price_overlay_line l JOIN pricing_price_overlay o ON o.price_overlay_id = l.price_overlay_id AND o.revision = l.overlay_revision WHERE l.line_id = NEW.line_id AND l.overlay_revision = NEW.overlay_revision AND l.tenant_id = NEW.tenant_id AND o.lifecycle_state = 'draft'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_price_overlay_line_amount"];

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
