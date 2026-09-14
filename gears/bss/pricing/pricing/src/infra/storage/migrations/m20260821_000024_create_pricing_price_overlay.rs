//! Create `bss.pricing_price_overlay` — the overlay header and its revision
//! chain (`design/09-price-overlays.md` §6, D-42, D-92, D-107).
//!
//! An overlay is a **line container**, not an adjustment (D-42): this row is the
//! header — scope, precedence, dating, tax basis, disclosure — and the lines it
//! contains live in `pricing_price_overlay_line`'s table. The pre-D-42 single-adjustment
//! overlay is the degenerate one-line case, so nothing about this shape loses it.
//!
//! # The precedence index is **partial**, and D-107 is why
//!
//! §6 spells it `UNIQUE (tenant_id, scope_class, precedence) WHERE
//! lifecycle_state = 'published'`, and the predicate is load-bearing rather than
//! an optimisation. D-92 gave overlays coexisting `draft` / `published` /
//! `superseded` revision rows on one `price_overlay_id`; an unqualified index
//! makes a draft revision of a published overlay collide **with itself**, so
//! every edit of a live overlay fails `PRECEDENCE_DUPLICATE` and an overlay is
//! authorable exactly once. `sqlite_overlay_store`'s
//! `a_draft_revision_may_reuse_its_own_published_revisions_precedence` is the
//! case that fails the moment the `WHERE` is dropped, and its sibling
//! `two_published_overlays_of_one_class_may_not_share_a_precedence` is the case
//! that fails if the index is dropped instead — partial does not mean absent.
//!
//! This is the same treatment the price rows and plan revisions already carry
//! (`uq_pricing_price_scope_key_current`, `uq_pricing_plan_current`).
//!
//! # `scope_value` renders the classless scope as the empty string
//!
//! §6 says `scope_value` is *"taxonomy-validated per class"*, and the `global`
//! class has no taxonomy and no value. The two representable choices are a NULL
//! and a sentinel, and this table takes the sentinel under one biconditional
//! `CHECK`: `(scope_class = 'global') = (length(scope_value) = 0)`.
//!
//! A NULL would have been the obvious reading and is the worse one here. Every
//! index and every comparison this column takes part in — the scope enumeration
//! index, `inst-plv-dating`'s per-line-key overlap walk, `inst-plv-scope`'s
//! taxonomy lookup — would then need a null-safe form, and `Column::X.eq(None)`
//! renders `X = NULL` and matches nothing, which `SeaORM` builds happily. The
//! empty string makes the column total and the comparisons ordinary. It is safe
//! as a sentinel for exactly `pricing_price`'s scope-key reason about `COALESCE(meter,
//! '')`: **a taxonomy value may not be blank** — all four taxonomy tables carry
//! `chk_*_value_present` — so `''` denotes "no scope value" and nothing else can
//! render it.
//!
//! # `row_version` is an addition to §6's column list, and it is not decoration
//!
//! §6 lists no concurrency column. Without one the `PATCH` half of
//! `POST/PATCH /bss-pricing/v1/price-overlays` has no entity tag to answer
//! `If-Match` with, and two authors editing one draft revision would both
//! satisfy their precondition — the lost update `fr-concurrent-edit` exists to
//! refuse. It is the `pricing_plan` revision's column under the same name and
//! the same D-170 tag shape (`"<revision>-<version>"`), so the overlay plane
//! answers preconditions the way every other authoring plane in this gear does.
//! Reported in the owed register rather than smoothed over.
//!
//! # What is deliberately **not** here
//!
//! No `created_by` / `created_at_utc`. `pricing_plan` carries them and this
//! table does not, because §6 lists neither and `pricing_audit_log` already
//! holds the actor, the instant and the before/after of every overlay mutation
//! (`inst-rb-audit`). A second copy of the actor on the row is a second thing to
//! keep true, and it is the copy that goes stale.
//!
//! No `overlay_index` projection column and no read-model coupling: D-112 and
//! D-133's sharded index is read-model territory and carries an open question of
//! its own (the `global` sentinel is one shard for every classless overlay, so
//! the sharding divides by one there).
//!
//! # The append-only trigger
//!
//! A published revision row is immutable in content (D-92): the projector — warm
//! and re-drive alike — reads the **published revision's** rows, so a draft edit
//! that could reach a published row would re-materialize a frozen
//! `CatalogVersion` at different content. The trigger freezes every content
//! column once the row leaves `draft`, and admits exactly two flips:
//! `draft -> published` (the submit) and `published -> superseded` (the same
//! submit, on the predecessor, in one commit).
//!
//! **DELETE is refused off the draft plane and permitted on it.** The overlay's
//! three states carry no `abandoned` tombstone the way `pricing_plan` does
//! (§6 names three states, not four), so a discarded draft revision leaves by
//! DELETE and there is no other way for it to leave. What must never be
//! deletable is a revision some `CatalogVersion` froze, and that is what the
//! guard says.
//!
//! **Backend differences.** Postgres carries the rule as one PL/pgSQL trigger
//! function with the offending state interpolated; `SQLite` has no procedural
//! language and `RAISE(ABORT, ...)` takes a literal message, so the same rule
//! becomes four fixed-message triggers. `uuid` becomes `text`, `timestamptz`
//! becomes `text`, `jsonb` becomes `text`, and the `bss.` qualification is
//! dropped. Every `CHECK`, index and the primary key are preserved on both
//! sides.
//!
//! `abandoned` joins the overlay's lifecycle — D-231's code half.
//!
//! §6 gave `pricing_price_overlay` three states and no tombstone, so a discarded
//! draft revision left by `DELETE` and the number it consumed went with it. The
//! next `open_revision` then re-minted that number, and a client still holding
//! the discarded draft's entity tag found a **different** revision under the same
//! overlay identity — its stale `If-Match` matching the fresh row's
//! compare-and-swap and landing an edit against content that no longer exists.
//!
//! `pricing_plan` has had the answer since D-145 and this is deliberately its
//! shape, not a variation on it: a terminal `draft -> abandoned` flip, `DELETE`
//! refused outright, and a number that is never freed.
//!
//! # What actually closes the hazard is the `DELETE` refusal, not the new state
//!
//! Adding `abandoned` to the `CHECK` alone would leave `DELETE` available and the
//! re-mint reachable by the path it already took. So the flip is *sanctioned* and
//! the deletion is *removed*, in one migration, and `open_revision`'s
//! `max(revision) + 1` becomes a true high-water read for the first time — it was
//! already written that way (a `superseded` predecessor can outrank the published
//! row, which is its own smaller reason) and simply could not see a deleted row.
//!
//! # `abandoned` is terminal, and it costs no new arm to make it so
//!
//! The post-draft branch already refuses every flip except `published ->
//! superseded`, so a row that reaches `abandoned` is frozen in content by the
//! column whitelist and left by no flip — without a clause naming `abandoned` at
//! all. The only edit the state machine needs is the draft's **exit** list, which
//! is why the diff is smaller than the rule.
//!
//! # Why the open-draft index needs no predicate change
//!
//! `uq_pricing_price_overlay_open_draft` is partial on `lifecycle_state =
//! 'draft'`. An abandoned row is not a draft, so it leaves the index the moment it
//! flips and a fresh draft opens against the same overlay — which is the whole
//! mechanism: the tombstone occupies a **revision number** without occupying the
//! *open draft* slot. Had the index been unconditional, a tombstone would have
//! blocked every future revision.
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
    "CREATE TABLE bss.pricing_price_overlay (
            tenant_id        uuid        NOT NULL,
            price_overlay_id uuid        NOT NULL,
            revision         bigint      NOT NULL,
            disclosure       text        NOT NULL DEFAULT 'restricted'::text,
            effective_from   timestamptz,
            effective_to     timestamptz,
            lifecycle_state  text        NOT NULL,
            precedence       integer     NOT NULL,
            scope_class      text        NOT NULL,
            scope_value      text        NOT NULL,
            target_ref       jsonb       NOT NULL DEFAULT '{}'::jsonb,
            tax_basis        text        NOT NULL,
            row_version      bigint      NOT NULL DEFAULT 0,
            CONSTRAINT chk_pricing_price_overlay_disclosure CHECK (disclosure IN ('restricted', 'public')),
            CONSTRAINT chk_pricing_price_overlay_interval CHECK (effective_from IS NULL OR effective_to IS NULL OR effective_to > effective_from),
            CONSTRAINT chk_pricing_price_overlay_lifecycle_state CHECK (lifecycle_state IN ('draft', 'published', 'superseded', 'abandoned')),
            CONSTRAINT chk_pricing_price_overlay_revision CHECK (revision >= 0),
            CONSTRAINT chk_pricing_price_overlay_row_version CHECK (row_version >= 0),
            CONSTRAINT chk_pricing_price_overlay_scope_class CHECK (scope_class IN ( 'partner', 'org_tier', 'brand', 'region', 'customer_group', 'global')),
            CONSTRAINT chk_pricing_price_overlay_scope_value CHECK ((scope_class = 'global') = (length(scope_value) = 0)),
            CONSTRAINT chk_pricing_price_overlay_tax_basis CHECK (tax_basis IN ('inclusive', 'exclusive', 'delegated_tariffs')),
            CONSTRAINT pricing_price_overlay_pkey PRIMARY KEY (price_overlay_id, revision)
        )",
    "CREATE INDEX idx_pricing_price_overlay_scope ON bss.pricing_price_overlay USING btree (tenant_id, scope_class, scope_value, lifecycle_state)",
    "CREATE UNIQUE INDEX uq_pricing_price_overlay_open_draft ON bss.pricing_price_overlay USING btree (price_overlay_id) WHERE (lifecycle_state = 'draft'::text)",
    "CREATE UNIQUE INDEX uq_pricing_price_overlay_precedence ON bss.pricing_price_overlay USING btree (tenant_id, scope_class, precedence) WHERE (lifecycle_state = 'published'::text)",
    "CREATE OR REPLACE FUNCTION bss.pricing_price_overlay_append_only() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION
              'pricing_price_overlay: DELETE of revision % of overlay % is not permitted; a discarded draft revision is abandoned',
              OLD.revision, OLD.price_overlay_id;
          END IF;

          -- The draft plane is where content moves, so its columns are
          -- unguarded - but its exits are not. A draft leaves by publishing or by
          -- being abandoned (D-231), and by nothing else; `draft -> superseded`
          -- would mint a superseded row that never published, which the projector
          -- would then source from.
          IF OLD.lifecycle_state = 'draft' THEN
            IF NEW.lifecycle_state NOT IN ('draft', 'published', 'abandoned') THEN
              RAISE EXCEPTION
                'pricing_price_overlay: lifecycle_state % -> % is not a sanctioned flip',
                OLD.lifecycle_state, NEW.lifecycle_state;
            END IF;
            RETURN NEW;
          END IF;

          -- Past here the row is published, superseded or abandoned. Once
          -- abandoned it is a tombstone: frozen in content by the whitelist below
          -- and left by no flip, so the number it consumed can never be attached
          -- to a different shape.

          IF NEW.price_overlay_id IS DISTINCT FROM OLD.price_overlay_id
          OR NEW.revision         IS DISTINCT FROM OLD.revision
          OR NEW.tenant_id        IS DISTINCT FROM OLD.tenant_id
          OR NEW.scope_class      IS DISTINCT FROM OLD.scope_class
          OR NEW.scope_value      IS DISTINCT FROM OLD.scope_value
          OR NEW.precedence       IS DISTINCT FROM OLD.precedence
          OR NEW.effective_from   IS DISTINCT FROM OLD.effective_from
          OR NEW.effective_to     IS DISTINCT FROM OLD.effective_to
          OR NEW.tax_basis        IS DISTINCT FROM OLD.tax_basis
          OR NEW.disclosure       IS DISTINCT FROM OLD.disclosure
          OR NEW.target_ref       IS DISTINCT FROM OLD.target_ref
          OR NEW.row_version      IS DISTINCT FROM OLD.row_version THEN
            RAISE EXCEPTION
              'pricing_price_overlay: revision % of overlay % is frozen; only a sanctioned lifecycle_state flip is permitted',
              OLD.revision, OLD.price_overlay_id;
          END IF;

          IF NEW.lifecycle_state IS DISTINCT FROM OLD.lifecycle_state
             AND NOT (OLD.lifecycle_state = 'published'
                      AND NEW.lifecycle_state = 'superseded') THEN
            RAISE EXCEPTION
              'pricing_price_overlay: lifecycle_state % -> % is not a sanctioned flip',
              OLD.lifecycle_state, NEW.lifecycle_state;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_price_overlay_append_only BEFORE DELETE OR UPDATE ON bss.pricing_price_overlay FOR EACH ROW EXECUTE FUNCTION bss.pricing_price_overlay_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_price_overlay",
    "DROP FUNCTION IF EXISTS bss.pricing_price_overlay_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_price_overlay (
            tenant_id        text   NOT NULL,
            price_overlay_id text   NOT NULL,
            revision         bigint NOT NULL,
            disclosure       text   NOT NULL DEFAULT 'restricted',
            effective_from   text,
            effective_to     text,
            lifecycle_state  text   NOT NULL,
            precedence       int    NOT NULL,
            scope_class      text   NOT NULL,
            scope_value      text   NOT NULL,
            target_ref       text   NOT NULL DEFAULT '{}',
            tax_basis        text   NOT NULL,
            row_version      bigint NOT NULL DEFAULT 0,
            PRIMARY KEY (price_overlay_id, revision),
            CONSTRAINT chk_pricing_price_overlay_disclosure CHECK (disclosure IN ('restricted', 'public')),
            CONSTRAINT chk_pricing_price_overlay_interval CHECK (effective_from IS NULL OR effective_to IS NULL OR effective_to > effective_from),
            CONSTRAINT chk_pricing_price_overlay_lifecycle_state CHECK (lifecycle_state IN ('draft', 'published', 'superseded', 'abandoned')),
            CONSTRAINT chk_pricing_price_overlay_revision CHECK (revision >= 0),
            CONSTRAINT chk_pricing_price_overlay_row_version CHECK (row_version >= 0),
            CONSTRAINT chk_pricing_price_overlay_scope_class CHECK (scope_class IN ( 'partner', 'org_tier', 'brand', 'region', 'customer_group', 'global')),
            CONSTRAINT chk_pricing_price_overlay_scope_value CHECK ((scope_class = 'global') = (length(scope_value) = 0)),
            CONSTRAINT chk_pricing_price_overlay_tax_basis CHECK (tax_basis IN ('inclusive', 'exclusive', 'delegated_tariffs'))
        )",
    "CREATE INDEX idx_pricing_price_overlay_scope ON pricing_price_overlay (tenant_id, scope_class, scope_value, lifecycle_state)",
    "CREATE UNIQUE INDEX uq_pricing_price_overlay_open_draft ON pricing_price_overlay (price_overlay_id) WHERE lifecycle_state = 'draft'",
    "CREATE UNIQUE INDEX uq_pricing_price_overlay_precedence ON pricing_price_overlay (tenant_id, scope_class, precedence) WHERE lifecycle_state = 'published'",
    "CREATE TRIGGER trg_pricing_price_overlay_draft_exit BEFORE UPDATE ON pricing_price_overlay FOR EACH ROW WHEN OLD.lifecycle_state = 'draft' AND NEW.lifecycle_state NOT IN ('draft', 'published', 'abandoned') BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay: that lifecycle_state move is not a sanctioned flip'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_frozen_columns BEFORE UPDATE ON pricing_price_overlay FOR EACH ROW WHEN OLD.lifecycle_state <> 'draft' AND (NEW.price_overlay_id IS NOT OLD.price_overlay_id OR NEW.revision IS NOT OLD.revision OR NEW.tenant_id IS NOT OLD.tenant_id OR NEW.scope_class IS NOT OLD.scope_class OR NEW.scope_value IS NOT OLD.scope_value OR NEW.precedence IS NOT OLD.precedence OR NEW.effective_from IS NOT OLD.effective_from OR NEW.effective_to IS NOT OLD.effective_to OR NEW.tax_basis IS NOT OLD.tax_basis OR NEW.disclosure IS NOT OLD.disclosure OR NEW.target_ref IS NOT OLD.target_ref OR NEW.row_version IS NOT OLD.row_version) BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay: the revision is frozen; only a sanctioned lifecycle_state flip is permitted'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_frozen_flip BEFORE UPDATE ON pricing_price_overlay FOR EACH ROW WHEN OLD.lifecycle_state <> 'draft' AND NEW.lifecycle_state IS NOT OLD.lifecycle_state AND NOT (OLD.lifecycle_state = 'published' AND NEW.lifecycle_state = 'superseded') BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay: that lifecycle_state move is not a sanctioned flip'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_no_delete BEFORE DELETE ON pricing_price_overlay FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay: DELETE of a revision is not permitted; a discarded draft revision is abandoned'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_price_overlay"];

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
