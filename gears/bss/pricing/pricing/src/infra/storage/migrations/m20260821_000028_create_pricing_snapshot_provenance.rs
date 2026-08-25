//! Create `bss.pricing_snapshot_provenance` — the `migrated-origin` record
//! (`design/11-lifecycle.md` §6, `inst-sy-provenance`, `inst-sy-payload`, D-76,
//! D-81, D-87, D-102).
//!
//! One row is one **frozen** snapshot for one subscription that never had a
//! `pricingSnapshotRef`: what was resolved, as of which instant, from which
//! selection tier, and the complete evaluable payload rating charges from and
//! Billing posts from.
//!
//! # This table is the marking, and there is no `origin` column
//!
//! §6 calls the record "marked **`migrated-origin`**". A column holding one
//! permitted value is not a marking, it is a tautology with a maintenance cost:
//! nothing else is ever stored here, so membership *is* the mark. The name says
//! so and `inst-sy-surface`'s endpoint is spelled after it.
//!
//! # Append-only in the strongest sense: no `UPDATE` at all
//!
//! Every other guarded table in this chain permits a whitelist of moves. This one
//! permits none, and the reason is the word §3 uses: the snapshot is **frozen**.
//! A `migrated-origin` ref resolves through **no** `CatalogVersion` by
//! construction (D-87, Foundation §4.4 names it the one deliberately
//! non-version-pinned reference), so the immutability that a frozen
//! `CatalogVersion` gives every other consumer contract has to come from
//! somewhere else — and the only place left is this row. If it could be edited,
//! a disputed legacy charge could be re-explained after the fact by the party
//! being disputed with.
//!
//! `DELETE` is refused for the same reason, one step further: an auditor
//! reconstructing a charge needs the record to still exist, and a subscription's
//! snapshot outlives the migration that synthesized it.
//!
//! # Idempotency is the unique index, and D-81 is why it is keyed this way
//!
//! §9 requires a second synthesis attempt to be idempotent — *the same frozen
//! ref*. `uq_pricing_snapshot_provenance_subscription` over
//! `(tenant_id, subscription_ref)` **is** that rule: a subscription has at most
//! one `migrated-origin` snapshot, ever, so a re-run finds the row rather than
//! freezing a second one at a second instant. Keying it on
//! `(subscription_ref, trigger)` instead would have let the `migration` and
//! `first-rating` triggers each freeze their own — and D-81 gives those two
//! *different* instants `t`, so the subscription would have two different frozen
//! prices with no rule saying which one rating reads.
//!
//! # `source_revision` is nullable, and that nullability is D-76's tier 2
//!
//! A fully-legacy key may have **no plan revision at all**: tier 2 resolves a
//! `pricing_historical_price` reference row that exists in no `CatalogVersion` by
//! construction, and D-87 states the case plainly — "a tier-2 (fully legacy) key
//! may have no plan revision at all". So the column admits `NULL` and the
//! payload, not the revision, is what makes the row evaluable. `source_plan_id`
//! stays `NOT NULL` because synthesis is always *about* a plan even when that
//! plan has no revision covering `t`.
//!
//! # What is deliberately **not** constrained
//!
//! **No relationship between `snapshot_instant` and `created_at`.** The obvious
//! rule — an instant frozen at execution cannot be in the future — is not written
//! because it is not true of both triggers: D-81 makes `t` the *migration
//! effective timestamp* for the `migration` trigger, and a migration is
//! synthesized in the run-up to a date that has not arrived. Writing the
//! plausible constraint would refuse the ordinary case of the more common
//! trigger. The `first-rating` half (`t` = earliest unrated usage) is genuinely
//! past, but a `CHECK` cannot be conditional on a fact this row states about
//! itself without becoming two rules that disagree at the boundary.
//!
//! **No foreign key on `source_plan_id`**, for the reason `pricing_bundle`'s doc
//! records:
//! `pricing_plan` is keyed `(plan_id, revision)` and its uniqueness on `plan_id`
//! alone lives in two *partial* indexes, which Postgres refuses as an FK
//! referent. And none on `subscription_ref` at all — subscriptions are another
//! gear's, which is the whole premise of this slice.
//!
//! **Backend differences.** The systematic mirror of this chain: `bss.` dropped,
//! `uuid` -> `text`, `timestamptz` -> `text`, `jsonb` -> `text`, `now()` ->
//! the RFC 3339 `strftime` its writers spell, and the single PL/pgSQL trigger split into two
//! `RAISE(ABORT, ...)` triggers, since `SQLite` has no procedural language and no
//! `BEFORE UPDATE OR DELETE`. Both arms are unconditional here, so no arm needs
//! to repeat another's exclusion — this is the one guarded table in the chain
//! whose mirror is a straight transliteration. Every `CHECK`, both indexes and the
//! primary key are preserved name for name.
//!
//! # `source_revision` is `bigint`, because a plan revision is a `u64`
//!
//! Every column in this chain that carries a plan revision is 64-bit —
//! `pricing_plan.revision`, `subject_revision` on the ref tables, `plan_revision` on
//! the revision-scoped children — and this one is no exception. `integer` would be
//! addressable to 2^31-1 where the value's own type reaches 2^64-1. Review finding
//! **Z6-7** found it the outlier; it was guarded rather than broken, because
//! `synthesis_repo::freeze_or_load` narrowed with `i32::try_from` and answered
//! `CorruptRow`, so the consequence was a fail-closed refusal and never a truncated
//! revision. That narrowing is gone: the guard is `i64::try_from`, which is the
//! column's own range rather than a third one.
//!
//! On `SQLite` the column reads `integer`, which is that engine's variable-width
//! integer up to eight bytes — the same range, spelled the way `SQLite` spells it.
//!
//! # What `idx_pricing_snapshot_provenance_plan` is for
//!
//! It is the reverse-lookup half of a pair. The table is unique per
//! `(tenant_id, subscription_ref)` — the forward direction — and the migrated-origin
//! read answers the other one: *"which subscriptions came off this legacy plan"*,
//! `(tenant_id, source_plan_id)`. On a table that is append-only over a >= 7-year
//! retention, the missing index is a sequential scan that grows with the retention
//! rather than with the tenant.
//!
//! It reached the Postgres server late, and not by anyone reading the migrations:
//! `postgres_migrations`' index census compares the server's roster against
//! `EXPECTED_INDEXES` and reported 50 against 52. One roster **per engine** is what
//! made that visible — a shared list would have been satisfied by the `SQLite` half
//! while the engine that ships was missing it.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_snapshot_provenance (
            tenant_id        uuid        NOT NULL,
            provenance_id    uuid        NOT NULL,
            acting_principal uuid        NOT NULL,
            payload          jsonb       NOT NULL,
            resolved         jsonb       NOT NULL,
            snapshot_instant timestamptz NOT NULL,
            source_plan_id   uuid        NOT NULL,
            source_revision  bigint,
            subscription_ref uuid        NOT NULL,
            trigger_kind     text        NOT NULL,
            created_at       timestamptz NOT NULL DEFAULT now(),
            CONSTRAINT chk_pricing_snapshot_provenance_payload CHECK ((jsonb_typeof(payload) = 'object'::text)),
            CONSTRAINT chk_pricing_snapshot_provenance_resolved CHECK (((jsonb_typeof(resolved) = 'array'::text) AND (jsonb_array_length(resolved) > 0))),
            CONSTRAINT chk_pricing_snapshot_provenance_revision CHECK (source_revision IS NULL OR source_revision >= 0),
            CONSTRAINT chk_pricing_snapshot_provenance_trigger CHECK (trigger_kind IN ('migration', 'first_rating')),
            CONSTRAINT pricing_snapshot_provenance_pkey PRIMARY KEY (provenance_id)
        )",
    "CREATE INDEX idx_pricing_snapshot_provenance_plan ON bss.pricing_snapshot_provenance USING btree (tenant_id, source_plan_id)",
    "CREATE UNIQUE INDEX uq_pricing_snapshot_provenance_subscription ON bss.pricing_snapshot_provenance USING btree (tenant_id, subscription_ref)",
    "CREATE OR REPLACE FUNCTION bss.pricing_snapshot_provenance_frozen() RETURNS trigger AS $$
        BEGIN
          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION
              'pricing_snapshot_provenance: DELETE of the migrated-origin record of subscription % is not permitted; an auditor reconstructing a legacy charge needs it to still exist',
              OLD.subscription_ref;
          END IF;

          RAISE EXCEPTION
            'pricing_snapshot_provenance: the migrated-origin snapshot of subscription % is frozen; it resolves through no CatalogVersion, so this row is the only thing making it immutable',
            OLD.subscription_ref;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_snapshot_provenance_frozen BEFORE DELETE OR UPDATE ON bss.pricing_snapshot_provenance FOR EACH ROW EXECUTE FUNCTION bss.pricing_snapshot_provenance_frozen()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_snapshot_provenance",
    "DROP FUNCTION IF EXISTS bss.pricing_snapshot_provenance_frozen()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_snapshot_provenance (
            tenant_id        text    NOT NULL,
            provenance_id    text    NOT NULL,
            acting_principal text    NOT NULL,
            payload          text    NOT NULL,
            resolved         text    NOT NULL,
            snapshot_instant text    NOT NULL,
            source_plan_id   text    NOT NULL,
            source_revision  integer,
            subscription_ref text    NOT NULL,
            trigger_kind     text    NOT NULL,
            created_at       text    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            PRIMARY KEY (provenance_id),
            CONSTRAINT chk_pricing_snapshot_provenance_payload CHECK (json_valid(payload) AND json_type(payload) = 'object'),
            CONSTRAINT chk_pricing_snapshot_provenance_resolved CHECK (json_valid(resolved) AND json_type(resolved) = 'array' AND json_array_length(resolved) > 0),
            CONSTRAINT chk_pricing_snapshot_provenance_revision CHECK (source_revision IS NULL OR source_revision >= 0),
            CONSTRAINT chk_pricing_snapshot_provenance_trigger CHECK (trigger_kind IN ('migration', 'first_rating'))
        )",
    "CREATE INDEX idx_pricing_snapshot_provenance_plan ON pricing_snapshot_provenance (tenant_id, source_plan_id)",
    "CREATE UNIQUE INDEX uq_pricing_snapshot_provenance_subscription ON pricing_snapshot_provenance (tenant_id, subscription_ref)",
    "CREATE TRIGGER trg_pricing_snapshot_provenance_no_delete BEFORE DELETE ON pricing_snapshot_provenance FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_snapshot_provenance: DELETE of a migrated-origin record is not permitted; an auditor reconstructing a legacy charge needs it to still exist'); END",
    "CREATE TRIGGER trg_pricing_snapshot_provenance_no_update BEFORE UPDATE ON pricing_snapshot_provenance FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_snapshot_provenance: a migrated-origin snapshot is frozen; it resolves through no CatalogVersion, so this row is the only thing making it immutable'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_snapshot_provenance"];

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
