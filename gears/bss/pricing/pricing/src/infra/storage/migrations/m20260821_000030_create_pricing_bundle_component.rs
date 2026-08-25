//! Create `bss.pricing_bundle_component` — the components of **one bundle
//! revision** (`design/08-bundles.md` §6, D-92 + D-105), keyed
//! `(bundle_id, plan_revision, component_plan_id)`.
//!
//! # The key carries `component_plan_id`, and the omission it repairs was fatal
//!
//! D-105, the same repair `pricing_plan_addon_rule` records one slice over.
//! D-92's *"keyed `(bundle_id, plan_revision)`"* admits **one** component per
//! revision, and one row cannot hold the data this slice's rules are written
//! over: *"every referenced component MUST have a covering published row in each
//! `(currency, region)`"* (`inst-bc-coverage`) quantifies over a set, the
//! frequency-match rule (`inst-bc-frequency`) compares components to each other,
//! and `COMPONENT_IS_BUNDLE` is a property of one member of a set. All three
//! were unsatisfiable by construction, and none would have failed a test — a
//! bundle simply could never reach the state the rule rejects.
//!
//! `plan_revision` is the copy-on-new-revision half (D-92, D-83 semantics
//! verbatim): a new revision copies these rows under its own number and the open
//! draft edits its own copies.
//!
//! # `component_plan_id` is in the key, so it is required on every basis
//!
//! §6 marks it *"required for `sum_of_parts`, B1"*, which reads as optional on
//! an `own_price` bundle. D-105's primary key does not admit that: a key column
//! is `NOT NULL`. The two are reconciled in the direction the key forces —
//! a component reference always names a component plan — and it costs nothing,
//! because `inst-bb-own` requires an `own_price` bundle to carry a
//! matching-currency component set anyway (Slice 4 case iii). A component row
//! that named no plan would be a row the coverage walk cannot evaluate. The
//! divergence from §6's wording is reported rather than smoothed over.
//!
//! # `tenant_id` is not in §6's column list, and is here anyway
//!
//! §6's preamble calls these tables "tenant-scoped, `SecureORM`" and
//! `01-foundation.md` §3.7 says it of every physical table in this gear;
//! `Scopable` has nowhere else to read the tenant from. The value is copied from
//! the parent bundle by the repository and never taken from a request (Global
//! Constraint 9): the foreign key covers `bundle_id` alone, so nothing here
//! stops a child carrying a different tenant from the row it points at.
//!
//! # No foreign key on `component_plan_id` or `included_sku_id`
//!
//! `component_plan_id` names a **published plan of another SKU**, which is a
//! `(plan_id, revision)` pair this column does not carry — and whose publication
//! is exactly what `COMPONENT_UNPUBLISHED` checks at publish rather than at
//! insert, because a draft composition may legally reference a plan that has not
//! published yet. `included_sku_id` points outside this gear entirely, into the
//! product/SKU registry. Neither referent is a row this schema holds.
//!
//! **The isolation half of that, which this doc did not state** (review A1-6, and
//! the answer to it): `component_plan_id` is client-supplied, so the question is
//! not only *when* the reference is resolved but *in whose scope*.
//! `infra::bundle::component_defects` makes all three of its reads through
//! `.secure()` under the caller's scope, so a **published** plan in another tenant
//! contributes `Unpublished` and nothing else — indistinguishable from an id
//! nobody holds, sentence for sentence. That is asserted rather than argued, in
//! `rest_bundles::a_component_in_another_tenant_reads_exactly_like_an_absent_one`,
//! against a *published* foreign plan specifically: a foreign draft would answer
//! the same way with the scoping removed, so it could not tell the two apart. The
//! key here is rooted in a server-minted `bundle_id`, so there is no slot for a
//! stranger to take either.
//!
//! # Append-only with its revision (D-92; `01-foundation.md` §3.7, the L-2 fix)
//!
//! Identical in shape to `pricing_plan_addon_rule`'s and identical deliberately.
//! There is no `lifecycle_state` on this table — the owning plan revision's is
//! the referent — but unlike the Slice-2 children this table does not carry
//! `plan_id`, so the predicate resolves it **through `pricing_bundle`**: join the
//! bundle to find its plan, then read that plan revision's state. That join is
//! also the enforcement of the `plan_id` reference D-105 asked for and which
//! `pricing_bundle` itself cannot declare (see its own module doc).
//!
//! Without the trigger, the component set of a frozen revision could be
//! rewritten under an unchanged `pricing_plan`, and the projector's warm
//! re-drive — which reads truth rows (§4.4) — would quietly re-materialize a
//! frozen `CatalogVersion` at a different composition. That is D-92's stated
//! defect, and a rev-share re-split or a component swap is vendor payout and
//! customer entitlement respectively, which is why D-104 makes both
//! always-material.
//!
//! INSERT is guarded and not only UPDATE and DELETE, because an INSERT is the
//! one verb that **adds** a component to a frozen revision. An UPDATE is checked
//! against **both** ends: the `OLD` parent, whose freeze governs the row now, and
//! the `NEW` parent, whose freeze forbids the append — re-pointing a child row's
//! `plan_revision` is precisely how one would otherwise append to a frozen
//! revision without ever issuing an INSERT.
//!
//! **`abandoned` is not `draft`**, so a repository dropping a discarded draft's
//! composition must do it **before** it flips the revision, and a copy-forward
//! must run **after** the new revision row is inserted. Both orderings are forced
//! by this trigger rather than chosen.
//!
//! **Backend differences.** Postgres carries the rule as one PL/pgSQL trigger
//! function with the offending state interpolated; `SQLite` has no procedural
//! language and `RAISE(ABORT, ...)` takes a literal message, so the same rule
//! becomes three fixed-message triggers, one per DML verb, whose parent lookup is
//! a `WHERE NOT EXISTS` subquery in the trigger **body**.
//!
//! The body rather than a `WHEN` clause is this chain's spelling for a guard that
//! reads another row, and **not** an engine limitation: a `SQLite` `WHEN` does
//! accept a subquery — `pricing_approval_key`'s triggers put a scalar `SELECT` in
//! four of theirs and they enforce, and sqlite 3.51 admits `WHEN NOT EXISTS (…)`
//! directly. The
//! body form is taken so each arm's condition sits beside the message it raises,
//! one trigger per arm of the PL/pgSQL function.
//!
//! `uuid` becomes `text` and the `bss.` qualification is dropped, as elsewhere in
//! this chain. Every `CHECK`, index, FK and PK is preserved on both sides.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_bundle_component (
            tenant_id         uuid    NOT NULL,
            bundle_id         uuid    NOT NULL,
            plan_revision     bigint  NOT NULL,
            component_plan_id uuid    NOT NULL,
            included_sku_id   uuid    NOT NULL,
            max_qty           integer,
            min_qty           integer,
            CONSTRAINT chk_pricing_bundle_component_min_qty CHECK (min_qty IS NULL OR min_qty >= 0),
            CONSTRAINT chk_pricing_bundle_component_qty_range CHECK (min_qty IS NULL OR max_qty IS NULL OR min_qty <= max_qty),
            CONSTRAINT fk_pricing_bundle_component_bundle FOREIGN KEY (bundle_id) REFERENCES bss.pricing_bundle(bundle_id),
            CONSTRAINT pricing_bundle_component_pkey PRIMARY KEY (bundle_id, plan_revision, component_plan_id)
        )",
    "CREATE INDEX idx_pricing_bundle_component_plan ON bss.pricing_bundle_component USING btree (tenant_id, component_plan_id)",
    "CREATE INDEX idx_pricing_bundle_component_revision ON bss.pricing_bundle_component USING btree (tenant_id, bundle_id, plan_revision)",
    "CREATE OR REPLACE FUNCTION bss.pricing_bundle_component_append_only() RETURNS trigger AS $$
        DECLARE
          parent_state text;
        BEGIN
          IF TG_OP <> 'INSERT' THEN
            SELECT p.lifecycle_state INTO parent_state
              FROM bss.pricing_bundle b
              JOIN bss.pricing_plan p ON p.plan_id = b.plan_id
             WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision;
            IF parent_state IS DISTINCT FROM 'draft' THEN
              RAISE EXCEPTION
                'pricing_bundle_component: % of a component under a non-draft plan revision is not permitted (state %)',
                TG_OP, coalesce(parent_state, 'missing');
            END IF;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RETURN OLD;
          END IF;

          SELECT p.lifecycle_state INTO parent_state
            FROM bss.pricing_bundle b
            JOIN bss.pricing_plan p ON p.plan_id = b.plan_id
           WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision;
          IF parent_state IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION
              'pricing_bundle_component: % of a component under a non-draft plan revision is not permitted (state %)',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_bundle_component_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_bundle_component FOR EACH ROW EXECUTE FUNCTION bss.pricing_bundle_component_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_bundle_component",
    "DROP FUNCTION IF EXISTS bss.pricing_bundle_component_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_bundle_component (
            tenant_id         text   NOT NULL,
            bundle_id         text   NOT NULL,
            plan_revision     bigint NOT NULL,
            component_plan_id text   NOT NULL,
            included_sku_id   text   NOT NULL,
            max_qty           int,
            min_qty           int,
            PRIMARY KEY (bundle_id, plan_revision, component_plan_id),
            CONSTRAINT chk_pricing_bundle_component_min_qty CHECK (min_qty IS NULL OR min_qty >= 0),
            CONSTRAINT chk_pricing_bundle_component_qty_range CHECK (min_qty IS NULL OR max_qty IS NULL OR min_qty <= max_qty),
            CONSTRAINT fk_pricing_bundle_component_bundle FOREIGN KEY (bundle_id) REFERENCES pricing_bundle(bundle_id)
        )",
    "CREATE INDEX idx_pricing_bundle_component_plan ON pricing_bundle_component (tenant_id, component_plan_id)",
    "CREATE INDEX idx_pricing_bundle_component_revision ON pricing_bundle_component (tenant_id, bundle_id, plan_revision)",
    "CREATE TRIGGER trg_pricing_bundle_component_no_delete BEFORE DELETE ON pricing_bundle_component FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_component: DELETE of a component under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision AND p.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_bundle_component_no_insert BEFORE INSERT ON pricing_bundle_component FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_component: INSERT of a component under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision AND p.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_bundle_component_no_update BEFORE UPDATE ON pricing_bundle_component FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_component: UPDATE of a component under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision AND p.lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision AND p.lifecycle_state = 'draft'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_bundle_component"];

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
