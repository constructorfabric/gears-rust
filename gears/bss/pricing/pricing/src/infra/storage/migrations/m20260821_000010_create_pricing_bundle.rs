//! Create `bss.pricing_bundle` — a bundle's identity, its price basis and its
//! invoice itemization (`design/08-bundles.md` §6, D-105). The first of Slice
//! 8's four tables and the parent the other three key on.
//!
//! §6's column list is `price_basis`, `invoice_itemization` and "lifecycle
//! refs", over a primary key of `bundle_id` and the `plan_id` D-105 added. The
//! lifecycle refs are the `plan_id`: §4 of that document states the bundle has
//! **no slice-owned state machine** and rides the plan lifecycle of Slices 2/11
//! on its `bundle`-type SKU, so the reference *is* the lifecycle ref and a
//! second `lifecycle_state` column here would be a second answer to what state a
//! bundle is in.
//!
//! # `plan_id` carries no foreign key, and D-105 assumed it could
//!
//! D-105 gives this table *"the `plan_id` FK the revision keying presupposes"*.
//! A foreign key is the one thing that cannot be declared on it. `pricing_plan`
//! is keyed `(plan_id, revision)` (D-56), and the only uniqueness it has on
//! `plan_id` **alone** is in two *partial* indexes — `uq_pricing_plan_current`
//! (`WHERE lifecycle_state IN ('published','retired')`) and
//! `uq_pricing_plan_open_draft` (`WHERE lifecycle_state = 'draft'`). Postgres
//! refuses a partial index as a foreign key's referent, so the constraint is not
//! expressible against the plan table as it stands, and a `(plan_id, revision)`
//! pair is not available either: this row is the bundle's identity and does not
//! belong to one revision.
//!
//! The reference is nonetheless enforced, one table down and at full strength:
//! the three composition tables carry `bundle_id` with a **real** foreign key
//! onto this table, and their append-only triggers resolve `plan_id` through it
//! to read the owning revision's `lifecycle_state`. So an orphan bundle row is
//! possible and an orphan *composition* is not, which is where the integrity
//! actually has to hold. The divergence from D-105 is reported rather than
//! smoothed over, and is in the owed register.
//!
//! # One bundle per plan (`uq_pricing_bundle_plan`), an addition
//!
//! §6 names no uniqueness on `plan_id`. Without it a plan carries two bundle
//! rows, and then "the bundle's plan revisions" (D-92) names two composition
//! chains inside one revision sequence: a component set frozen at revision *N*
//! would have two parents and the projector no way to choose. It is a total
//! `UNIQUE` rather than a partial one because it constrains identity, not
//! lifecycle — the bundle row does not have a lifecycle of its own.
//!
//! **The index below is no longer the one the schema carries.**
//! D-340 puts `(tenant_id, plan_id)` in it, and
//! every word above survives that: one plan still carries at most one bundle. What
//! the statement here additionally asserted, and what nothing above argues for, is
//! that a `plan_id` belongs to one bundle **across every tenant** — on the one
//! column of this table that is a client-supplied reference and the one the
//! paragraph above explains cannot carry a foreign key. The first tenant to name a
//! `plan_id` therefore locked every other tenant out of it, irreversibly. Read
//! D-340 for the whole of it; this statement stays as written because
//! it is the state the chain passes through.
//!
//! # `price_basis` and `invoice_itemization` are **not** revision-scoped, and
//!
//! # that is D-92's own defect arriving one row up
//!
//! Stated here because building is where it becomes visible. D-92 puts the
//! revision discipline on *"the three composition tables below"* and this row is
//! not one of them, so `price_basis` and `invoice_itemization` are mutated **in
//! place**. Both are D-104 always-material triggers — a basis change and an
//! itemization change each route through the two-person workflow — and an
//! in-place column means the published revision reads the new value from the
//! moment it is authored, before any approver has seen it. That is precisely the
//! defect D-92 was written to close ("a draft recomposition of a published
//! bundle mutates the published truth state"), one table above where it looked.
//!
//! Nothing is invented here to close it: the shape is §6's and D-105's as
//! written, the hazard is in the owed register with a proposed decision, and the
//! publish path re-reads this row inside its commit so the value it validates is
//! the value it publishes. A reader who reaches for this table's columns as
//! frozen truth should read that entry first.
//!
//! **Backend differences.** `uuid` becomes `text` and the `bss.` qualification
//! is dropped, as elsewhere in this chain. Every `CHECK`, index and the primary
//! key are preserved on both sides. No trigger: this table has no revision to be
//! frozen with, which is the subject of the paragraph above.
//!
//! `uq_pricing_bundle_plan` gains `tenant_id` — D-340's class, one table over.
//!
//! `pricing_bundle` added the index and argued it: §6 names no uniqueness on
//! `plan_id`, and without one a plan carries two bundle rows, so "the bundle's
//! plan revisions" (D-92) names two composition chains inside one revision
//! sequence and the projector has no way to choose. That argument is entirely
//! about **one plan**, and every word of it survives here. What it never said, and
//! what the index nevertheless asserted, is that a `plan_id` belongs to one
//! bundle **across the whole table, every tenant's included**.
//!
//! # Why that is an isolation defect and not a modelling nit
//!
//! `plan_id` is the one column on this table that is a client-supplied reference
//! to another table: `POST /bss-pricing/v1/bundles` takes it from the request
//! body, and `pricing_bundle` holds no foreign key at all — a fact
//! `pricing_bundle`'s module doc states and explains, `pricing_plan` having no
//! total uniqueness on `plan_id` alone for a foreign key to name. So the index
//! was the *only* thing in the schema with an opinion about a plan id a caller
//! supplied, and its opinion spanned every tenant.
//!
//! Two consequences, measured on this branch 2026-08-17 and reproduced in
//! `tests/sqlite_bundle_repo.rs` before this migration existed:
//!
//! - **An existence oracle.** A caller naming another tenant's `plan_id` was told
//!   `BUNDLE_EXISTS_ON_PLAN` when that tenant had a bundle and `201 Created` when
//!   it did not. The repository's own pre-check is tenant-scoped and found nothing
//!   either way, so the whole discrimination came from this index.
//! - **A cross-tenant denial that is irreversible from the victim's side.** In the
//!   `201` case the row *lands*, occupying the owning tenant's bundle slot. That
//!   tenant is then refused `BUNDLE_EXISTS_ON_PLAN` against a row it cannot read,
//!   in a tenant it cannot see, and `pricing_bundle` has no `DELETE` path anywhere
//!   in the API.
//!
//! The oracle is closed one layer up, by `bundle_repo::create_on` reading the plan
//! in the caller's scope and answering `NotFound` — the two halves close different
//! things and both are wanted. This one is what stops the slot being *takeable*,
//! which is the half that matters on a database that already holds a squatted row
//! and the half that decides a race, a read being unable to.
//!
//! # The index is already read as if it carried the tenant
//!
//! `bundle_repo::bundle_of_plan` filters `tenant_id.eq(…)` **and**
//! `plan_id.eq(…)`, and `list_page`'s doc says the filtered page answers *"does
//! this plan carry a bundle"* — a question that is only well posed per tenant. The
//! index was therefore **narrower than its only reader assumes**, which is the
//! precise inversion of the usual hazard and the reason the widening removes no
//! guarantee anything relied on: `(tenant_id, plan_id)` still admits exactly one
//! bundle per plan for the tenant that owns it, and D-92's ambiguity cannot arise.
//!
//! # About this file
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
    "CREATE TABLE bss.pricing_bundle (
            tenant_id           uuid NOT NULL,
            bundle_id           uuid NOT NULL,
            invoice_itemization text NOT NULL,
            plan_id             uuid NOT NULL,
            price_basis         text NOT NULL,
            CONSTRAINT chk_pricing_bundle_invoice_itemization CHECK (invoice_itemization IN ('aggregate', 'itemize')),
            CONSTRAINT chk_pricing_bundle_price_basis CHECK (price_basis IN ('sum_of_parts', 'own_price')),
            CONSTRAINT pricing_bundle_pkey PRIMARY KEY (bundle_id)
        )",
    "CREATE INDEX idx_pricing_bundle_tenant ON bss.pricing_bundle USING btree (tenant_id, bundle_id)",
    "CREATE UNIQUE INDEX uq_pricing_bundle_plan ON bss.pricing_bundle USING btree (tenant_id, plan_id)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_bundle"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_bundle (
            tenant_id           text NOT NULL,
            bundle_id           text NOT NULL,
            invoice_itemization text NOT NULL,
            plan_id             text NOT NULL,
            price_basis         text NOT NULL,
            PRIMARY KEY (bundle_id),
            CONSTRAINT chk_pricing_bundle_invoice_itemization CHECK (invoice_itemization IN ('aggregate', 'itemize')),
            CONSTRAINT chk_pricing_bundle_price_basis CHECK (price_basis IN ('sum_of_parts', 'own_price'))
        )",
    "CREATE INDEX idx_pricing_bundle_tenant ON pricing_bundle (tenant_id, bundle_id)",
    "CREATE UNIQUE INDEX uq_pricing_bundle_plan ON pricing_bundle (tenant_id, plan_id)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_bundle"];

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
