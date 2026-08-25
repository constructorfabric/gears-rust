//! Create `bss.pricing_price_overlay_line` — the adjustment lines of one
//! overlay revision (`design/09-price-overlays.md` §6, D-42, D-67, D-78, D-92,
//! D-138).
//!
//! D-42 made an overlay a **container of one or more adjustment lines**, each
//! keyed `(planId?, targetSku?)` with its own kind and magnitude, and D-78
//! extended that key with `cohort`. This table is that key.
//!
//! # The primary key is `(line_id, overlay_revision)`, and §6's is not buildable
//!
//! §6 gives this table **`PK line_id`** and, in the same parenthesis,
//! *"**copy-on-new-revision** with stable line identity where unchanged,
//! D-92"*. Those two sentences cannot both hold. A copy-on-new-revision writes a
//! **second row** for the same line under the successor revision; under a
//! `line_id` primary key that second row needs a **new** `line_id`, and then the
//! line's identity is not stable across revisions — which is the half of the
//! sentence a consumer diffing two revisions actually uses, and the half D-92
//! is about.
//!
//! The primary key is what gives, because D-92 is a decision and the key is a
//! spelling of one. `(line_id, overlay_revision)` makes both sentences true at
//! once: a line keeps its id across every revision that does not change it, and
//! each revision holds its own copy. `pricing_price_overlay_line_amount`'s
//! foreign key widens with it, for the reason §6 gives that table — it *"rides
//! the same revision through its line"*, which is only expressible if the line's
//! key carries the revision.
//!
//! This divergence from §6's literal column list is reported in the owed
//! register rather than smoothed over. Nothing else about the table moves: the
//! null-safe line key below is unchanged, and it is what stops one revision
//! holding the same *logical* line twice.
//!
//! # The uniqueness is **null-safe**, and the naive form is silently wrong
//!
//! §6 states `UNIQUE (price_overlay_id, overlay_revision, plan_id, target_sku,
//! cohort)` and adds *"(null-safe — one default line, one line per plan, one per
//! `(plan, sku)`…)"*. A plain `UNIQUE` over those five columns delivers none of
//! that: inside a `UNIQUE` index NULLs are **distinct** on both engines, and all
//! three of `plan_id`, `target_sku` and `cohort` are nullable — so the very case
//! §6 spells *"one default line"* (all three NULL) would admit any number of
//! rows, and so would "one line per plan" (two of the three NULL). The rule
//! would be stated, indexed, and enforcing nothing.
//!
//! The index therefore keys over `COALESCE`d sentinels, which is
//! `pricing_price`'s scope-key treatment of `meter`, applied to three columns at once.
//! Each sentinel is chosen so nothing else can render it:
//!
//! * `target_sku` -> `''`. A SKU may not be blank; `overlay_repo` refuses one.
//! * `cohort` -> `'-infinity'` on Postgres, `''` on `SQLite`. A cohort is a
//!   cutover instant and no cutover is at negative infinity. The two backends
//!   differ here because each sentinel is type-native to its column, which is
//!   what keeps the expression from silently coercing.
//! * `plan_id` -> the **nil uuid**. A plan id is a v4/v7 uuid and never nil.
//!
//! **Two of those three sentinels are spellable from the wire**, so the table
//! refuses them itself rather than relying on a repository to: a request naming
//! `00000000-…-0000` as its plan, or `''` as its SKU, would key as the
//! list-default line and collide with it. `chk_..._plan_id_not_nil` and
//! `chk_..._target_sku_present` are what make the index self-enforcing.
//!
//! That is the argument the four taxonomy tables already took for the sibling
//! sentinel — `pricing_region_taxonomy`'s *"A blank value is refused, and that is not
//! tidiness"*, which exists so `pricing_price_overlay.scope_value`'s `''` cannot
//! be forged. It was not carried here at first, and the module doc claimed "the
//! constraint is the guarantee and the check is what makes the refusal legible"
//! while for `plan_id` it was the other way round. `overlay_repo` still refuses a
//! nil plan id, so the caller reads a typed refusal rather than a `CHECK`
//! violation; now the sentence is true as well.
//!
//! # The primary key below is no longer the one the schema carries
//!
//! `pricing_price_overlay_line_amount` widened it to `(tenant_id, overlay_revision, line_id)`
//! on
//! 2026-08-18 (review A1-3), and both halves of the argument this doc makes for
//! `(line_id, overlay_revision)` survive it — the revision is in the key for the
//! reason given, and §6's literal `PK line_id` is still unbuildable. What the
//! narrow pair additionally asserted, and what nothing here argues for, is that a
//! **client-supplied** `line_id` belongs to one overlay per revision number across
//! every tenant. The child amount table's key and foreign key moved in the same
//! migration; read that one for the whole of it. The statements below stay as
//! written, being the state the chain passes through.
//!
//! # The `CHECK`s, and which decision each is
//!
//! * `cohort_needs_plan` (§6, 2026-07-31 review fix) — a `cohort` is validated
//!   against *"the line's target plan"*, which the list-default line has none of.
//!   Targeting a generation across every plan of a scope is authored as per-plan
//!   lines, never as a cohort-carrying default line.
//! * `sku_needs_plan` (§3 step 2a) — a bare SKU id is ambiguous per
//!   `(currency, region)`, so a sku-only line names no resolvable target.
//! * `magnitude_pairing` (D-08) — `(magnitude_kind = 'percent_bp') =
//!   (adjustment_value IS NOT NULL)`. The value type is **declared, never
//!   inferred from the presence of amount rows**: implicit-absence semantics are
//!   forbidden by the Foundation, and a bp value read as minor units mis-prices
//!   by orders of magnitude.
//! * `fixed_is_amount` (D-138) — `fixed` **replaces** the running line amount
//!   with an absolute price, so a percentage of the amount it is replacing
//!   evaluates to nothing.
//! * `magnitude_positive` + `discount_ceiling` (D-67) — `0 < v` on both kinds and
//!   `v <= 10000` on a discount. Before D-67 the only checks were duplicate line
//!   keys, out-of-scope targets, per-currency coverage and tax-basis
//!   declaration, so `discount / percent_bp = 15000` — the "150% of list"
//!   data-entry inversion — passed every stated validation. Two `CHECK`s rather
//!   than one compound arm because they are two rules and a compound refusal
//!   would not tell an author which bound they crossed.
//!
//! # The resolution order is **not** here, and that is D-42's own line
//!
//! `(plan, sku) > (plan) > default` is a **pipeline rule** (`inst-plv-lines`),
//! not an index: it selects which of several legal lines applies to a priced
//! row, and every one of them is legal. It lives in `domain::overlay_rules`, and
//! it is never `precedence` — precedence orders **lists** against each other and
//! never lines inside one.
//!
//! # The append-only trigger
//!
//! Lines are frozen with the revision that published them (D-92,
//! copy-on-new-revision). INSERT is guarded as well as UPDATE and DELETE,
//! because an INSERT is the one verb that **adds** a line to a frozen revision;
//! an UPDATE is checked against both the `OLD` parent and the `NEW` one, since
//! re-pointing `overlay_revision` is how one would otherwise append to a frozen
//! revision without ever issuing an INSERT. This is
//! `pricing_bundle_component`'s arrangement one slice over, one join shorter.
//!
//! **The trigger answers ahead of the composite foreign key**, and the suite
//! says so rather than asserting a message it will not get: an INSERT naming an
//! absent `(price_overlay_id, overlay_revision)` trips the `NOT EXISTS` in the
//! `BEFORE INSERT` trigger before `SQLite` evaluates the foreign key. The
//! foreign key is proved on its own by a DELETE of a **draft** revision that
//! still carries lines, which the header's own trigger permits and this table's
//! reference refuses.
//!
//! **Backend differences.** As `pricing_price_overlay`, plus: the expression index is
//! written per backend because the `cohort` sentinel is type-native to each. Note
//! that an expression index changes `SQLite`'s uniqueness message from the column
//! list to `index '<name>'`, while Postgres names the index either way — which is
//! why both suites assert the **index name**.
//!
//! `pricing_price_overlay_line`'s primary key gains `tenant_id`, and its amount
//! table's key and foreign key move with it — D-340's class, review A1-3.
//!
//! `pricing_price_overlay_line` keyed the line `(line_id, overlay_revision)` and argued the
//! pair at length: `overlay_revision` because a copy-on-new-revision needs the
//! revision in the key or D-92's stable line identity is unexpressible, and
//! `line_id` because §6's literal `PK line_id` cannot hold two copies of one line.
//! Both halves survive here untouched. What that argument never mentions — the
//! module doc never uses the word *tenant* — is that a line id therefore belonged
//! to one overlay per revision **number across the entire table, every tenant's
//! included**.
//!
//! # `line_id` is client-supplied, and supplying it is the documented usage
//!
//! `api/rest/overlays.rs` renders it `request.line_id.unwrap_or_else(Uuid::now_v7)`
//! and the route description names the read-modify-write round trip as the
//! intended flow, because that is what makes D-92's *"the identity is stable"* true
//! of an authored edit and not only of the server's copy-forward. It is one of
//! exactly three client-supplied ids in this schema; the other two were
//! `phase_id` and `composite_id`, both scoped by D-340.
//!
//! # Why this one was Medium and what remained of it
//!
//! Unlike the other two, the refusal here was already **typed**:
//! `overlay_repo::is_line_identity_collision` gates on `is_unique_violation()`
//! before matching the constraint, and the caller gets
//! `RepoError::ValueOutOfRange { field: "line_id", … }` rather than a `500`. So
//! the caller-fixable half was fixed. What remained is the isolation half, and a
//! 4xx that discriminates is still an oracle: the difference between the refusal
//! and the `200` answered *is this line id in use somewhere I cannot read*. The
//! permanent-lockout half applies identically — the first tenant to take a line id
//! at a revision number held it against every other tenant, at that number, for
//! good.
//!
//! # What this migration does **not** do, and the reason is a schema decision
//!
//! The review's stated fix is `(tenant_id, price_overlay_id, overlay_revision,
//! line_id)`. This migration widens by `tenant_id` alone, to
//! `(tenant_id, overlay_revision, line_id)`, and the `price_overlay_id` half is
//! deliberately left — recorded here rather than quietly dropped.
//!
//! The two halves close different things and only one of them is isolation.
//! `tenant_id` closes it completely: no id a caller supplies can collide with, or
//! be probed against, a row in another tenant. `price_overlay_id` would close a
//! second, **intra-tenant** question — that pasting a line id from one of a
//! tenant's own overlays into another at the same revision collides — which is the
//! caller-fixable half that is already typed, already legible, and already
//! remediable by the author who caused it.
//!
//! And it is not a mechanical widening. `pricing_price_overlay_line_amount` does
//! not carry `price_overlay_id`, so a parent key containing it could not be
//! referenced by the child at all: the column would have to be **added** to the
//! amount table and backfilled through the line, which is a change to what the
//! child row *is* and not to how the parent is keyed. That is worth deciding on
//! its own rather than as a rider here.
//!
//! # The child moves in the same migration, and A1-4 is why
//!
//! `pricing_price_overlay_line_amount` is keyed `(line_id, overlay_revision,
//! currency)` and its insert maps every failure to `RepoError::Db` → `500`. That
//! catch-all is unreachable today for a reason review A1-4 records exactly: the
//! line insert refuses the colliding `line_id` first, and a `line_id` past that
//! check was unique at that revision *globally*, so its amounts were too. A1-4
//! then names the condition that arms it — *"if A1-3's fix scopes the line key by
//! tenant, then two tenants can hold the same `line_id` at one revision, and the
//! amount table's key must widen in the same change or this catch-all becomes
//! reachable as a `500`"*. It widens in the same change.
//!
//! # trigger body this migration moves
//!
//! This is the half that is not a key. The amount guard resolves its parent by
//! `(line_id, overlay_revision)` and nothing else, which was unambiguous while the
//! line key was globally unique and stops being so here. Left alone, an amount
//! under a **published** line would find another tenant's **draft** line carrying
//! the same `(line_id, overlay_revision)`, read `draft`, and be admitted — D-92's
//! freeze defeated across a tenant boundary by a widening meant to close a leak.
//!
//! All three `SQLite` bodies and the Postgres function therefore gain
//! `l.tenant_id = NEW.tenant_id` (`OLD.tenant_id` on the arms that ask where the
//! row comes from). Those three digests move in `tests/sqlite_migrations.rs` and
//! no others: the **line's** own triggers resolve their parent through
//! `price_overlay_id`, whose own key `(price_overlay_id, revision)` is rooted in a
//! server-minted id and is globally unique, so nothing about them is ambiguous and
//! they are carried over character for character.
//!
//! # `target_sku` is absent or names something
//!
//! `chk_pricing_price_overlay_line_target_sku_present` keeps its `NULL` arm — an
//! absent SKU is the list-default line and the per-plan line, which `LineKey` builds
//! with no SKU at all — and holds the present case to a trim against ASCII
//! whitespace entire (`pricing_region_taxonomy`'s set and its argument, D-242).
//!
//! `NULL` and a blank string are not the same state here and only one of them is a
//! line: `TargetSku` is a newtype whose constructor trims, minted at the REST door
//! (`api::rest::overlays`) and again by `overlay_repo`, which folds a refusal to
//! `RepoError::CorruptRow` — so a blank SKU fails the read of the revision it sits
//! in, while `NULL` reads back as the keyless line it is meant to be. The residue is
//! `pricing_region_taxonomy`'s: non-ASCII whitespace satisfies the predicate and
//! `TargetSku::new` still refuses it.
//!
//! # About this file
//!
//! Dependency level 1: everything it references is created before it.
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
    "CREATE TABLE bss.pricing_price_overlay_line (
            tenant_id        uuid        NOT NULL,
            overlay_revision bigint      NOT NULL,
            line_id          uuid        NOT NULL,
            adjustment_kind  text        NOT NULL,
            adjustment_value bigint,
            cohort           timestamptz,
            magnitude_kind   text        NOT NULL,
            plan_id          uuid,
            price_overlay_id uuid        NOT NULL,
            target_sku       text,
            CONSTRAINT chk_pricing_price_overlay_line_adjustment_kind CHECK (adjustment_kind IN ('markup', 'discount', 'fixed')),
            CONSTRAINT chk_pricing_price_overlay_line_cohort_needs_plan CHECK (cohort IS NULL OR plan_id IS NOT NULL),
            CONSTRAINT chk_pricing_price_overlay_line_discount_ceiling CHECK (adjustment_kind <> 'discount' OR adjustment_value IS NULL OR adjustment_value <= 10000),
            CONSTRAINT chk_pricing_price_overlay_line_fixed_is_amount CHECK (adjustment_kind <> 'fixed' OR magnitude_kind = 'amount'),
            CONSTRAINT chk_pricing_price_overlay_line_magnitude_kind CHECK (magnitude_kind IN ('percent_bp', 'amount')),
            CONSTRAINT chk_pricing_price_overlay_line_magnitude_pairing CHECK ((magnitude_kind = 'percent_bp') = (adjustment_value IS NOT NULL)),
            CONSTRAINT chk_pricing_price_overlay_line_magnitude_positive CHECK (adjustment_value IS NULL OR adjustment_value > 0),
            CONSTRAINT chk_pricing_price_overlay_line_plan_id_not_nil CHECK (plan_id IS NULL OR plan_id <> '00000000-0000-0000-0000-000000000000'),
            CONSTRAINT chk_pricing_price_overlay_line_sku_needs_plan CHECK (target_sku IS NULL OR plan_id IS NOT NULL),
            CONSTRAINT chk_pricing_price_overlay_line_target_sku_present CHECK (target_sku IS NULL OR length(btrim(target_sku, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0),
            CONSTRAINT fk_pricing_price_overlay_line_overlay FOREIGN KEY (price_overlay_id, overlay_revision) REFERENCES bss.pricing_price_overlay(price_overlay_id, revision),
            CONSTRAINT pricing_price_overlay_line_pkey PRIMARY KEY (tenant_id, overlay_revision, line_id)
        )",
    "CREATE INDEX idx_pricing_price_overlay_line_plan ON bss.pricing_price_overlay_line USING btree (tenant_id, plan_id)",
    "CREATE INDEX idx_pricing_price_overlay_line_revision ON bss.pricing_price_overlay_line USING btree (tenant_id, price_overlay_id, overlay_revision)",
    "CREATE UNIQUE INDEX uq_pricing_price_overlay_line_key ON bss.pricing_price_overlay_line USING btree (price_overlay_id, overlay_revision, COALESCE(plan_id, '00000000-0000-0000-0000-000000000000'::uuid), COALESCE(target_sku, ''::text), COALESCE(cohort, '-infinity'::timestamp with time zone))",
    "CREATE OR REPLACE FUNCTION bss.pricing_price_overlay_line_append_only() RETURNS trigger AS $$
        DECLARE
          parent_state  text;
          parent_tenant uuid;
        BEGIN
          IF TG_OP <> 'INSERT' THEN
            SELECT o.lifecycle_state INTO parent_state
              FROM bss.pricing_price_overlay o
             WHERE o.price_overlay_id = OLD.price_overlay_id
               AND o.revision = OLD.overlay_revision;
            IF parent_state IS DISTINCT FROM 'draft' THEN
              RAISE EXCEPTION
                'pricing_price_overlay_line: % of a line under a non-draft overlay revision is not permitted (state %)',
                TG_OP, coalesce(parent_state, 'missing');
            END IF;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RETURN OLD;
          END IF;

          SELECT o.lifecycle_state, o.tenant_id INTO parent_state, parent_tenant
            FROM bss.pricing_price_overlay o
           WHERE o.price_overlay_id = NEW.price_overlay_id
             AND o.revision = NEW.overlay_revision;
          IF parent_state IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION
              'pricing_price_overlay_line: % of a line under a non-draft overlay revision is not permitted (state %)',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          -- `fk_pricing_price_overlay_line_overlay` covers
          -- `(price_overlay_id, overlay_revision)` alone, so without this arm a line
          -- could carry a tenant its own parent overlay does not belong to:
          -- invisible to every scoped reader, and frozen with the revision it was
          -- written under. The state arm above has already refused a parent that
          -- does not exist, so a foreign tenant is the only thing left for this one
          -- to find.
          IF parent_tenant IS DISTINCT FROM NEW.tenant_id THEN
            RAISE EXCEPTION
              'pricing_price_overlay_line: overlay revision %/% belongs to another tenant and may not hold this line',
              NEW.price_overlay_id, NEW.overlay_revision;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_price_overlay_line_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_price_overlay_line FOR EACH ROW EXECUTE FUNCTION bss.pricing_price_overlay_line_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_price_overlay_line",
    "DROP FUNCTION IF EXISTS bss.pricing_price_overlay_line_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_price_overlay_line (
            tenant_id        text   NOT NULL,
            overlay_revision bigint NOT NULL,
            line_id          text   NOT NULL,
            adjustment_kind  text   NOT NULL,
            adjustment_value bigint,
            cohort           text,
            magnitude_kind   text   NOT NULL,
            plan_id          text,
            price_overlay_id text   NOT NULL,
            target_sku       text,
            PRIMARY KEY (tenant_id, overlay_revision, line_id),
            CONSTRAINT chk_pricing_price_overlay_line_adjustment_kind CHECK (adjustment_kind IN ('markup', 'discount', 'fixed')),
            CONSTRAINT chk_pricing_price_overlay_line_cohort_needs_plan CHECK (cohort IS NULL OR plan_id IS NOT NULL),
            CONSTRAINT chk_pricing_price_overlay_line_discount_ceiling CHECK (adjustment_kind <> 'discount' OR adjustment_value IS NULL OR adjustment_value <= 10000),
            CONSTRAINT chk_pricing_price_overlay_line_fixed_is_amount CHECK (adjustment_kind <> 'fixed' OR magnitude_kind = 'amount'),
            CONSTRAINT chk_pricing_price_overlay_line_magnitude_kind CHECK (magnitude_kind IN ('percent_bp', 'amount')),
            CONSTRAINT chk_pricing_price_overlay_line_magnitude_pairing CHECK ((magnitude_kind = 'percent_bp') = (adjustment_value IS NOT NULL)),
            CONSTRAINT chk_pricing_price_overlay_line_magnitude_positive CHECK (adjustment_value IS NULL OR adjustment_value > 0),
            CONSTRAINT chk_pricing_price_overlay_line_plan_id_not_nil CHECK (plan_id IS NULL OR plan_id <> '00000000-0000-0000-0000-000000000000'),
            CONSTRAINT chk_pricing_price_overlay_line_sku_needs_plan CHECK (target_sku IS NULL OR plan_id IS NOT NULL),
            CONSTRAINT chk_pricing_price_overlay_line_target_sku_present CHECK (target_sku IS NULL OR length(trim(target_sku, char(9,10,11,12,13,32))) > 0),
            CONSTRAINT fk_pricing_price_overlay_line_overlay FOREIGN KEY (price_overlay_id, overlay_revision) REFERENCES pricing_price_overlay(price_overlay_id, revision)
        )",
    "CREATE INDEX idx_pricing_price_overlay_line_plan ON pricing_price_overlay_line (tenant_id, plan_id)",
    "CREATE INDEX idx_pricing_price_overlay_line_revision ON pricing_price_overlay_line (tenant_id, price_overlay_id, overlay_revision)",
    "CREATE UNIQUE INDEX uq_pricing_price_overlay_line_key ON pricing_price_overlay_line (price_overlay_id, overlay_revision, COALESCE(plan_id, '00000000-0000-0000-0000-000000000000'), COALESCE(target_sku, ''), COALESCE(cohort, ''))",
    "CREATE TRIGGER trg_pricing_price_overlay_line_no_delete BEFORE DELETE ON pricing_price_overlay_line FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line: DELETE of a line under a non-draft overlay revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = OLD.price_overlay_id AND o.revision = OLD.overlay_revision AND o.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_line_no_insert BEFORE INSERT ON pricing_price_overlay_line FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line: INSERT of a line under a non-draft overlay revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = NEW.price_overlay_id AND o.revision = NEW.overlay_revision AND o.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_line_no_update BEFORE UPDATE ON pricing_price_overlay_line FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line: UPDATE of a line under a non-draft overlay revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = OLD.price_overlay_id AND o.revision = OLD.overlay_revision AND o.lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = NEW.price_overlay_id AND o.revision = NEW.overlay_revision AND o.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_price_overlay_line_same_tenant_as_its_revision_on_insert BEFORE INSERT ON pricing_price_overlay_line FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line: the overlay revision belongs to another tenant and may not hold this line') WHERE EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = NEW.price_overlay_id AND o.revision = NEW.overlay_revision) AND NOT EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = NEW.price_overlay_id AND o.revision = NEW.overlay_revision AND o.tenant_id = NEW.tenant_id); END",
    "CREATE TRIGGER trg_pricing_price_overlay_line_same_tenant_as_its_revision_on_update BEFORE UPDATE ON pricing_price_overlay_line FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_price_overlay_line: the overlay revision belongs to another tenant and may not hold this line') WHERE EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = NEW.price_overlay_id AND o.revision = NEW.overlay_revision) AND NOT EXISTS (SELECT 1 FROM pricing_price_overlay o WHERE o.price_overlay_id = NEW.price_overlay_id AND o.revision = NEW.overlay_revision AND o.tenant_id = NEW.tenant_id); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_price_overlay_line"];

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
