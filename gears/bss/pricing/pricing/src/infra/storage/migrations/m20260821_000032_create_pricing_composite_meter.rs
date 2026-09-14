//! `pricing_composite_meter` — Slice 10's derived-meter definition
//! (`design/10-advanced-primitives.md` §6, `inst-cm-constituents`,
//! `inst-cm-formula`, `inst-cm-frozen`, A4, D-32, D-106).
//!
//! A composite meter prices several constituent `meteringUnit`s as **one line**
//! — VM = vCPU + RAM — by declaring one `output_unit` and a formula over the
//! constituents. The catalog persists and freezes the definition and **never
//! evaluates it**: Rating does, from the snapshot (`inst-cm-frozen`).
//!
//! # Why it is its own table and keyed the way it is
//!
//! `PRIMARY KEY (tenant_id, plan_id, plan_revision, composite_id)` is
//! `pricing_plan_phase`'s shape one table over — scope first, then the plan's own
//! two — and the inner pair is D-106's reason restated in §6: the formula is
//! plan-shape configuration *"versioned with the plan revision"*, so opening a
//! draft revision **copies** the rows under the new `plan_revision` with a
//! **stable `composite_id`**, and a published revision's rows are immutable with
//! it. A bare `revision` column whose referent was never stated is what §6
//! replaced to get here.
//!
//! # Two rules this table deliberately does **not** enforce
//!
//! **Arity (`≥ 2` constituents) and self-reference are publish rules, not column
//! constraints**, and the reason is `pricing_price`'s reservation pair's: `SQLite`
//! has no incremental table-`CHECK`, and a
//! Postgres-only `ALTER` splits the two `EXPECTED_CHECKS` censuses that exist to
//! keep the engines legible against each other. §6 says as much for
//! self-reference already — *"check application-level (graph walk over
//! `constituent_units` vs `output_unit`)"* — and arity joins it for the same
//! reason plus a second: `json_array_length` is an extension function on
//! `SQLite` and a constraint that silently degrades on one engine is worse than
//! one stated in a rule that runs on both. They are
//! `COMPOSITE_TOO_FEW_CONSTITUENTS` and `COMPOSITE_SELF_REFERENCE`, both 422.
//!
//! **A constituent's *publication* is not checked at all**, here or anywhere.
//! `inst-cm-constituents` requires "≥ 2 **published** constituent `meteringUnit`
//! ids (registry-declared)" and this gear has **no registry client**:
//! `metering_unit` / `MeteringUnit` appear nowhere in `src/`, and
//! `PriceRow::meter` is a free `Option<String>` validated against nothing. The
//! arity and self-reference halves need no counterparty and are built; the
//! publication half is owed with the registry seam, and is recorded on the
//! instruction rather than left for a reader to infer from a rule that is not
//! there. `inst-cm-output-unit` is a registry *declaration* act and is not this
//! gear's at all (D-32).
//!
//! # The append-only guard
//!
//! `pricing_plan_phase`'s, verbatim in shape: the parent revision's
//! `lifecycle_state` is this row's, so every verb consults it — `UPDATE` and
//! `DELETE` against the revision the row is bound to **now**, `INSERT` and
//! `UPDATE` against the revision it would land **under**. Postgres expresses it
//! as one PL/pgSQL function with the offending state interpolated; `SQLite` has
//! no procedural language and `RAISE(ABORT, …)` takes a literal, so the same
//! rule becomes three fixed-message triggers whose parent lookup is a
//! `WHERE NOT EXISTS` subquery in the trigger **body**.
//!
//! The body rather than a `WHEN` clause is this chain's spelling for a guard that
//! reads another row, and **not** an engine limitation: a `SQLite` `WHEN` does
//! accept a subquery — `pricing_approval_key`'s triggers put a scalar `SELECT` in
//! four of theirs and they enforce, and sqlite 3.51 admits `WHEN NOT EXISTS (…)`
//! directly. The
//! body form is taken so each arm's condition sits beside the message it raises,
//! one trigger per arm of the PL/pgSQL function.
//!
//! # Why the scope is in the key, and what the key still refuses
//!
//! `tenant_id` and `plan_id` are in the key by D-340, whose class this is. Without
//! them the key asserts what D-106's argument never said: that a composite id
//! belongs to one plan **per revision number across the whole table**, every
//! tenant's included.
//!
//! **`composite_id` is client-supplied, which is what makes that reachable.**
//! `api/rest/plans.rs` renders it `view.composite_id.unwrap_or_else(Uuid::now_v7)`
//! — the mint-if-absent idiom, one of exactly three hits crate-wide, the other two
//! being `phase_id` and `line_id`. Supplying it is the intended usage and has to be:
//! D-19's clone remap and D-83's copy-forward both hand the server an id it did not
//! mint. So any `plan × write` holder reaches the key by naming an id through the
//! `composites` PATCH facet — and under an unscoped key, naming one another tenant
//! holds at the same `plan_revision` is refused while naming a free one is not,
//! with the tenant-scoped reads on that path seeing nothing either way. The whole
//! discrimination comes from the key: an oracle over another tenant's composite ids
//! on a table this gear scopes by `tenant_id` everywhere else. The other half is
//! worse and permanent — the *first* tenant to take an id at revision `0` would
//! lock every other tenant out of it at that number for good.
//!
//! **What the key still refuses is not a leftover.**
//! `(tenant_id, plan_id, plan_revision, composite_id)` admits exactly one row per
//! `(plan, revision, composite)`, so one revision may still not hold the same
//! composite id twice. `list_composites` hands back a set that the self-reference
//! walk and `COMPOSITE_TOO_FEW_CONSTITUENTS`'s arity rule both quantify over, and
//! both are written as though a composite id names at most one row of a revision. A
//! key that also admitted the duplicate would close the hole above and quietly make
//! those rules judge a set nobody can author. `tests/sqlite_plan_repo.rs` carries
//! one probe per direction for that reason.
//!
//! The key tuple is exactly what `idx_pricing_composite_meter_revision` ranges over
//! and what `uq_pricing_composite_meter_output` leads with. This table has one
//! foreign key **outward** (`fk_pricing_composite_meter_revision` → `pricing_plan`)
//! and none **onto** it, and `composite_meter::Entity::find_by_id` has no call
//! site, so the key's arity is not a signature anything spells out.
//!
//! # `output_unit` is held to the taxonomies' blankness predicate
//!
//! `chk_pricing_composite_meter_output_unit` strips ASCII whitespace entire — tab,
//! line feed, vertical tab, form feed, carriage return, space — and refuses a unit
//! left with nothing. The rule and its argument are `pricing_region_taxonomy`'s
//! (D-242), including the character set, why the two engines spell it two ways, and
//! the non-ASCII whitespace residue no `CHECK` on this chain reaches.
//!
//! What this column loses to a blank unit is its own: a unit of nothing renders on an
//! invoice line as a blank and joins no meter to any unit, and
//! `uq_pricing_composite_meter_output` then reserves it per revision as if it were a
//! name. `require_authorable_composites` in `api::rest::plans` trims before it
//! decides, so the residue is out of the one writer's reach; the field itself is a
//! plain `String`, so the trim lives at the door rather than in the type, and this
//! predicate is what the door's absence would leave.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_composite_meter (
            tenant_id         uuid   NOT NULL,
            plan_id           uuid   NOT NULL,
            plan_revision     bigint NOT NULL,
            composite_id      uuid   NOT NULL,
            constituent_units jsonb  NOT NULL,
            formula           jsonb  NOT NULL,
            output_unit       text   NOT NULL,
            CONSTRAINT chk_pricing_composite_meter_output_unit CHECK (length(btrim(output_unit, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0),
            CONSTRAINT fk_pricing_composite_meter_revision FOREIGN KEY (plan_id, plan_revision) REFERENCES bss.pricing_plan(plan_id, revision),
            CONSTRAINT pricing_composite_meter_pkey PRIMARY KEY (tenant_id, plan_id, plan_revision, composite_id)
        )",
    "CREATE INDEX idx_pricing_composite_meter_revision ON bss.pricing_composite_meter USING btree (tenant_id, plan_id, plan_revision)",
    "CREATE UNIQUE INDEX uq_pricing_composite_meter_output ON bss.pricing_composite_meter USING btree (tenant_id, plan_id, plan_revision, output_unit)",
    "CREATE OR REPLACE FUNCTION bss.pricing_composite_meter_append_only() RETURNS trigger AS $$
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
                'pricing_composite_meter: % of a composite under a % plan revision is not permitted',
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
              'pricing_composite_meter: % of a composite under a % plan revision is not permitted',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          -- `fk_pricing_composite_meter_revision` covers `(plan_id, plan_revision)` alone, so
          -- without this arm a row could carry a tenant its own parent revision
          -- does not belong to: invisible to every scoped reader, and frozen with
          -- the revision it was written under. The state arm above has already
          -- refused a parent that does not exist, so a foreign tenant is the only
          -- thing left for this one to find.
          IF parent_tenant IS DISTINCT FROM NEW.tenant_id THEN
            RAISE EXCEPTION
              'pricing_composite_meter: plan revision %/% belongs to another tenant and may not hold this row',
              NEW.plan_id, NEW.plan_revision;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_composite_meter_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_composite_meter FOR EACH ROW EXECUTE FUNCTION bss.pricing_composite_meter_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_composite_meter",
    "DROP FUNCTION IF EXISTS bss.pricing_composite_meter_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_composite_meter (
            tenant_id         text   NOT NULL,
            plan_id           text   NOT NULL,
            plan_revision     bigint NOT NULL,
            composite_id      text   NOT NULL,
            constituent_units text   NOT NULL,
            formula           text   NOT NULL,
            output_unit       text   NOT NULL,
            PRIMARY KEY (tenant_id, plan_id, plan_revision, composite_id),
            CONSTRAINT chk_pricing_composite_meter_output_unit CHECK (length(trim(output_unit, char(9,10,11,12,13,32))) > 0),
            CONSTRAINT fk_pricing_composite_meter_revision FOREIGN KEY (plan_id, plan_revision) REFERENCES pricing_plan(plan_id, revision)
        )",
    "CREATE INDEX idx_pricing_composite_meter_revision ON pricing_composite_meter (tenant_id, plan_id, plan_revision)",
    "CREATE UNIQUE INDEX uq_pricing_composite_meter_output ON pricing_composite_meter (tenant_id, plan_id, plan_revision, output_unit)",
    "CREATE TRIGGER trg_pricing_composite_meter_no_delete BEFORE DELETE ON pricing_composite_meter FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_composite_meter: DELETE of a composite under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_composite_meter_no_insert BEFORE INSERT ON pricing_composite_meter FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_composite_meter: INSERT of a composite under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_composite_meter_no_update BEFORE UPDATE ON pricing_composite_meter FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_composite_meter: UPDATE of a composite under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision AND lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_composite_meter_same_tenant_as_its_revision_on_insert BEFORE INSERT ON pricing_composite_meter FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_composite_meter: the plan revision belongs to another tenant and may not hold this row') WHERE EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision) AND NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND tenant_id = NEW.tenant_id); END",
    "CREATE TRIGGER trg_pricing_composite_meter_same_tenant_as_its_revision_on_update BEFORE UPDATE ON pricing_composite_meter FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_composite_meter: the plan revision belongs to another tenant and may not hold this row') WHERE EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision) AND NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND tenant_id = NEW.tenant_id); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_composite_meter"];

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
