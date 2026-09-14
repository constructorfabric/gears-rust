//! `pricing_approval_threshold_tombstone` — the version that positively says
//! *this tenant has no thresholds* (D-185), one row per `(tenant_id, version)`,
//! append-only.
//!
//! `design/05-governance.md` §6 makes *"unset ⇒ two-person rule always"* the G1
//! fail-safe, and every tenant starts there. Without this table it would also be a
//! state no tenant could **return** to, and the reason is the shape of
//! `pricing_approval_threshold` rather than anybody's decision: the policy is
//! per-currency rows keyed `(tenant_id, version, currency)`, so "no thresholds"
//! would have to be a version with **zero** rows — which `threshold_repo::latest_version`
//! cannot see, which `read_version` cannot tell from a version nobody proposed, and
//! which no approval `content_hash` could cover, there being nothing to hash. The
//! authoring door refuses one outright (`ThresholdRefusal::NoEntries` →
//! `THRESHOLD_INVALID`) and **that refusal stays**: an empty entry set is
//! indistinguishable from absence, so the way back has to be a *positive* marker.
//!
//! **What the alternative actually is, corrected 2026-08-05.** This paragraph said the
//! only way back was "a version whose every entry is set absurdly high", which "silently
//! stops being true the day the tenant sells in a currency the version does not name".
//! Both halves are false, and the second is false in the reassuring direction, which is
//! why it is corrected rather than softened. `reaches_absolute` is
//! `magnitude >= absolute_minor`, so a **high** bar makes *fewer* changes material — the
//! opposite of the fail-safe. What behaves like the fail-safe is a bar of **zero**, which
//! `chk_pricing_approval_threshold_absolute_non_negative` explicitly permits and which
//! every delta reaches; so a tenant *could* already get back to "everything is material"
//! through an ordinary `PUT`. And nothing silently stops being true when a currency is
//! added: a currency with no entry meets `inst-mat-percurrency`'s own fail-safe and is
//! material anyway.
//!
//! So what this table buys is **expressiveness, not capability** (D-185 clause (1)): a
//! version reading `entries: [USD 0, EUR 0]` says *"we have thresholds and they are
//! zero"*, where the operator meant *"we have no thresholds"*. An auditor asking **when
//! did this tenant stop thresholding** reads the answer off a tombstone's number and
//! `effectiveFrom` instead of inferring it from a row of zeroes, and the statement is
//! currency-agnostic rather than an enumeration somebody has to keep complete.
//!
//! # Why a marker table, and not a column
//!
//! The two shapes the decision left open were this and a nullable column on an
//! existing row. There is no existing row: a tombstone version's whole content is
//! that it has **no** entries, so a column on `pricing_approval_threshold` would
//! need a row to sit on, and that row would need a `currency` — which is a
//! `NOT NULL` member of the primary key with `length(currency) = 3` over it. Any
//! sentinel that satisfied those would be a currency code, and a reader that could
//! not tell the sentinel from a real ISO 4217 entry is the failure this table exists
//! to prevent. So the marker is a table of its own, keyed exactly as the *version*
//! is keyed — `(tenant_id, version)`, with no currency axis, because a tombstone is
//! a statement about the whole policy and not about any currency in it.
//!
//! # What the marker has to satisfy, and what carries each part
//!
//! * **`latest_version` sees it** — it holds `version`, and `latest_version` takes
//!   the maximum across both tables, so a tombstone consumes a number and the next
//!   proposal is minted above it.
//! * **`read_version` returns it** — it holds `effective_from`, so a tombstone is a
//!   complete `StoredVersion` (its instant, and an empty entry set) rather than a
//!   half one that has to borrow a field from the table it is not in.
//! * **The pin covers it** — `content_pin::put_threshold_version` frames the entry
//!   count before the entries, and `ThresholdVersion::new` refuses an empty set, so
//!   a count of zero is a preimage no non-tombstone version can produce. An approver
//!   signs "no thresholds" under a digest distinguishable from every particular set.
//! * **`effective_version` returns it as in force, and empty** — it is a version like
//!   any other, so the walk finds it, its unit's approval makes it effective, and
//!   `ThresholdVersion::policy()` answers `None` for it, which is exactly what makes
//!   `materiality::evaluate` answer `noConfiguredThreshold` again.
//!
//! # There is no cross-table CHECK, and the reader is what fails closed
//!
//! Nothing in either schema stops one version number carrying both a tombstone row
//! and entry rows — the two tables have two primary keys and neither sees the other.
//! It is reachable: two proposals that read the same `latest_version` and mint the
//! same number, one retiring and one configuring, collide on nothing. A trigger
//! could refuse it, but it would have to query the sibling table on every insert of
//! either, which makes each table's append path depend on the other's contents and
//! puts the invariant somewhere no reader of either table would look for it.
//!
//! So it is `threshold_repo::read_version` that refuses, as `RepoError::CorruptRow`,
//! the way it already refuses a version whose rows disagree about their
//! `effective_from`. A version that is both is a version **no approver signed** —
//! one signed the empty digest, the other signed the entry digest, and the stored
//! version is neither — so it is skipped by `infra::approval::read_threshold_version`
//! and the tenant stays on the version they already had. Neither proposal takes
//! effect, which is the only answer that is not somebody's signature applied to
//! content they did not see.
//!
//! # The constraints, and what each refuses
//!
//! * `chk_pricing_approval_threshold_tombstone_version` — `version >= 0`, for
//!   `chk_pricing_approval_threshold_version`'s reason: the first version a tenant
//!   proposes is `0`, and a negative one would sort under it forever. The two
//!   tables' version columns are one sequence, so they carry one rule.
//!
//! # Append-only, and the same split-trigger discipline
//!
//! Every column is content — the version, the instant it takes effect and the
//! provenance of the proposal — so `DELETE` is refused and `UPDATE` is refused, with
//! **two** triggers rather than one function bound to both events. That is
//! `pricing_approval_threshold`'s convention and it is kept for its stated reason: one
//! function with both arms makes the `DELETE` arm unprovable, because removing it
//! leaves a `DELETE` refused by the other arm with a different sentence and the
//! proof degenerates into a proof about a message.
//!
//! `created_by` / `created_at` are the crate's provenance idiom and are **not** the
//! approval trail: D-10 puts the second principal on `pricing_approval`, and a
//! tombstone rides exactly that unit.
//!
//! **No `REVOKE`**, for the chain's stated reason: it issues no grants and `SQLite`
//! has no `GRANT`/`REVOKE` at all.
//!
//! # Backend differences
//!
//! The systematic type mirror only (`uuid` -> `text`, `timestamptz` -> `text`,
//! `now()` -> the RFC 3339 `strftime` its writers spell, the `bss.` prefix dropped), plus the two
//! PL/pgSQL functions becoming two literal-message `RAISE(ABORT, …)` triggers,
//! `SQLite` having no procedural language and no message interpolation. The trigger
//! names are the same on both backends.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_approval_threshold_tombstone (
            tenant_id      uuid        NOT NULL,
            version        bigint      NOT NULL,
            effective_from timestamptz NOT NULL,
            created_at     timestamptz NOT NULL DEFAULT now(),
            created_by     uuid        NOT NULL,
            CONSTRAINT chk_pricing_approval_threshold_tombstone_version CHECK (version >= 0),
            CONSTRAINT pricing_approval_threshold_tombstone_pkey PRIMARY KEY (tenant_id, version)
        )",
    "CREATE OR REPLACE FUNCTION bss.pricing_approval_threshold_tombstone_no_delete() RETURNS trigger AS $$
        BEGIN
          RAISE EXCEPTION
            'pricing_approval_threshold_tombstone: DELETE of tenant % version % is not permitted; a threshold policy is append-only history',
            OLD.tenant_id, OLD.version;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE OR REPLACE FUNCTION bss.pricing_approval_threshold_tombstone_no_update() RETURNS trigger AS $$
        BEGIN
          RAISE EXCEPTION
            'pricing_approval_threshold_tombstone: version % is immutable; a correction is a new version, because an earlier version is what an approval pin covers',
            OLD.version;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_approval_threshold_tombstone_no_delete BEFORE DELETE ON bss.pricing_approval_threshold_tombstone FOR EACH ROW EXECUTE FUNCTION bss.pricing_approval_threshold_tombstone_no_delete()",
    "CREATE TRIGGER trg_pricing_approval_threshold_tombstone_no_update BEFORE UPDATE ON bss.pricing_approval_threshold_tombstone FOR EACH ROW EXECUTE FUNCTION bss.pricing_approval_threshold_tombstone_no_update()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_approval_threshold_tombstone",
    "DROP FUNCTION IF EXISTS bss.pricing_approval_threshold_tombstone_no_delete()",
    "DROP FUNCTION IF EXISTS bss.pricing_approval_threshold_tombstone_no_update()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_approval_threshold_tombstone (
            tenant_id      text   NOT NULL,
            version        bigint NOT NULL,
            effective_from text   NOT NULL,
            created_at     text   NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            created_by     text   NOT NULL,
            PRIMARY KEY (tenant_id, version),
            CONSTRAINT chk_pricing_approval_threshold_tombstone_version CHECK (version >= 0)
        )",
    "CREATE TRIGGER trg_pricing_approval_threshold_tombstone_no_delete BEFORE DELETE ON pricing_approval_threshold_tombstone FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_approval_threshold_tombstone: DELETE of a threshold version is not permitted; a threshold policy is append-only history'); END",
    "CREATE TRIGGER trg_pricing_approval_threshold_tombstone_no_update BEFORE UPDATE ON pricing_approval_threshold_tombstone FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_approval_threshold_tombstone: a threshold version is immutable; a correction is a new version, because an earlier version is what an approval pin covers'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] =
    &["DROP TABLE IF EXISTS pricing_approval_threshold_tombstone"];

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
