//! Create `bss.pricing_plan_descriptor_set` — the billing descriptor set of
//! **one plan revision** (`design/02-plan-definition.md` §6, D-48 as revised by
//! D-110), keyed `(plan_id, plan_revision)`. The last of Slice 2's three
//! revision-scoped child tables.
//!
//! # The key needs no discriminator, and §6 says so
//!
//! A revision has exactly one descriptor set — genuinely 1:1, unlike the phase
//! chain and the add-on rules, which is why this key is the plan key and the
//! revision and nothing else. `plan_revision` is still there for the same reason
//! it is on the siblings: the set versions with the revision (D-83), a new
//! revision copies it under its own number, and the open draft edits its own
//! copy.
//!
//! **`tenant_id` is not in §6's column list, and is here anyway** — the same
//! omission the other two child tables carry, reported rather than treated as a
//! decision. §6's preamble calls these tables "tenant-scoped, `SecureORM`",
//! `01-foundation.md` §3.7 says it of every physical table in this gear, and
//! `Scopable` has nowhere else to read the tenant from. The value is copied from
//! the parent revision by the repository and never taken from a request (Global
//! Constraint 9).
//!
//! # Three columns, not five (D-110)
//!
//! D-48 pinned a five-element v1 descriptor contract. Two of the five are
//! deliberately **not** columns here: `billingTiming` (2026-07-28) and
//! `taxCategory` (D-110). Both ride `pricing_price` and are delivered with the
//! row.
//!
//! D-110's defect is the one worth keeping in front of whoever reads this table
//! and wonders where the tax category went: a **per-plan** `taxCategory` column
//! cannot mirror the **per-row** `tax_category_ref` that Slice 4 makes the
//! source of truth, and the publish-time consistency check that was supposed to
//! reconcile the two was undefined the moment two rows of one plan carried
//! different categories — which is the ordinary case, not the exotic one, for a
//! plan selling a service line and a hardware line. Adding either column back
//! would be a second, disagreeing home for a value that already has one.
//!
//! # Every column is nullable, and that is what makes the rule reachable
//!
//! A draft may be incomplete: `flow-plan-author` step 4 attaches descriptors
//! **incrementally in `draft`**, so a `NOT NULL` here would refuse the ordinary
//! authoring path. It would also make `DESCRIPTOR_INCOMPLETE` unreachable —
//! `inst-ds-required` blocks the **publish** on a missing element and names each
//! one, and a column that cannot be missing is an element that can never be
//! named. The pipeline is what enforces completeness; this table only holds what
//! has been authored so far.
//!
//! `additional_fields` is P5's config-extensible required-field registry:
//! `jsonb` on Postgres and `text` on `SQLite`, holding a JSON object of
//! name/value pairs, the same transform `included_allowance` and the add-on edge
//! sets use. It exists so a deployment that must require a fourth descriptor
//! names it in configuration and carries its value here, instead of waiting for
//! a migration — which is the whole of what "config-extensible without a schema
//! change" can mean once the required set is not fixed in code. It is
//! `NOT NULL DEFAULT '{}'`: an empty object is what "no extra fields" is, and a
//! nullable column would give that state two spellings.
//!
//! # Append-only with its revision (`01-foundation.md` §3.7, the L-2 fix)
//!
//! Identical to the two sibling tables', and identical deliberately: descriptor
//! rows are physically immutable once **their** revision publishes, while a
//! draft revision's copy stays mutable and deletable. There is no
//! `lifecycle_state` here — the parent revision's is the referent — so the
//! predicate reads `pricing_plan.lifecycle_state = 'draft'` for
//! `(plan_id, plan_revision)`. Without it, the descriptor set of a frozen
//! revision could be rewritten under an unchanged `pricing_plan`, and Billing
//! would post against a GL code nobody published: §4.2's whole reason for
//! freezing the set is that the invoice line an ERP posts must not change under
//! a `CatalogVersion` that already resolved.
//!
//! INSERT is guarded as well as UPDATE and DELETE, because on a 1:1 table INSERT
//! is how a revision that published **without** a descriptor set would acquire
//! one afterwards — the publish having already been refused by
//! `DESCRIPTOR_INCOMPLETE`, or having frozen a set an operator now wants to
//! "fix". An UPDATE is checked against **both** ends, the `OLD` parent and the
//! `NEW` one, because re-pointing a child row's `plan_revision` is how one would
//! otherwise write to a frozen revision without ever issuing an INSERT.
//!
//! **`abandoned` is not `draft`**, so `PlanRepo::abandon_draft` drops this row
//! **before** it flips the revision, and `PlanRepo::open_revision` copies it
//! **after** it inserts the new revision row. Both orderings are forced by this
//! trigger rather than chosen.
//!
//! **Backend differences.** Postgres carries the rule as one PL/pgSQL trigger
//! function with the offending state interpolated; `SQLite` has no procedural
//! language and `RAISE(ABORT, ...)` takes a literal message, so the same rule
//! becomes three fixed-message triggers, one per DML verb, whose parent lookup
//! is a `WHERE NOT EXISTS` subquery in the trigger **body**.
//!
//! The body rather than a `WHEN` clause is this chain's spelling for a guard that
//! reads another row, and **not** an engine limitation: a `SQLite` `WHEN` does
//! accept a subquery — `pricing_approval_key`'s triggers put a scalar `SELECT` in
//! four of theirs and they enforce, and sqlite 3.51 admits `WHEN NOT EXISTS (…)`
//! directly. The
//! body form is taken so each arm's condition sits beside the message it raises,
//! one trigger per arm of the PL/pgSQL function.
//!
//! `uuid` becomes `text`, `jsonb` becomes `text`, and the `bss.` qualification is
//! dropped, as elsewhere in this chain. The index, FK and PK are preserved on
//! both sides.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_plan_descriptor_set (
            tenant_id             uuid   NOT NULL,
            plan_id               uuid   NOT NULL,
            plan_revision         bigint NOT NULL,
            additional_fields     jsonb  NOT NULL DEFAULT '{}'::jsonb,
            gl_code               text,
            invoice_line_template text,
            itemization_rule      text,
            CONSTRAINT fk_pricing_plan_descriptor_set_revision FOREIGN KEY (plan_id, plan_revision) REFERENCES bss.pricing_plan(plan_id, revision),
            CONSTRAINT pricing_plan_descriptor_set_pkey PRIMARY KEY (plan_id, plan_revision)
        )",
    "CREATE INDEX idx_pricing_plan_descriptor_set_revision ON bss.pricing_plan_descriptor_set USING btree (tenant_id, plan_id, plan_revision)",
    "CREATE OR REPLACE FUNCTION bss.pricing_plan_descriptor_set_append_only() RETURNS trigger AS $$
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
                'pricing_plan_descriptor_set: % of a descriptor set under a % plan revision is not permitted',
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
              'pricing_plan_descriptor_set: % of a descriptor set under a % plan revision is not permitted',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          -- `fk_pricing_plan_descriptor_set_revision` covers `(plan_id, plan_revision)` alone, so
          -- without this arm a row could carry a tenant its own parent revision
          -- does not belong to: invisible to every scoped reader, and frozen with
          -- the revision it was written under. The state arm above has already
          -- refused a parent that does not exist, so a foreign tenant is the only
          -- thing left for this one to find.
          IF parent_tenant IS DISTINCT FROM NEW.tenant_id THEN
            RAISE EXCEPTION
              'pricing_plan_descriptor_set: plan revision %/% belongs to another tenant and may not hold this row',
              NEW.plan_id, NEW.plan_revision;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_plan_descriptor_set_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_plan_descriptor_set FOR EACH ROW EXECUTE FUNCTION bss.pricing_plan_descriptor_set_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_plan_descriptor_set",
    "DROP FUNCTION IF EXISTS bss.pricing_plan_descriptor_set_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_plan_descriptor_set (
            tenant_id             text   NOT NULL,
            plan_id               text   NOT NULL,
            plan_revision         bigint NOT NULL,
            additional_fields     text   NOT NULL DEFAULT '{}',
            gl_code               text,
            invoice_line_template text,
            itemization_rule      text,
            PRIMARY KEY (plan_id, plan_revision),
            CONSTRAINT fk_pricing_plan_descriptor_set_revision FOREIGN KEY (plan_id, plan_revision) REFERENCES pricing_plan(plan_id, revision)
        )",
    "CREATE INDEX idx_pricing_plan_descriptor_set_revision ON pricing_plan_descriptor_set (tenant_id, plan_id, plan_revision)",
    "CREATE TRIGGER trg_pricing_plan_descriptor_set_no_delete BEFORE DELETE ON pricing_plan_descriptor_set FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_descriptor_set: DELETE of a descriptor set under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_plan_descriptor_set_no_insert BEFORE INSERT ON pricing_plan_descriptor_set FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_descriptor_set: INSERT of a descriptor set under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_plan_descriptor_set_no_update BEFORE UPDATE ON pricing_plan_descriptor_set FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_descriptor_set: UPDATE of a descriptor set under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = OLD.plan_id AND revision = OLD.plan_revision AND lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_plan_descriptor_set_same_tenant_as_its_revision_on_insert BEFORE INSERT ON pricing_plan_descriptor_set FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_descriptor_set: the plan revision belongs to another tenant and may not hold this row') WHERE EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision) AND NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND tenant_id = NEW.tenant_id); END",
    "CREATE TRIGGER trg_pricing_plan_descriptor_set_same_tenant_as_its_revision_on_update BEFORE UPDATE ON pricing_plan_descriptor_set FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_plan_descriptor_set: the plan revision belongs to another tenant and may not hold this row') WHERE EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision) AND NOT EXISTS (SELECT 1 FROM pricing_plan WHERE plan_id = NEW.plan_id AND revision = NEW.plan_revision AND tenant_id = NEW.tenant_id); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_plan_descriptor_set"];

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
