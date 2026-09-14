//! Create `bss.pricing_migration` — one scheduled plan migration
//! (`design/11-lifecycle.md` §6, `cpt-cf-bss-pricing-state-migration`).
//!
//! One row is one migration: a source plan revision, a published target, an
//! effective date, a scope, and the §4 state machine that carries it from
//! `scheduled` to a terminal state. The catalog never mutates a subscription and
//! never touches a posted invoice (M1) — this row is a **schedule**, and
//! Subscriptions is what executes against it.
//!
//! # `migration_id` is client-supplied, and that is the whole of M2
//!
//! `inst-ms-api` makes the create idempotent on a **client-supplied**
//! `migration_id`, mirroring Slice 12's `run_id`: a timed-out client retry
//! returns the original schedule, never a second one. So the primary key is not a
//! minted surrogate, and the idempotency is the key rather than a dedup table
//! beside it. A second `POST` carrying a `migration_id` that already exists is
//! answered from the stored row; there is no path by which two rows can describe
//! one migration, because the store will not hold two.
//!
//! # The §4 state machine lives in the constraints, not only in the domain
//!
//! `pricing_price_window`'s reason, one table over: a rule that lives only in
//! application code is one ad-hoc `UPDATE` away from being bypassed, and what it
//! would bypass here is whether an executor that has already re-bound a thousand
//! subscriptions is allowed to be told the run was never real. Four edges, and
//! nothing else:
//!
//! ```text
//! scheduled   -> in_progress   (`inst-mst-start`, and only via POST .../start)
//! scheduled   -> cancelled     (`inst-mst-cancel`, M3 — nothing executed yet)
//! in_progress -> cancelled     (`inst-mst-cancel-inflight`, D-34)
//! in_progress -> completed     (`inst-mst-complete`)
//! ```
//!
//! **`completed` is terminal and uncancellable, and that is D-34's own sentence
//! made physical.** There is no `completed -> cancelled` edge, so the 409 the
//! route answers (`MIGRATION_COMPLETED`) is guarded at two layers rather than
//! one. `in_progress -> cancelled` **is** an edge, which is the part of D-34 that
//! changed on 2026-08-07: the stop-the-bleeding control halts further `PlanLink`
//! processing, and already-migrated subscriptions are unaffected because the
//! catalog never held their state to begin with.
//!
//! # `DELETE` is refused, and the REST `DELETE` is not a deletion
//!
//! `DELETE /bss-pricing/v1/migrations/{id}` **cancels**; it does not remove the
//! row. The trigger refuses the statement outright, in `pricing_price_window`'s
//! words ("cancel is a state, not a deletion") and for a sharper reason: the
//! schedule is the record an executor re-reads before each batch
//! (`inst-mg-cancel`'s state handshake), so a deleted row would read as "no such
//! migration" to a party whose correct behaviour is to **stop**. Absence and
//! cancellation must not be the same answer on that lane.
//!
//! # The flip timestamps, and the one biconditional that is deliberately not one
//!
//! `completed_at` and `cancelled_at` are plain biconditionals against their
//! states, in `chk_pricing_approval_decided_at`'s idiom. `started_at` is **not**,
//! and the asymmetry is a fact about §4 rather than an omission: `cancelled` is
//! reachable from *both* `scheduled` (never started) and `in_progress` (started),
//! so a biconditional would have to refuse one of D-34's two cancel edges. It is
//! written as the two implications that are actually true —
//! `in_progress`/`completed` **require** a start, and `scheduled` **forbids** one
//! — leaving `cancelled` admitting either, which is exactly the reachable set.
//!
//! # `exclusion_snapshot` is co-nullable with `started_at`, and that is D-65
//!
//! D-65 (sharpened 2026-07-31) makes `POST .../start` **persist-and-replay**: the
//! exclusion set is computed once at the first call, persisted, and repeat calls
//! return that stored snapshot verbatim without re-transitioning and without
//! re-running the D-36 re-validation, because a recompute could differ from the
//! set the executor already honoured. The biconditional
//! `(started_at IS NOT NULL) = (exclusion_snapshot IS NOT NULL)` makes the
//! "persist" half physical: a row cannot be `in_progress` without the snapshot
//! its executor was handed, so there is no state in which a replay has nothing to
//! replay.
//!
//! # `delta_report` is frozen and `exclusion_snapshot` is not, on purpose
//!
//! §6 calls `delta_report` the deltas "**at schedule time**", so it is in the
//! frozen-column whitelist: it is evidence of what the operator confirmed
//! against, and an operator who approved a schedule against one report must not
//! find a different one on the row afterwards. D-36's execution-time
//! re-resolution deliberately lands in a **different** column, so "what was known
//! when this was scheduled" and "what was true when it ran" are two facts the row
//! holds separately rather than one the second overwrites.
//!
//! # `announced_at`, and the half of D-49 a row-local `CHECK` can carry
//!
//! D-49 validates `effective_at >= announcement + the tenant's configured notice
//! period`, floor 60 days, and the notice value lives in
//! `pricing_policy_object.enforced_migration_notice_days` — **another table**, so
//! no `CHECK` here can state the rule. What is row-local is its weakest half,
//! `announced_at <= effective_at`, and that is the one written: whatever the
//! configured notice was, the row cannot record a migration that took effect
//! before it was announced. `pricing_price_window`'s split, verbatim in shape — the
//! durable half is here and the configured half is
//! [`domain::migration`](crate::domain::migration)'s, which is also where the
//! `MIGRATION_NOTICE_TOO_SHORT` refusal is produced.
//!
//! # No foreign keys, for `pricing_bundle`'s reason
//!
//! `source_plan_id` and `target_plan_id` name plans, and `pricing_plan` is keyed
//! `(plan_id, revision)` (D-56) with uniqueness on `plan_id` alone only in two
//! **partial** indexes. Postgres refuses a partial index as a foreign key's
//! referent, so neither reference is expressible, exactly as `pricing_bundle`
//! found. The references are enforced one layer up instead: scheduling resolves
//! both plans through `plan_repo::load_current` and refuses a target that is not
//! published (`MIGRATION_TARGET_INVALID`), which is a stronger predicate than a
//! foreign key could state anyway — an FK would admit a `draft` target.
//!
//! # An addition §6 does not state: `source_plan_id <> target_plan_id`
//!
//! Reported as an addition rather than presented as transcription. §6 lists the
//! two columns and says nothing about their relationship, but a migration from a
//! plan to itself is not a degenerate case with a sensible answer: it would emit
//! `PlanMigrationScheduled` asking Subscriptions to create `PlanLink`s onto the
//! plan every subscriber is already on. It is refused here because the refusal is
//! total and row-local; nothing in the design set contradicts it.
//!
//! **Backend differences.** The systematic mirror of this chain: `bss.` dropped,
//! `uuid` -> `text`, `timestamptz` -> `text`, `jsonb` -> `text`, `now()` ->
//! the RFC 3339 `strftime` its writers spell, and the single PL/pgSQL trigger split into four
//! `RAISE(ABORT, ...)` triggers, since `SQLite` has no procedural language, no
//! `BEFORE UPDATE OR DELETE` and no message interpolation. Each arm below the
//! terminal one repeats that arm's exclusion in its `WHEN`, so exactly one fires
//! on any one statement, in the Postgres order. Every `CHECK`, index and the
//! primary key are preserved name for name. The Postgres `down` drops the
//! function as well as the table; the `SQLite` one drops only the table.
//!
//! `pricing_migration`'s primary key gains `tenant_id`.
//!
//! `migration_id` is **client-supplied** — `pricing_migration`'s own module doc
//! opens with "`migration_id` is client-supplied, and that is the whole of M2" —
//! and it arrives straight off the request body. Until this migration it was also
//! the table's entire `PRIMARY KEY`, so the namespace of a client-chosen
//! identifier was the whole deployment rather than the tenant.
//!
//! # Two defects in one column, and the second has no remedy
//!
//! `migration_repo::insert_or_load` is `ON CONFLICT DO NOTHING`, then a
//! tenant-filtered `load`, then `ok_or_else(ConcurrentMutation)`. With a global
//! conflict target:
//!
//! - **An existence oracle on a live authenticated route.** Tenant B posting a
//!   `migration_id` tenant A already holds takes the `DO NOTHING` branch, so the
//!   `load` finds nothing under B and B is answered `CONCURRENT_MUTATION` (409);
//!   an unused id is answered 202. That difference is observable and is a fact
//!   about another tenant's rows.
//! - **A permanent cross-tenant denial.** The refusal says *retry*, and a retry
//!   collides identically, forever. Any tenant could reserve arbitrary migration
//!   ids against every other tenant, and the victim has no remedy at all:
//!   `trg_pricing_migration_no_delete` refuses the `DELETE` that would free it.
//!
//! It is filed High rather than Critical because no other tenant's *content*
//! becomes readable and nothing about financial integrity breaks; exploiting the
//! oracle also requires knowing or predicting the uuid, so with random ids it
//! amplifies an identifier leak rather than standing on its own.
//!
//! # Every sibling already did this
//!
//! `pricing_migration` was the last client-key store in the crate with a global
//! uniqueness namespace. `synthesis_repo` scopes its conflict target to
//! `(tenant_id, subscription_ref)` — with a doc block reasoning about *which*
//! target is right — `idempotency_repo` to `(tenant_id, operation, client_key)`,
//! and `bulk_repo`'s moved to `(tenant_id, kind, client_key)` under D-307, which
//! `pricing_bulk_operation`'s client key already carries. So this is the same correction
//! applied to the one table that was missed, not a new position.
//!
//! # No separate `(tenant_id, migration_id)` index
//!
//! That pair is exactly the primary key on both engines, so a second index over it
//! would be maintained on every write to serve reads the key already serves. Neither
//! engine's roster names one and neither golden holds one.
//!
//! `pricing_migration.source_revision` and
//! `pricing_snapshot_provenance.source_revision` become `bigint` — review finding
//! **Z6-7**, the last two `integer` revision columns in the chain.
//!
//! # The outlier
//!
//! A plan revision is a `u64` everywhere it is a value: `pricing_plan.revision`
//! is `bigint` (`pricing_plan`), and so is every column in the chain that
//! carries one — `subject_revision` on the ref tables, `plan_revision` on the
//! eleven revision-scoped children, `source_revision` on the outbox payload.
//! These two were `integer`, i.e. addressable up to 2^31-1 where the value's own
//! type reaches 2^64-1.
//!
//! It was **guarded rather than broken**, and that is why the review filed it Low:
//! `migration_repo::insert_or_load` and `synthesis_repo::freeze_or_load` each
//! narrowed with an `i32::try_from` and answered `CorruptRow` on failure, so the
//! consequence was a fail-closed refusal and never a truncated revision. The
//! narrowing is what goes away with the column — both sites now guard on
//! `i64::try_from`, which is the column's own range rather than a third one.
//!
//! # Postgres does the work; `SQLite` has nothing to do, and that is not laziness
//!
//! On Postgres `integer` and `bigint` are different types with different widths,
//! and `ALTER COLUMN … TYPE bigint` is the whole change: the widening is
//! unconditionally safe (every `int4` is an `int8`), it rewrites the table but
//! validates nothing, and no CHECK, index or trigger on either table mentions the
//! column in a way a type change invalidates —
//! `chk_pricing_migration_source_revision` (`>= 0`) and
//! `chk_pricing_snapshot_provenance_revision` (`IS NULL OR >= 0`) hold over the
//! wider type unchanged, and any index over the column is rebuilt by the `ALTER`
//! itself.
//!
//! **This paragraph named `idx_pricing_migration_source` as that index until
//! 2026-08-18, and on this engine there was no such index to rebuild** (review
//! Z26-1): it was declared in `pricing_migration`'s `SQLite` arm only, and
//! the index below is what carries it on Postgres. The claim above is
//! restated over "any index" because it is a property of `ALTER COLUMN … TYPE`,
//! which is what made it safe to assert — naming a specific index turned a fact
//! about the engine into an unchecked fact about this schema, and the schema was
//! the half that was wrong. The column this migration widens
//! (`source_revision`) is in neither index in any case; both key
//! `(tenant_id, source_plan_id)`.
//!
//! On `SQLite` the two spellings **are the same type**. Affinity is assigned by
//! substring: any declared type containing `INT` takes INTEGER affinity, and a
//! `SQLite` integer is stored in up to 8 bytes regardless of what the column was
//! declared as. So `integer` there already addressed the whole `u64` range that
//! fits in an `i64`, the narrowing never existed on that engine, and the only thing
//! a `SQLite` arm could change is the word in `sqlite_master`. That word would cost
//! a full table rebuild — `SQLite` cannot `ALTER COLUMN` — restating
//! `pricing_migration`'s three indexes and five triggers, which is real risk bought
//! for a cosmetic diff. The arm is therefore **empty on purpose**, and the claim is
//! measured rather than asserted:
//! `sqlite_migration_repo::a_revision_beyond_the_old_columns_range_round_trips` and
//! `sqlite_snapshot_provenance_store::a_revision_beyond_the_old_columns_range_round_trips`
//! write a revision above `i32::MAX` through the repositories and read it back, and
//! both were RED before this migration because the `i32::try_from` above refused
//! them.
//!
//! **This is the one place in the chain where an empty arm is right**, so it is
//! worth stating why the usual rule does not apply: elsewhere a missing `SQLite`
//! statement means the mirror drifts from the canonical schema and the fast tier
//! stops measuring what production runs. Here the mirror does not drift — the two
//! declarations denote one type on that engine — and what the fast tier measures is
//! the behaviour, which is identical before and after. The Postgres half of the
//! same claim is `postgres_migrations::every_revision_column_is_bigint`, which reads
//! `information_schema` over the whole schema rather than trusting this file — and
//! which is stated over every `…_revision` column rather than over these two,
//! because a spot check on the known outliers would be green against the next one.
//!
//! # Why a new migration and not a correction to those two arms
//!
//! Because a correction there fixes only databases that have never been migrated.
//! `pricing_migration` and `_000044` are long applied on every stand, and
//! `sea_orm_migration` runs each name once: editing their `PG_UP_STATEMENTS` would
//! make a fresh chain correct and leave every existing Postgres database exactly as
//! it is now. The rule this follows is the chain's own, stated for
//! guards — *the chain moves forward; a past migration's text is history* — and it
//! is why the statements below are plain `CREATE INDEX` rather than
//! `IF NOT EXISTS`: on any database this migration can reach, neither index
//! exists, so a collision would be a fact worth failing on rather than skipping.
//!
//! # What the indexes are for
//!
//! Both are the reverse-lookup half of a pair whose forward half exists on both
//! engines. `pricing_migration` is keyed and indexed by `target_plan_id`, and a
//! plan being retired or superseded has to answer *"what migrates away from me"* —
//! `(tenant_id, source_plan_id)`. `pricing_snapshot_provenance` is unique per
//! `(tenant_id, subscription_ref)`, and the migrated-origin read answers *"which
//! subscriptions came off this legacy plan"* — `(tenant_id, source_plan_id)`. On a
//! table that is append-only over a >= 7-year retention, the missing index is a
//! sequential scan that grows with the retention rather than with the tenant, which
//! is why this is repaired rather than recorded.
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
    "CREATE TABLE bss.pricing_migration (
            tenant_id          uuid        NOT NULL,
            migration_id       uuid        NOT NULL,
            announced_at       timestamptz NOT NULL,
            cancelled_at       timestamptz,
            completed_at       timestamptz,
            completion_record  jsonb,
            delta_report       jsonb       NOT NULL,
            effective_at       timestamptz NOT NULL,
            exclusion_snapshot jsonb,
            scope              jsonb       NOT NULL,
            source_plan_id     uuid        NOT NULL,
            source_revision    bigint      NOT NULL,
            started_at         timestamptz,
            state              text        NOT NULL,
            target_plan_id     uuid        NOT NULL,
            created_at         timestamptz NOT NULL DEFAULT now(),
            created_by         uuid        NOT NULL,
            CONSTRAINT chk_pricing_migration_announced_before_effective CHECK (announced_at <= effective_at),
            CONSTRAINT chk_pricing_migration_cancelled_at CHECK ((state = 'cancelled') = (cancelled_at IS NOT NULL)),
            CONSTRAINT chk_pricing_migration_cancelled_order CHECK (cancelled_at IS NULL OR cancelled_at >= created_at),
            CONSTRAINT chk_pricing_migration_completed_at CHECK ((state = 'completed') = (completed_at IS NOT NULL)),
            CONSTRAINT chk_pricing_migration_completed_order CHECK (completed_at IS NULL OR completed_at >= started_at),
            CONSTRAINT chk_pricing_migration_distinct_plans CHECK (source_plan_id <> target_plan_id),
            CONSTRAINT chk_pricing_migration_exclusion_snapshot CHECK ((started_at IS NOT NULL) = (exclusion_snapshot IS NOT NULL)),
            CONSTRAINT chk_pricing_migration_scheduled_unstarted CHECK (state <> 'scheduled' OR started_at IS NULL),
            CONSTRAINT chk_pricing_migration_source_revision CHECK (source_revision >= 0),
            CONSTRAINT chk_pricing_migration_started_order CHECK (started_at IS NULL OR started_at >= created_at),
            CONSTRAINT chk_pricing_migration_started_required CHECK (state NOT IN ('in_progress','completed') OR started_at IS NOT NULL),
            CONSTRAINT chk_pricing_migration_state CHECK (state IN ('scheduled','in_progress','completed','cancelled')),
            CONSTRAINT pricing_migration_pkey PRIMARY KEY (tenant_id, migration_id)
        )",
    "CREATE INDEX idx_pricing_migration_due ON bss.pricing_migration USING btree (state, effective_at)",
    "CREATE INDEX idx_pricing_migration_source ON bss.pricing_migration USING btree (tenant_id, source_plan_id)",
    "CREATE INDEX idx_pricing_migration_target ON bss.pricing_migration USING btree (tenant_id, target_plan_id)",
    "CREATE OR REPLACE FUNCTION bss.pricing_migration_append_only() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION
              'pricing_migration: DELETE of migration % is not permitted; cancel is a state, not a deletion, and an executor must be able to tell a cancelled run from an absent one',
              OLD.migration_id;
          END IF;

          IF OLD.state IN ('completed','cancelled') THEN
            RAISE EXCEPTION
              'pricing_migration: migration % is %; a completed or cancelled run is immutable history',
              OLD.migration_id, OLD.state;
          END IF;

          IF NEW.migration_id    IS DISTINCT FROM OLD.migration_id
          OR NEW.tenant_id       IS DISTINCT FROM OLD.tenant_id
          OR NEW.source_plan_id  IS DISTINCT FROM OLD.source_plan_id
          OR NEW.source_revision IS DISTINCT FROM OLD.source_revision
          OR NEW.target_plan_id  IS DISTINCT FROM OLD.target_plan_id
          OR NEW.effective_at    IS DISTINCT FROM OLD.effective_at
          OR NEW.announced_at    IS DISTINCT FROM OLD.announced_at
          OR NEW.scope           IS DISTINCT FROM OLD.scope
          OR NEW.delta_report    IS DISTINCT FROM OLD.delta_report
          OR NEW.created_by      IS DISTINCT FROM OLD.created_by
          OR NEW.created_at      IS DISTINCT FROM OLD.created_at THEN
            RAISE EXCEPTION
              'pricing_migration: migration % is bound to its source, target, effective date, scope and schedule-time delta report; only state, the execution records and the flip timestamps may move',
              OLD.migration_id;
          END IF;

          IF NEW.state IS DISTINCT FROM OLD.state
             AND NOT (OLD.state = 'scheduled'   AND NEW.state IN ('in_progress','cancelled'))
             AND NOT (OLD.state = 'in_progress' AND NEW.state IN ('completed','cancelled')) THEN
            RAISE EXCEPTION
              'pricing_migration: state % -> % is not a sanctioned transition',
              OLD.state, NEW.state;
          END IF;

          -- D-65's replay half: once persisted, the exclusion set an executor was
          -- handed is what every repeat call must be answered with.
          IF OLD.exclusion_snapshot IS NOT NULL
             AND NEW.exclusion_snapshot IS DISTINCT FROM OLD.exclusion_snapshot THEN
            RAISE EXCEPTION
              'pricing_migration: the exclusion set of migration % is computed once and replayed verbatim; a recompute could differ from the set the executor already honoured',
              OLD.migration_id;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_migration_append_only BEFORE DELETE OR UPDATE ON bss.pricing_migration FOR EACH ROW EXECUTE FUNCTION bss.pricing_migration_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_migration",
    "DROP FUNCTION IF EXISTS bss.pricing_migration_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_migration (
            tenant_id          text    NOT NULL,
            migration_id       text    NOT NULL,
            announced_at       text    NOT NULL,
            cancelled_at       text,
            completed_at       text,
            completion_record  text,
            delta_report       text    NOT NULL,
            effective_at       text    NOT NULL,
            exclusion_snapshot text,
            scope              text    NOT NULL,
            source_plan_id     text    NOT NULL,
            source_revision    integer NOT NULL,
            started_at         text,
            state              text    NOT NULL,
            target_plan_id     text    NOT NULL,
            created_at         text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            created_by         text    NOT NULL,
            PRIMARY KEY (tenant_id, migration_id),
            CONSTRAINT chk_pricing_migration_announced_before_effective CHECK (announced_at <= effective_at),
            CONSTRAINT chk_pricing_migration_cancelled_at CHECK ((state = 'cancelled') = (cancelled_at IS NOT NULL)),
            CONSTRAINT chk_pricing_migration_cancelled_order CHECK (cancelled_at IS NULL OR cancelled_at >= created_at),
            CONSTRAINT chk_pricing_migration_completed_at CHECK ((state = 'completed') = (completed_at IS NOT NULL)),
            CONSTRAINT chk_pricing_migration_completed_order CHECK (completed_at IS NULL OR completed_at >= started_at),
            CONSTRAINT chk_pricing_migration_distinct_plans CHECK (source_plan_id <> target_plan_id),
            CONSTRAINT chk_pricing_migration_exclusion_snapshot CHECK ((started_at IS NOT NULL) = (exclusion_snapshot IS NOT NULL)),
            CONSTRAINT chk_pricing_migration_scheduled_unstarted CHECK (state <> 'scheduled' OR started_at IS NULL),
            CONSTRAINT chk_pricing_migration_source_revision CHECK (source_revision >= 0),
            CONSTRAINT chk_pricing_migration_started_order CHECK (started_at IS NULL OR started_at >= created_at),
            CONSTRAINT chk_pricing_migration_started_required CHECK (state NOT IN ('in_progress','completed') OR started_at IS NOT NULL),
            CONSTRAINT chk_pricing_migration_state CHECK (state IN ('scheduled','in_progress','completed','cancelled'))
        )",
    "CREATE INDEX idx_pricing_migration_due ON pricing_migration (state, effective_at)",
    "CREATE INDEX idx_pricing_migration_source ON pricing_migration (tenant_id, source_plan_id)",
    "CREATE INDEX idx_pricing_migration_target ON pricing_migration (tenant_id, target_plan_id)",
    "CREATE TRIGGER trg_pricing_migration_exclusion_replay BEFORE UPDATE ON pricing_migration FOR EACH ROW WHEN OLD.state NOT IN ('completed','cancelled') AND OLD.exclusion_snapshot IS NOT NULL AND NEW.exclusion_snapshot IS NOT OLD.exclusion_snapshot BEGIN SELECT RAISE(ABORT, 'pricing_migration: the exclusion set is computed once and replayed verbatim; a recompute could differ from the set the executor already honoured'); END",
    "CREATE TRIGGER trg_pricing_migration_flip_whitelist BEFORE UPDATE ON pricing_migration FOR EACH ROW WHEN OLD.state NOT IN ('completed','cancelled') AND NEW.state IS NOT OLD.state AND NOT (OLD.state = 'scheduled' AND NEW.state IN ('in_progress','cancelled')) AND NOT (OLD.state = 'in_progress' AND NEW.state IN ('completed','cancelled')) BEGIN SELECT RAISE(ABORT, 'pricing_migration: state transition is not a sanctioned one'); END",
    "CREATE TRIGGER trg_pricing_migration_frozen_columns BEFORE UPDATE ON pricing_migration FOR EACH ROW WHEN OLD.state NOT IN ('completed','cancelled') AND (NEW.migration_id IS NOT OLD.migration_id OR NEW.tenant_id IS NOT OLD.tenant_id OR NEW.source_plan_id IS NOT OLD.source_plan_id OR NEW.source_revision IS NOT OLD.source_revision OR NEW.target_plan_id IS NOT OLD.target_plan_id OR NEW.effective_at IS NOT OLD.effective_at OR NEW.announced_at IS NOT OLD.announced_at OR NEW.scope IS NOT OLD.scope OR NEW.delta_report IS NOT OLD.delta_report OR NEW.created_by IS NOT OLD.created_by OR NEW.created_at IS NOT OLD.created_at) BEGIN SELECT RAISE(ABORT, 'pricing_migration: the migration is bound to its source, target, effective date, scope and schedule-time delta report; only state, the execution records and the flip timestamps may move'); END",
    "CREATE TRIGGER trg_pricing_migration_immutable_history BEFORE UPDATE ON pricing_migration FOR EACH ROW WHEN OLD.state IN ('completed','cancelled') BEGIN SELECT RAISE(ABORT, 'pricing_migration: a completed or cancelled run is immutable history'); END",
    "CREATE TRIGGER trg_pricing_migration_no_delete BEFORE DELETE ON pricing_migration FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_migration: DELETE of a migration is not permitted; cancel is a state, not a deletion, and an executor must be able to tell a cancelled run from an absent one'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_migration"];

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
