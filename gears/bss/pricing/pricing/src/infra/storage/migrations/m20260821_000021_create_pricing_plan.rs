//! Create `bss.pricing_plan` — the plan aggregate's **revision** rows
//! (`design/01-foundation.md` §3.7, D-56), keyed `(plan_id, revision)`. A plan
//! is not one row: it is a chain of revisions of which at most one is the
//! **current** one and at most one is an open `draft`.
//!
//! Two partial `UNIQUE` indexes carry that, and both are load-bearing.
//! `uq_pricing_plan_current` spans `lifecycle_state IN ('published','retired')`
//! — widened from `= 'published'` by **D-128**, because retirement flips the
//! only current revision and the narrower predicate then held no row at all,
//! leaving "the current revision" (the projection source, the sellability
//! predicate, every referential check) with no referent.
//! `uq_pricing_plan_open_draft` keeps a plan from having two concurrently
//! editable shapes.
//!
//! **A `revision` number is never freed** (D-145). A discarded draft revision is
//! flipped to the terminal `abandoned` state rather than deleted, so no DELETE
//! is permitted on this table at all — not even of a draft. Deleting one
//! returned `max(revision)` to its previous value, the next opened draft minted
//! the **same** number, and `(plan_id, revision)` — the name the grant table would be
//! keyed by, every revision-scoped child copies under, and the audit trail
//! records — then denoted two different rows over a plan's lifetime, under which
//! a stale entity tag passes its precondition against the wrong one.
//! `abandoned` falls outside **both** partial predicates above, which is what
//! lets a tombstone accumulate without disturbing either uniqueness rule: a
//! replacement draft opens immediately, and "the current revision" is untouched.
//!
//! The physical guard is append-only enforcement with a **column whitelist**: a
//! published, retired or abandoned revision's content is frozen, and the only
//! sanctioned in-place mutations are the state-machine `lifecycle_state` flips
//! (`published -> superseded` when the successor revision commits,
//! `published -> retired` at retire, `draft -> published` at publish and
//! `draft -> abandoned` at discard). Draft revisions stay freely mutable **in
//! content**, and the guard restates the machine's edges on that plane too:
//! `draft -> superseded` and `draft -> retired` are refused here as they are in
//! `domain::lifecycle`, because a hand-run flip to `retired` mints a row that
//! satisfies `uq_pricing_plan_current` without ever having published — and the
//! projector sources a plan subject from exactly that row. Nothing on this table
//! is deletable. Without the guard an ad-hoc UPDATE would silently change a
//! frozen `CatalogVersion`'s content at the next warm re-drive, since the
//! projector reads truth rows (§4.4).
//!
//! **Backend differences.** Postgres raises through a PL/pgSQL
//! `RAISE EXCEPTION` carrying the offending value; `SQLite` has no PL/pgSQL and
//! `RAISE(ABORT, ...)` takes a **literal** message only, so the mirror splits
//! the same whitelist across three triggers with fixed messages and no
//! interpolated values. `REVOKE UPDATE, DELETE` is deliberately not issued on
//! either backend: it names a deployment role this migration does not own, and
//! `SQLite` has no `GRANT`/`REVOKE` at all — the trigger is the portable half of
//! the "REVOKE + trigger" discipline the design set describes.
//!
//! # The Slice-2 shape columns (`design/02-plan-definition.md` §6)
//!
//! The table is Foundation-owned and its **capability semantics** are not:
//! Slice 2 declares `billing_cycle`'s value set, the `frequency` metadata beside
//! it, `plan_tier_override`, the one-time purchase-quantity window and D-96's
//! `invoice_grouping_key`. Each of them joins the frozen-column whitelist above,
//! and that is not bookkeeping: a column outside the whitelist is a column an
//! ad-hoc UPDATE can move under a `CatalogVersion` that is already frozen, which
//! the projector's warm re-drive then re-materializes as though it had always
//! been that way (§4.4). The whitelist is enumerated per column by
//! `tests/sqlite_plan_guards.rs` for exactly that reason.
//!
//! **The custom-interval pairing.** `Frequency` makes two states unrepresentable
//! in the domain — a `monthly` frequency carrying an interval, and a custom one
//! carrying none — and `chk_pricing_plan_custom_interval_pairing` is the
//! physical half of the same property, because three columns can express what
//! one enum cannot. It carries an explicit `frequency IS NOT NULL` conjunct: a
//! bare `frequency = 'custom_every_n'` is NULL when the column is NULL, and both
//! engines count a NULL CHECK as **satisfied** — the reason
//! `chk_pricing_price_package_fields_kind` in migration 000002 spells out its
//! own `IS NOT NULL` the same way. What the biconditional does **not** catch is
//! a half-set pair under a non-custom frequency (`custom_interval_n` set,
//! `custom_interval_unit` NULL): its right-hand side is already false, so the
//! CHECK holds. That row is refused where it is read instead —
//! `plan_repo::read_frequency` accepts an interval column only on the custom
//! token — so the typed path fails closed on it, with the schema standing behind
//! the reading rather than in front of it.
//!
//! **`chk_pricing_plan_purchase_qty` makes `PURCHASE_QTY_RANGE_INVALID`
//! unreachable through `PlanRepo`**, and the rule stays anyway: the validation
//! pipeline judges a subject the **caller** assembled, before any of it is a
//! row, and its contract is to enumerate every finding in one report. A pipeline
//! that let this one arrive as a driver error would answer a publish attempt
//! with an internal fault instead of a line an author can act on.
//!
//! # About this file
//!
//! # The columns beyond §6's list, and the one rule they share
//!
//! `allowed_change_targets`, `comparability_rank` and `usage_counter_on_plan_change`
//! are D-113's plan-change contract; `entitlement_grants` is D-41's grant set;
//! `cloned_from` records D-19's clone provenance; `plan_name` is the authored label;
//! `row_version` is the optimistic-concurrency counter. Each is `NOT NULL` with a
//! fail-safe default where the absent reading is a decision, and nullable where
//! absence is a state.
//!
//! **Every one of them is in the append-only guard's frozen list**, and that is not
//! bookkeeping. `pricing_plan_append_only` whitelists what a *published* revision may
//! still move — the `lifecycle_state` flip, and nothing else — so a column added
//! outside that list is a published plan whose content can be edited after the fact.
//! Splitting or adding a column splits or extends its rules with it; the guard body
//! and the column list are one change, never two.
//!
//! Dependency level 0: it references no other table.
//! Columns read identity first, then content by name, then the audit columns.
//!
//! The SQL is generated by `tasks/emit_chain.py` from the frozen schema goldens and
//! is rewritten on every run; this doc is not. What dissolved into this migration is
//! recorded in `tasks/migration-inventory.md`, which is where to look for the chain's
//! own history — nothing above narrates it, because a fresh-install chain has none.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_plan (
            tenant_id                    uuid        NOT NULL,
            plan_id                      uuid        NOT NULL,
            revision                     bigint      NOT NULL,
            allowed_change_targets       jsonb,
            available_from               timestamptz,
            available_to                 timestamptz,
            billing_cycle                text,
            cloned_from                  uuid,
            comparability_rank           integer,
            custom_interval_n            integer,
            custom_interval_unit         text,
            entitlement_grants           jsonb,
            frequency                    text,
            invoice_grouping_key         text,
            lifecycle_state              text        NOT NULL,
            plan_name                    text,
            plan_tier                    text,
            plan_tier_override           boolean     NOT NULL DEFAULT false,
            purchase_max_qty             bigint,
            purchase_min_qty             bigint,
            sku_id                       uuid,
            usage_counter_on_plan_change text,
            created_at_utc               timestamptz NOT NULL DEFAULT now(),
            created_by                   uuid        NOT NULL,
            row_version                  bigint      NOT NULL DEFAULT 0,
            CONSTRAINT chk_pricing_plan_availability CHECK (available_from IS NULL OR available_to IS NULL OR available_to > available_from),
            CONSTRAINT chk_pricing_plan_billing_cycle CHECK (billing_cycle IS NULL OR billing_cycle IN ('one_time','recurring','usage','hybrid')),
            CONSTRAINT chk_pricing_plan_custom_interval_n CHECK (custom_interval_n IS NULL OR custom_interval_n > 0),
            CONSTRAINT chk_pricing_plan_custom_interval_pairing CHECK ((frequency IS NOT NULL AND frequency = 'custom_every_n') = (custom_interval_n IS NOT NULL AND custom_interval_unit IS NOT NULL)),
            CONSTRAINT chk_pricing_plan_custom_interval_unit CHECK (custom_interval_unit IS NULL OR custom_interval_unit IN ('days','months')),
            CONSTRAINT chk_pricing_plan_frequency CHECK (frequency IS NULL OR frequency IN ('monthly','quarterly','semiannual','annual','custom_every_n')),
            CONSTRAINT chk_pricing_plan_lifecycle_state CHECK (lifecycle_state IN ('draft','abandoned','published','superseded','retired')),
            CONSTRAINT chk_pricing_plan_purchase_max_qty CHECK (purchase_max_qty IS NULL OR purchase_max_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_min_qty CHECK (purchase_min_qty IS NULL OR purchase_min_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_qty CHECK (purchase_min_qty IS NULL OR purchase_max_qty IS NULL OR purchase_min_qty <= purchase_max_qty),
            CONSTRAINT chk_pricing_plan_revision CHECK (revision >= 0),
            CONSTRAINT chk_pricing_plan_row_version CHECK (row_version >= 0),
            CONSTRAINT pricing_plan_pkey PRIMARY KEY (plan_id, revision)
        )",
    "CREATE INDEX idx_pricing_plan_tenant ON bss.pricing_plan USING btree (tenant_id, plan_id, revision)",
    "CREATE UNIQUE INDEX uq_pricing_plan_current ON bss.pricing_plan USING btree (plan_id) WHERE (lifecycle_state = ANY (ARRAY['published'::text, 'retired'::text]))",
    "CREATE UNIQUE INDEX uq_pricing_plan_open_draft ON bss.pricing_plan USING btree (plan_id) WHERE (lifecycle_state = 'draft'::text)",
    "CREATE OR REPLACE FUNCTION bss.pricing_plan_append_only() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION
              'pricing_plan: DELETE of revision % of plan % is not permitted; a discarded draft revision is abandoned',
              OLD.revision, OLD.plan_id;
          END IF;

          -- The draft plane is where content moves, so its columns are
          -- unguarded - but its **exits** are not. A draft leaves by publishing
          -- or by being abandoned, and `NEW = draft` is the ordinary edit.
          -- Without the check a hand-run flip could mint a `retired` row that
          -- never published - one that satisfies the current-revision partial
          -- UNIQUE and is what the projector then sources a plan subject from.
          -- Membership is tested rather than change: a
          -- `NEW IS DISTINCT FROM OLD` conjunct would let the SQLite mirror
          -- accept a no-op UPDATE this branch refuses, and a backend divergence
          -- is worse than the hole it would close.
          IF OLD.lifecycle_state = 'draft' THEN
            IF NEW.lifecycle_state NOT IN ('draft','published','abandoned') THEN
              RAISE EXCEPTION 'pricing_plan: lifecycle_state % -> % is not a sanctioned flip',
                OLD.lifecycle_state, NEW.lifecycle_state;
            END IF;
            RETURN NEW;
          END IF;

          -- Past here the row is published, superseded, retired or abandoned.
          -- Once abandoned it is a tombstone: frozen in content by the whitelist
          -- below and left by no flip, so the number it consumed can never be
          -- attached to a different shape.

          IF NEW.plan_id              IS DISTINCT FROM OLD.plan_id
          OR NEW.revision             IS DISTINCT FROM OLD.revision
          OR NEW.tenant_id            IS DISTINCT FROM OLD.tenant_id
          OR NEW.sku_id               IS DISTINCT FROM OLD.sku_id
          OR NEW.plan_tier            IS DISTINCT FROM OLD.plan_tier
          OR NEW.billing_cycle        IS DISTINCT FROM OLD.billing_cycle
          OR NEW.frequency            IS DISTINCT FROM OLD.frequency
          OR NEW.custom_interval_n    IS DISTINCT FROM OLD.custom_interval_n
          OR NEW.custom_interval_unit IS DISTINCT FROM OLD.custom_interval_unit
          OR NEW.plan_tier_override   IS DISTINCT FROM OLD.plan_tier_override
          OR NEW.purchase_min_qty     IS DISTINCT FROM OLD.purchase_min_qty
          OR NEW.purchase_max_qty     IS DISTINCT FROM OLD.purchase_max_qty
          OR NEW.invoice_grouping_key IS DISTINCT FROM OLD.invoice_grouping_key
          OR NEW.available_from       IS DISTINCT FROM OLD.available_from
          OR NEW.available_to         IS DISTINCT FROM OLD.available_to
          OR NEW.created_by           IS DISTINCT FROM OLD.created_by
          OR NEW.created_at_utc       IS DISTINCT FROM OLD.created_at_utc
          OR NEW.allowed_change_targets       IS DISTINCT FROM OLD.allowed_change_targets
          OR NEW.comparability_rank           IS DISTINCT FROM OLD.comparability_rank
          OR NEW.usage_counter_on_plan_change IS DISTINCT FROM OLD.usage_counter_on_plan_change
          OR NEW.entitlement_grants           IS DISTINCT FROM OLD.entitlement_grants
          OR NEW.cloned_from                  IS DISTINCT FROM OLD.cloned_from
          OR NEW.plan_name            IS DISTINCT FROM OLD.plan_name
          OR NEW.row_version          IS DISTINCT FROM OLD.row_version THEN
            RAISE EXCEPTION
              'pricing_plan: revision % of plan % is frozen; only a sanctioned lifecycle_state flip is permitted',
              OLD.revision, OLD.plan_id;
          END IF;

          IF NOT (OLD.lifecycle_state = 'published'
                  AND NEW.lifecycle_state IN ('superseded','retired')) THEN
            RAISE EXCEPTION 'pricing_plan: lifecycle_state % -> % is not a sanctioned flip',
              OLD.lifecycle_state, NEW.lifecycle_state;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_plan_append_only BEFORE DELETE OR UPDATE ON bss.pricing_plan FOR EACH ROW EXECUTE FUNCTION bss.pricing_plan_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_plan",
    "DROP FUNCTION IF EXISTS bss.pricing_plan_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_plan (
            tenant_id                    text    NOT NULL,
            plan_id                      text    NOT NULL,
            revision                     bigint  NOT NULL,
            allowed_change_targets       text,
            available_from               text,
            available_to                 text,
            billing_cycle                text,
            cloned_from                  text,
            comparability_rank           integer,
            custom_interval_n            int,
            custom_interval_unit         text,
            entitlement_grants           text,
            frequency                    text,
            invoice_grouping_key         text,
            lifecycle_state              text    NOT NULL,
            plan_name                    text,
            plan_tier                    text,
            plan_tier_override           boolean NOT NULL DEFAULT 0,
            purchase_max_qty             bigint,
            purchase_min_qty             bigint,
            sku_id                       text,
            usage_counter_on_plan_change text,
            created_at_utc               text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            created_by                   text    NOT NULL,
            row_version                  bigint  NOT NULL DEFAULT 0,
            PRIMARY KEY (plan_id, revision),
            CONSTRAINT chk_pricing_plan_availability CHECK (available_from IS NULL OR available_to IS NULL OR available_to > available_from),
            CONSTRAINT chk_pricing_plan_billing_cycle CHECK (billing_cycle IS NULL OR billing_cycle IN ('one_time','recurring','usage','hybrid')),
            CONSTRAINT chk_pricing_plan_custom_interval_n CHECK (custom_interval_n IS NULL OR custom_interval_n > 0),
            CONSTRAINT chk_pricing_plan_custom_interval_pairing CHECK ((frequency IS NOT NULL AND frequency = 'custom_every_n') = (custom_interval_n IS NOT NULL AND custom_interval_unit IS NOT NULL)),
            CONSTRAINT chk_pricing_plan_custom_interval_unit CHECK (custom_interval_unit IS NULL OR custom_interval_unit IN ('days','months')),
            CONSTRAINT chk_pricing_plan_frequency CHECK (frequency IS NULL OR frequency IN ('monthly','quarterly','semiannual','annual','custom_every_n')),
            CONSTRAINT chk_pricing_plan_lifecycle_state CHECK (lifecycle_state IN ('draft','abandoned','published','superseded','retired')),
            CONSTRAINT chk_pricing_plan_purchase_max_qty CHECK (purchase_max_qty IS NULL OR purchase_max_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_min_qty CHECK (purchase_min_qty IS NULL OR purchase_min_qty >= 0),
            CONSTRAINT chk_pricing_plan_purchase_qty CHECK (purchase_min_qty IS NULL OR purchase_max_qty IS NULL OR purchase_min_qty <= purchase_max_qty),
            CONSTRAINT chk_pricing_plan_revision CHECK (revision >= 0),
            CONSTRAINT chk_pricing_plan_row_version CHECK (row_version >= 0)
        )",
    "CREATE INDEX idx_pricing_plan_tenant ON pricing_plan (tenant_id, plan_id, revision)",
    "CREATE UNIQUE INDEX uq_pricing_plan_current ON pricing_plan (plan_id) WHERE lifecycle_state IN ('published','retired')",
    "CREATE UNIQUE INDEX uq_pricing_plan_open_draft ON pricing_plan (plan_id) WHERE lifecycle_state = 'draft'",
    "CREATE TRIGGER trg_pricing_plan_draft_flip_whitelist BEFORE UPDATE ON pricing_plan FOR EACH ROW WHEN OLD.lifecycle_state = 'draft' AND NEW.lifecycle_state NOT IN ('draft','published','abandoned') BEGIN SELECT RAISE(ABORT, 'pricing_plan: lifecycle_state transition is not a sanctioned flip'); END",
    "CREATE TRIGGER trg_pricing_plan_flip_whitelist BEFORE UPDATE ON pricing_plan FOR EACH ROW WHEN OLD.lifecycle_state <> 'draft' AND NOT (OLD.lifecycle_state = 'published' AND NEW.lifecycle_state IN ('superseded','retired')) BEGIN SELECT RAISE(ABORT, 'pricing_plan: lifecycle_state transition is not a sanctioned flip'); END",
    "CREATE TRIGGER trg_pricing_plan_frozen_columns BEFORE UPDATE ON pricing_plan FOR EACH ROW WHEN OLD.lifecycle_state <> 'draft' AND (NEW.plan_id IS NOT OLD.plan_id OR NEW.revision IS NOT OLD.revision OR NEW.tenant_id IS NOT OLD.tenant_id OR NEW.sku_id IS NOT OLD.sku_id OR NEW.plan_tier IS NOT OLD.plan_tier OR NEW.billing_cycle IS NOT OLD.billing_cycle OR NEW.frequency IS NOT OLD.frequency OR NEW.custom_interval_n IS NOT OLD.custom_interval_n OR NEW.custom_interval_unit IS NOT OLD.custom_interval_unit OR NEW.plan_tier_override IS NOT OLD.plan_tier_override OR NEW.purchase_min_qty IS NOT OLD.purchase_min_qty OR NEW.purchase_max_qty IS NOT OLD.purchase_max_qty OR NEW.invoice_grouping_key IS NOT OLD.invoice_grouping_key OR NEW.available_from IS NOT OLD.available_from OR NEW.available_to IS NOT OLD.available_to OR NEW.created_by IS NOT OLD.created_by OR NEW.created_at_utc IS NOT OLD.created_at_utc OR NEW.allowed_change_targets IS NOT OLD.allowed_change_targets OR NEW.comparability_rank IS NOT OLD.comparability_rank OR NEW.usage_counter_on_plan_change IS NOT OLD.usage_counter_on_plan_change OR NEW.entitlement_grants IS NOT OLD.entitlement_grants OR NEW.cloned_from IS NOT OLD.cloned_from OR NEW.plan_name IS NOT OLD.plan_name OR NEW.row_version IS NOT OLD.row_version) BEGIN SELECT RAISE(ABORT, 'pricing_plan: revision is frozen; only a sanctioned lifecycle_state flip is permitted'); END",
    "CREATE TRIGGER trg_pricing_plan_no_delete BEFORE DELETE ON pricing_plan FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan: DELETE of a revision is not permitted; a discarded draft revision is abandoned'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_plan"];

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
