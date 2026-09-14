//! Create `bss.pricing_plan_period_floor_cap` — the plan-level **period floor
//! and cap** per sold market (`design/02-plan-definition.md` §6, **D-319**),
//! keyed `(plan_id, plan_revision, currency, region)`. Slice 2's fourth
//! revision-scoped child table.
//!
//! # What this table is, and the one it must not be confused with
//!
//! A period floor is *"this plan bills at least X per period in this market"*.
//! It is **money compared against a period total**, not a price: Rating emits a
//! `PeriodFloorCapObligation` from the pinned snapshot and **Billing** executes
//! `max(total, floor)` / `min(total, cap)` after step 9 (rating PRD
//! `fr-period-floor-cap-obligation`). Nothing in this gear evaluates it.
//!
//! It is **not** `min_qty_purchase` / `min_qty_usage` / `minQtyThreshold`,
//! which are floors on **quantity** and live on `pricing_price`; conflating the
//! two is what rating §6.2 forbids. It is **not** a committed-spend pool
//! either — negotiated commitments are Contracts' system of record
//! (`commitmentPools[]`, rating T-D-14), and this is a self-service catalog
//! field with no true-up.
//!
//! # Why a table of its own, and why keyed on the market
//!
//! `pricing_plan` has no market axis: `(currency, region)` live on the price
//! row's canonical scope key. A plan-level floor per `(currency, region)` — the
//! shape [`../PRD.md`](../../../../docs/PRD.md) §17.8 names — therefore has
//! nowhere to sit on the plan row, and a currency-scalar column on
//! `pricing_plan` could denominate the floor in exactly one currency: a plan
//! selling USD and EUR would carry a floor that silently applies to one market
//! and not the other, or an implicit FX conversion this gear refuses to make
//! (no implicit FX, `currencyFallbackPolicy` is fail-closed).
//!
//! The market pair being **in the key** is also what makes the two amount rules
//! expressible as `CHECK`s at all. A column bolted onto an existing table could not
//! carry one:
//! `SQLite` cannot `ALTER TABLE ... ADD CONSTRAINT`, so a `CHECK` added on
//! Postgres alone would leave the two engines' `EXPECTED_CHECKS` censuses
//! describing different schemas, and the invariant has to move to a companion
//! guard trigger instead. A `CREATE TABLE` has no such problem — every
//! constraint below is in the table body, identical on both engines.
//!
//! # The five constraints, and why each is here rather than in the pipeline
//!
//! The house rule is that a **per-row** property is a `CHECK` and a **set or
//! graph** property is a pipeline rule. All five below are properties of one
//! row, and — unlike a phase graph, which is authored across successive
//! `PATCH`es — a floor row is authored **whole**: `PlanShapeRepo` replaces the
//! set wholesale, so there is no half-authored state for a constraint to
//! refuse.
//!
//! 1. `chk_..._floor_positive` and 2. `chk_..._cap_positive` — a bound is
//!    strictly positive when present. **`0` is refused rather than accepted as
//!    a second spelling of absence** (D-319): the per-line non-negative guard
//!    already holds every line at or above zero before floor/cap is applied
//!    (rating PRD §6.11), so `max(total, 0)` is a no-op by construction and an
//!    author who wrote it would believe they had set a minimum. This is the
//!    opposite call from a **price**, where an explicit `$0` row and an absent
//!    row are genuinely different states (a free market versus an unrateable
//!    one, S3 Q5) — there the two spellings mean two things, and here they
//!    mean one.
//! 3. `chk_..._ordered` — a floor above its cap describes a period total that
//!    must be both at least X and at most less-than-X. Written with both NULL
//!    arms explicit, because SQL's NULL propagation would otherwise make the
//!    comparison silently satisfied whenever either side is absent — the shape
//!    `chk_pricing_plan_phase_display_trial_days` fell into (D-151).
//! 4. `chk_..._present` — a row authoring neither bound says nothing. It is
//!    not a draft state: the market pair is the key, so an author removing both
//!    bounds removes the row.
//! 5. `chk_..._currency` — an ISO 4217 alphabetic code is exactly three
//!    characters, and `CurrencyCode::new` already requires that of every
//!    request. This `CHECK` is the store's half of that rule, and it is stated
//!    here rather than assumed because this column had **neither** half. Three
//!    other tables carry a `currency`: `pricing_approval_threshold` and
//!    `pricing_price_overlay_line_amount` bound it with this same `CHECK`, and
//!    `pricing_price` is bounded on Postgres by its `varchar(3)` — a bound
//!    `SQLite` discards, since the length in a `varchar(n)` is dropped by type
//!    affinity. This column was `text` with nothing on **either** engine, which
//!    made it the one place in the schema where the engine that runs in
//!    production held no bound at all. `length()` counts characters on both
//!    engines, so the guard means the same thing on each.
//!
//! What is **not** here: that the row's `(currency, region)` is a market the
//! plan actually sells. That is a property of the *plan's row set*, not of this
//! row, it is unknowable to this table, and it is `PERIOD_FLOOR_CAP_MARKET_UNSOLD`
//! at publish.
//!
//! # Append-only with its revision (`01-foundation.md` §3.7)
//!
//! Identical in every respect to `pricing_plan_phase`'s arrangement, and for
//! its reasons: no `lifecycle_state` of its own, the parent revision's is the
//! referent, INSERT guarded as well as UPDATE and DELETE, and the UPDATE arm
//! checking **both** ends because re-pointing `plan_revision` is how one would
//! append to a frozen revision without issuing an INSERT. The two orderings
//! that follow are mandatory: `PlanRepo::abandon_draft` drops these rows
//! **before** flipping the revision (`abandoned` is not `draft`), and
//! `PlanRepo::open_revision` inserts the new revision row **before** copying
//! them (the INSERT arm reads the *new* parent).
//!
//! **Backend differences.** As elsewhere in this chain: `bss.` dropped,
//! `uuid` -> `text`, and the single PL/pgSQL trigger function split into three
//! fixed-message `RAISE(ABORT, ...)` triggers whose parent lookup is a
//! `WHERE NOT EXISTS` subquery in the trigger **body**. Every `CHECK`, the FK,
//! the index and the PK are preserved on both sides.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_plan_period_floor_cap (
            tenant_id     uuid   NOT NULL,
            plan_id       uuid   NOT NULL,
            plan_revision bigint NOT NULL,
            currency      text   NOT NULL,
            region        text   NOT NULL,
            cap_minor     bigint,
            floor_minor   bigint,
            CONSTRAINT chk_pricing_plan_period_floor_cap_cap_positive CHECK (cap_minor IS NULL OR cap_minor > 0),
            CONSTRAINT chk_pricing_plan_period_floor_cap_currency CHECK (length(currency) = 3),
            CONSTRAINT chk_pricing_plan_period_floor_cap_floor_positive CHECK (floor_minor IS NULL OR floor_minor > 0),
            CONSTRAINT chk_pricing_plan_period_floor_cap_ordered CHECK (floor_minor IS NULL OR cap_minor IS NULL OR floor_minor <= cap_minor),
            CONSTRAINT chk_pricing_plan_period_floor_cap_present CHECK (floor_minor IS NOT NULL OR cap_minor IS NOT NULL),
            CONSTRAINT fk_pricing_plan_period_floor_cap_revision FOREIGN KEY (plan_id, plan_revision) REFERENCES bss.pricing_plan(plan_id, revision),
            CONSTRAINT pricing_plan_period_floor_cap_pkey PRIMARY KEY (plan_id, plan_revision, currency, region)
        )",
    "CREATE INDEX idx_pricing_plan_period_floor_cap_revision ON bss.pricing_plan_period_floor_cap USING btree (tenant_id, plan_id, plan_revision)",
    "CREATE OR REPLACE FUNCTION bss.pricing_plan_period_floor_cap_append_only() RETURNS trigger AS $$
        DECLARE
          parent_state  text;
          parent_tenant uuid;
        BEGIN
          IF TG_OP <> 'INSERT' THEN
            SELECT lifecycle_state INTO parent_state
              FROM bss.pricing_plan
             WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision;
            IF parent_state IS DISTINCT FROM 'draft' THEN
              RAISE EXCEPTION
                'pricing_plan_period_floor_cap: % of a period bound under a % plan revision is not permitted',
                TG_OP, coalesce(parent_state, 'missing');
            END IF;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RETURN OLD;
          END IF;

          SELECT lifecycle_state, tenant_id INTO parent_state, parent_tenant
            FROM bss.pricing_plan
           WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision;
          IF parent_state IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION
              'pricing_plan_period_floor_cap: % of a period bound under a % plan revision is not permitted',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          -- `fk_pricing_plan_period_floor_cap_revision` covers `(plan_id, plan_revision)` alone, so
          -- without this arm a row could carry a tenant its own parent revision
          -- does not belong to: invisible to every scoped reader, and frozen with
          -- the revision it was written under. The state arm above has already
          -- refused a parent that does not exist, so a foreign tenant is the only
          -- thing left for this one to find.
          IF parent_tenant IS DISTINCT FROM NEW.tenant_id THEN
            RAISE EXCEPTION
              'pricing_plan_period_floor_cap: plan revision %/% belongs to another tenant and may not hold this row',
              NEW.plan_id, NEW.plan_revision;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_plan_period_floor_cap_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_plan_period_floor_cap FOR EACH ROW EXECUTE FUNCTION bss.pricing_plan_period_floor_cap_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_plan_period_floor_cap",
    "DROP FUNCTION IF EXISTS bss.pricing_plan_period_floor_cap_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_plan_period_floor_cap (
            tenant_id     text   NOT NULL,
            plan_id       text   NOT NULL,
            plan_revision bigint NOT NULL,
            currency      text   NOT NULL,
            region        text   NOT NULL,
            cap_minor     bigint,
            floor_minor   bigint,
            PRIMARY KEY (plan_id, plan_revision, currency, region),
            CONSTRAINT chk_pricing_plan_period_floor_cap_cap_positive CHECK (cap_minor IS NULL OR cap_minor > 0),
            CONSTRAINT chk_pricing_plan_period_floor_cap_currency CHECK (length(currency) = 3),
            CONSTRAINT chk_pricing_plan_period_floor_cap_floor_positive CHECK (floor_minor IS NULL OR floor_minor > 0),
            CONSTRAINT chk_pricing_plan_period_floor_cap_ordered CHECK (floor_minor IS NULL OR cap_minor IS NULL OR floor_minor <= cap_minor),
            CONSTRAINT chk_pricing_plan_period_floor_cap_present CHECK (floor_minor IS NOT NULL OR cap_minor IS NOT NULL),
            CONSTRAINT fk_pricing_plan_period_floor_cap_revision FOREIGN KEY (plan_id, plan_revision) REFERENCES pricing_plan(plan_id, revision)
        )",
    "CREATE INDEX idx_pricing_plan_period_floor_cap_revision ON pricing_plan_period_floor_cap (tenant_id, plan_id, plan_revision)",
    "CREATE TRIGGER trg_pricing_plan_period_floor_cap_no_delete BEFORE DELETE ON pricing_plan_period_floor_cap FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_period_floor_cap: DELETE of a period bound under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_plan_period_floor_cap_no_insert BEFORE INSERT ON pricing_plan_period_floor_cap FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_period_floor_cap: INSERT of a period bound under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_plan_period_floor_cap_no_update BEFORE UPDATE ON pricing_plan_period_floor_cap FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_period_floor_cap: UPDATE of a period bound under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision AND lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_plan_period_floor_cap_same_tenant_as_its_revision_on_insert BEFORE INSERT ON pricing_plan_period_floor_cap FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_period_floor_cap: the plan revision belongs to another tenant and may not hold this row') WHERE EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision) AND NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND tenant_id = NEW.tenant_id); END",
    "CREATE TRIGGER trg_pricing_plan_period_floor_cap_same_tenant_as_its_revision_on_update BEFORE UPDATE ON pricing_plan_period_floor_cap FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_period_floor_cap: the plan revision belongs to another tenant and may not hold this row') WHERE EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision) AND NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND tenant_id = NEW.tenant_id); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_plan_period_floor_cap"];

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
