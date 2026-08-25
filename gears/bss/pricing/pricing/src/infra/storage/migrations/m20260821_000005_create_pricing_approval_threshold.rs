//! `pricing_approval_threshold` — the tenant approval-threshold policy, one row
//! per `(tenant_id, version, currency)`, append-only.
//!
//! `design/05-governance.md` §6 states the policy as *"per-currency
//! `{absolute_minor | percent}` thresholds; **unset ⇒ two-person rule always**"*,
//! and `inst-mat-percurrency` rests its fail-safe on *"a row whose currency has
//! **no threshold entry** … is material (the G1 fail-safe applies per currency,
//! not per policy object)"*. This table is the operand that sentence had none of.
//!
//! # Why a table of its own, and not one of the two that already exist
//!
//! **Not a column pair on `pricing_policy_object`.** That is where it lived
//! first: `approval_threshold_minor bigint` beside `approval_threshold_currency
//! varchar(3)` — **one** currency's absolute threshold, with a co-nullability
//! CHECK and a non-negativity CHECK. A sentence about *"a row whose currency has
//! no threshold entry"* has no operand against a single column pair, and §6's own
//! `{absolute_minor | percent}` has no home in a column that is only an amount.
//! `pricing_policy_object` therefore carries no such pair at all, rather than one
//! standing beside its replacement, because two places to read a threshold from is the
//! shape this programme has twice found a stale claim surviving in.
//!
//! **Not ledger's.** `ledger_currency_scale_registry` is keyed
//! `(tenant_id, currency)` and holds `minor_units` / `plausible_max_major` /
//! `source` — currency *reference data*, not policy, and another gear's table
//! besides: pricing reaches other gears through SDK clients, never through their
//! schema. `ledger_dual_control_policy`, the precedent §3's authz catalog cites
//! **by name**, carries a single `d2_threshold_minor` and is **currency-agnostic**
//! — so it supports the *resource separation* argument (a policy a config admin
//! must not edit) and says nothing about a per-currency shape. Ledger never needed
//! one; its threshold lives in the entry's own currency.
//!
//! # Why the policy is versioned, and why that is load-bearing rather than tidy
//!
//! What ledger's precedent *does* give is `PRIMARY KEY (tenant_id, version)` plus
//! `effective_from` — append-only history rather than mutation in place. That is
//! the half this table needs.
//!
//! **D-10 makes a threshold-policy PUT an always-material approval unit**: the
//! diff applies only after an independent `FinanceReviewer` approves it. And
//! `pricing_approval` carries a `content_hash` and **no content column** — the
//! pinned content is re-derived from the subject (D-61, `GET /approvals/{id}`).
//! Under mutation in place the "after" state does not exist until it is applied,
//! so the proposed diff would have nowhere to live and the pin nothing to cover.
//! Versioned rows resolve it exactly: the proposal is a **new version**, the pin
//! covers that version's rows, and the version becomes the tenant's policy when
//! its unit is approved. Which version is in effect is therefore a fact about the
//! approval store and **not a column here** — see `threshold_repo`, whose
//! `effective_policy` reads it. A `state` column would be a second answer to it,
//! free to disagree with the record that decided it, and flipping one would need
//! an UPDATE this table refuses.
//!
//! # `percent` is basis points, and that is a decision rather than a reading
//!
//! §6 says only `percent > 0` and **the design set declares no representation for
//! it anywhere**. Basis points is the set's own idiom — D-104's `share_bp` and
//! `platform_cut_bp` — and an integer keeps the comparison integral, with no
//! floating point beside money. `percent_bp` is therefore what the column is
//! called, so a reader meets the unit in the name and the wire DTO does not pick
//! a second one. **It is recorded here as a decision owed to the register**, not
//! as something read out of a document.
//!
//! # The constraints, and what each refuses
//!
//! * `chk_pricing_approval_threshold_currency` — three characters. The ISO 4217
//!   **shape**; the *validity* of the code is `THRESHOLD_INVALID`'s job at the
//!   surface and `CurrencyCode`'s in the domain, and a CHECK that tried to hold
//!   the register would be a third owner of it.
//! * `chk_pricing_approval_threshold_basis` — exactly one of the two bases is
//!   set. §6's `{absolute_minor | percent}` is a choice, not a pair: a row with
//!   both would leave the evaluator picking one, and a row with neither is an
//!   entry that thresholds nothing while still counting as "this currency has an
//!   entry", which is the fail-safe switched off by an empty row.
//! * `chk_pricing_approval_threshold_absolute_non_negative` — §6's
//!   `absolute_minor ≥ 0`. A negative threshold is below every change there is,
//!   which is a two-person rule switched off by arithmetic.
//! * `chk_pricing_approval_threshold_percent_positive` — §6's `percent > 0`,
//!   verbatim. Zero would auto-publish every change that moved by nothing at all.
//! * `chk_pricing_approval_threshold_version` — `version >= 0`. The first version
//!   a tenant proposes is `0`, and a negative one would sort under it forever.
//!
//! # Append-only, and stricter than the window table's
//!
//! `pricing_price_window` whitelists the columns a state flip may move. Here
//! **every** column is content — the keys, the two bases, the instant it takes
//! effect and the provenance of the proposal — so there is no whitelist to write
//! and the discipline is the simplest one: `DELETE` is refused and `UPDATE` is
//! refused. A correction is a new version, which is what makes the pin of an
//! earlier version still mean what it meant when it was signed.
//!
//! `created_by` / `created_at` are the crate's own provenance idiom
//! (`pricing_price_window` carries the same pair) and are where the `AuditStamp`
//! every mutating repository call takes comes to rest. They are **not** the
//! approval trail — that is `pricing_approval`'s, and D-10 puts the second
//! principal there.
//!
//! **No `REVOKE`.** The chain issues no grants, deliberately and in writing
//! (`pricing_plan`'s module doc says so), and `SQLite` has no `GRANT`/`REVOKE` at
//! all, so
//! where the design set says "REVOKE + trigger discipline" the portable half is
//! what is built.
//!
//! # Backend differences
//!
//! Beyond the systematic type mirror (`uuid` -> `text`, `timestamptz` -> `text`,
//! `now()` -> the RFC 3339 `strftime` its writers spell, the `bss.` prefix dropped):
//!
//! * the two PL/pgSQL functions become two literal-message `RAISE(ABORT, …)`
//!   triggers, `SQLite` having no procedural language and no message
//!   interpolation. **The two arms are two triggers on Postgres as well**, which
//!   the other tables in this chain do not do — they bind one `_append_only`
//!   function to `BEFORE UPDATE OR DELETE`. One function with both arms would make
//!   the DELETE arm unprovable: removing it leaves a `DELETE` refused by the arm
//!   below with a different sentence, so the proof degenerates into a proof about
//!   a message. Split, each arm's removal lets exactly its own statement through,
//!   and the trigger names are then the same on both backends;
//!
//! The `down` is symmetric on both backends: the table goes, and on Postgres its
//! two trigger functions go with it.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_approval_threshold (
            tenant_id      uuid        NOT NULL,
            version        bigint      NOT NULL,
            currency       varchar(3)  NOT NULL,
            absolute_minor bigint,
            effective_from timestamptz NOT NULL,
            percent_bp     integer,
            created_at     timestamptz NOT NULL DEFAULT now(),
            created_by     uuid        NOT NULL,
            CONSTRAINT chk_pricing_approval_threshold_absolute_non_negative CHECK (absolute_minor IS NULL OR absolute_minor >= 0),
            CONSTRAINT chk_pricing_approval_threshold_basis CHECK ((absolute_minor IS NULL) <> (percent_bp IS NULL)),
            CONSTRAINT chk_pricing_approval_threshold_currency CHECK (length(currency) = 3),
            CONSTRAINT chk_pricing_approval_threshold_percent_positive CHECK (percent_bp IS NULL OR percent_bp > 0),
            CONSTRAINT chk_pricing_approval_threshold_version CHECK (version >= 0),
            CONSTRAINT pricing_approval_threshold_pkey PRIMARY KEY (tenant_id, version, currency)
        )",
    "CREATE OR REPLACE FUNCTION bss.pricing_approval_threshold_no_delete() RETURNS trigger AS $$
        BEGIN
          RAISE EXCEPTION
            'pricing_approval_threshold: DELETE of tenant % version % is not permitted; a threshold policy is append-only history',
            OLD.tenant_id, OLD.version;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE OR REPLACE FUNCTION bss.pricing_approval_threshold_no_update() RETURNS trigger AS $$
        BEGIN
          RAISE EXCEPTION
            'pricing_approval_threshold: version % is immutable; a correction is a new version, because an earlier version is what an approval pin covers',
            OLD.version;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_approval_threshold_no_delete BEFORE DELETE ON bss.pricing_approval_threshold FOR EACH ROW EXECUTE FUNCTION bss.pricing_approval_threshold_no_delete()",
    "CREATE TRIGGER trg_pricing_approval_threshold_no_update BEFORE UPDATE ON bss.pricing_approval_threshold FOR EACH ROW EXECUTE FUNCTION bss.pricing_approval_threshold_no_update()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_approval_threshold",
    "DROP FUNCTION IF EXISTS bss.pricing_approval_threshold_no_delete()",
    "DROP FUNCTION IF EXISTS bss.pricing_approval_threshold_no_update()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_approval_threshold (
            tenant_id      text       NOT NULL,
            version        bigint     NOT NULL,
            currency       varchar(3) NOT NULL,
            absolute_minor bigint,
            effective_from text       NOT NULL,
            percent_bp     integer,
            created_at     text       NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            created_by     text       NOT NULL,
            PRIMARY KEY (tenant_id, version, currency),
            CONSTRAINT chk_pricing_approval_threshold_absolute_non_negative CHECK (absolute_minor IS NULL OR absolute_minor >= 0),
            CONSTRAINT chk_pricing_approval_threshold_basis CHECK ((absolute_minor IS NULL) <> (percent_bp IS NULL)),
            CONSTRAINT chk_pricing_approval_threshold_currency CHECK (length(currency) = 3),
            CONSTRAINT chk_pricing_approval_threshold_percent_positive CHECK (percent_bp IS NULL OR percent_bp > 0),
            CONSTRAINT chk_pricing_approval_threshold_version CHECK (version >= 0)
        )",
    "CREATE TRIGGER trg_pricing_approval_threshold_no_delete BEFORE DELETE ON pricing_approval_threshold FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_approval_threshold: DELETE of a threshold version is not permitted; a threshold policy is append-only history'); END",
    "CREATE TRIGGER trg_pricing_approval_threshold_no_update BEFORE UPDATE ON pricing_approval_threshold FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_approval_threshold: a threshold version is immutable; a correction is a new version, because an earlier version is what an approval pin covers'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_approval_threshold"];

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
