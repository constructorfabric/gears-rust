//! `pricing_bulk_operation` — Slice 12's bulk-operation record and the state
//! machine §4 gives it (`design/12-operator-efficiency.md` §4, §6, `inst-bs-*`,
//! D-37, D-137, O4).
//!
//! Both of the slice's flows — bulk import and mass repricing — are one record
//! moving through six states, and nothing else in Slice 12 can be written until
//! that record exists. It is the first group of the slice for that reason.
//!
//! # The state machine is in the trigger, and the edges are §4's exactly
//!
//! ```text
//! validating ─┬─> validation_failed          (inst-bs-fail)
//!             ├─> awaiting_approval          (inst-bs-approval, repricing only)
//!             └─> committing                 (inst-bs-commit)
//! awaiting_approval ──> committing           (inst-bs-commit, on approval)
//! committing ─┬─> completed                  (inst-bs-done)
//!             └─> completed_with_conflicts    (inst-bs-done / inst-bs-abort)
//! ```
//!
//! **`awaiting_approval` is unreachable for `kind = import`, and the store says
//! so** rather than leaving it to a caller: D-137 makes draft-plane authoring
//! never material, so an import that parked awaiting an approval would be waiting
//! for a decision no trigger can ever produce. That is a `CHECK`, not a
//! convention — it is the one edge of §4 whose violation would strand a record
//! forever.
//!
//! **The three terminal states are terminal.** `validation_failed`, `completed`
//! and `completed_with_conflicts` have no outgoing edge in §4, and a record that
//! left one would re-run a commit whose rows are already applied — the journal
//! (`pricing_repricing_journal`, this slice's next table) is the idempotency
//! spine precisely because a re-drive must be safe, and a terminal record
//! re-entering `committing` is not a re-drive but a second run wearing the first
//! one's id.
//!
//! # What this table deliberately does not carry
//!
//! **The row lock and the journal are their own tables** (§6), and neither is
//! this group's: `pricing_bulk_row_lock` is a side table the Foundation's
//! concurrent-edit check reads, deliberately not a column on `pricing_price`
//! (2026-07-31 review fix), and `pricing_repricing_journal` is keyed
//! `(run_id, price_id)`. Both land with the flows that write them.
//!
//! **It is revision-independent**, so unlike `pricing_composite_meter` (D-256) it
//! owes no copy-forward and no drop-on-abandon: a bulk operation is not part of a
//! plan revision's shape, and
//! `the_revision_scoped_tables_are_a_closed_set_and_each_one_is_copied_and_dropped`
//! does not reach it. That is the one way this table is cheaper than the last.
//!
//! # `SQLite`
//!
//! Systematic transforms only: `bss.` dropped, `uuid` -> `text`, `jsonb` ->
//! `text`, and the one PL/pgSQL function split into fixed-message
//! `RAISE(ABORT, …)` triggers, one per guarded verb. Every guard here reads only
//! `OLD`/`NEW`, so each becomes a plain `WHEN` and none needs the body form the
//! sibling tables use for a cross-table read — which is a spelling rather than an
//! engine limitation; see `pricing_repricing_journal`.
//!
//! `rejected` joins the bulk-operation state machine -- D-267's code half.
//!
//! Section 4 gave the machine six states and one exit from `awaiting_approval`:
//! `-> committing`, on approval. A batch approval that is **refused** therefore
//! had nowhere to put the run. `DELETE` is refused by the table's own trigger, so
//! the record cannot be withdrawn; `uq_pricing_bulk_operation_client_key` is
//! unique per tenant, so the operator cannot resubmit the same work under the
//! same key; and no edge leaves `awaiting_approval` except the one the rejection
//! did not take. A rejected repricing run was stranded in `awaiting_approval`
//! permanently, holding its client key against every retry.
//!
//! D-260 recorded the gap on the day `pricing_bulk_operation` built the machine and
//! left it, deliberately, for a decision. This is that decision built: one
//! state, one edge, and nothing else.
//!
//! # `rejected` is terminal, and it is reachable from exactly one state
//!
//! ```text
//! awaiting_approval -> rejected            (inst-bs-reject)
//! ```
//!
//! No arm names `rejected` on the left of an edge, so the machine leaves it by
//! nothing -- the same way `pricing_price_overlay` makes `abandoned` terminal without
//! a clause naming it, and for the same reason: an edge list that is a whitelist
//! makes every unnamed move a refusal already.
//!
//! **A rejected run is over, so it carries a `completed_at`.** The terminal set
//! in `chk_pricing_bulk_operation_completed_at` gains it beside
//! `validation_failed`, `completed` and `completed_with_conflicts`, which is what
//! keeps the instant and the state from disagreeing about whether the run ended.
//!
//! # D-137 still holds, and it holds without a new clause
//!
//! An import can never be **rejected**, because `rejected` is reachable only from
//! `awaiting_approval` and `chk_pricing_bulk_operation_import_never_awaits`
//! forbids an import that state. The new edge inherits the old `CHECK` rather
//! than restating it: a second constraint saying the same thing would be a rule
//! that can never fail, which is indistinguishable from a rule that holds.
//! Verified behaviourally on both engines rather than argued -- an import walking
//! `validating -> rejected` is refused by the edge list, and one born `rejected`
//! by the born-validating arm.
//!
//! # Both child tables carry a foreign key to this one, and their **rows** are
//!
//! # The dangerous half is the replay, not the insert
//!
//! An insert refused across kinds is at least a refusal. What the shared
//! namespace really costs is on the read: `bulk_repo::find_by_client_key` filters
//! `(tenant_id, client_key)` and `BulkImportView` carries **no `kind` member**, so
//! once the repricing engine exists an import `POST` under a key a run holds
//! would answer `202 ACCEPTED` describing **the run**, import nothing, and hand
//! the caller a document with no field that could reveal the substitution. That
//! is the inversion D-295 fixed on the state axis and left open on this one; the
//! query is fixed with this index, and either fix alone would be half of it.
//!
//! # Per-`kind` uniqueness, not weaker
//!
//! `inst-bs-reject`'s auditability argument rests on the key staying spent: an
//! operator's remedy for a refused run is a fresh run under a new key, and "O4's
//! per-tenant uniqueness holds the old key against the rejected record".
//! `(tenant_id, kind, client_key)` keeps exactly that and separates only the two
//! flows, which is what §5 already separates.
//!
//! # A digest and not the payload, for `pricing_idempotency_dedup`'s reason
//!
//! `bytea`/`blob`, the SHA-256 of the canonical request rendering
//! (`preconditions::request_digest`). The run needs to know whether two requests
//! are the same, not what they said, and retaining request bodies on a run row
//! would put a second, unmanaged copy of what callers sent beside the audit trail
//! that is supposed to be the one place it lives.
//!
//! # `request_hash` is frozen on a submitted run
//!
//! The column is in the append-only guard's whitelist of things that may **not**
//! move: a run's request hash is what makes a resubmission recognisable as the same
//! work, so a run whose hash could be edited would answer the idempotency question
//! differently after the fact. Adding a column and freezing it are one act here, not
//! two — a price or identity column outside the whitelist is the gap this discipline
//! exists to close.
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
    r"CREATE TABLE bss.pricing_bulk_operation (
            tenant_id    uuid        NOT NULL,
            operation_id uuid        NOT NULL,
            client_key   text        NOT NULL,
            completed_at timestamptz,
            kind         text        NOT NULL,
            report       jsonb       NOT NULL DEFAULT '{}'::jsonb,
            request_hash bytea       NOT NULL DEFAULT '\x'::bytea,
            state        text        NOT NULL,
            submitted_at timestamptz NOT NULL,
            submitted_by uuid        NOT NULL,
            CONSTRAINT chk_pricing_bulk_operation_completed_at CHECK ((completed_at IS NOT NULL) = (state IN ('validation_failed', 'completed', 'completed_with_conflicts', 'rejected'))),
            CONSTRAINT chk_pricing_bulk_operation_import_never_awaits CHECK (NOT (kind = 'import' AND state = 'awaiting_approval')),
            CONSTRAINT chk_pricing_bulk_operation_kind CHECK (kind IN ('import', 'repricing')),
            CONSTRAINT chk_pricing_bulk_operation_state CHECK (state IN ('validating', 'validation_failed', 'awaiting_approval', 'committing', 'completed', 'completed_with_conflicts', 'rejected')),
            CONSTRAINT pricing_bulk_operation_pkey PRIMARY KEY (operation_id)
        )",
    "CREATE INDEX idx_pricing_bulk_operation_live ON bss.pricing_bulk_operation USING btree (tenant_id, state, submitted_at)",
    "CREATE UNIQUE INDEX uq_pricing_bulk_operation_client_key ON bss.pricing_bulk_operation USING btree (tenant_id, kind, client_key)",
    "CREATE OR REPLACE FUNCTION bss.pricing_bulk_operation_transitions() RETURNS trigger AS $$
        BEGIN
          -- **A run is born `validating` and in no other state.** -4 names it the
          -- initial state, and without this arm the whole machine is a rule about
          -- UPDATE with a row free to be born `committing` -- past the approval
          -- gate transitions 2 and 3 exist to impose - or born terminal,
          -- reporting outcomes for rows it never committed. `pricing_approval`
          -- carries the same arm for the same reason, in the same words.
          IF TG_OP = 'INSERT' THEN
            IF NEW.state <> 'validating' THEN
              RAISE EXCEPTION
                'pricing_bulk_operation: a run is born validating, not %', NEW.state;
            END IF;
            RETURN NEW;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION
              'pricing_bulk_operation: DELETE of operation % is not permitted; a run is a record, not a draft',
              OLD.operation_id;
          END IF;

          -- Identity and provenance are frozen; only the run's progress moves.
          IF NEW.operation_id IS DISTINCT FROM OLD.operation_id
          OR NEW.tenant_id    IS DISTINCT FROM OLD.tenant_id
          OR NEW.kind         IS DISTINCT FROM OLD.kind
          OR NEW.client_key   IS DISTINCT FROM OLD.client_key
          OR NEW.submitted_by IS DISTINCT FROM OLD.submitted_by
          OR NEW.submitted_at IS DISTINCT FROM OLD.submitted_at
          OR NEW.request_hash IS DISTINCT FROM OLD.request_hash THEN
            RAISE EXCEPTION
              'pricing_bulk_operation: operation % is frozen; only state, report and completed_at move',
              OLD.operation_id;
          END IF;

          IF NEW.state = OLD.state THEN
            RETURN NEW;
          END IF;

          -- Section 4's edges, and nothing else.
          IF NOT (
               (OLD.state = 'validating'        AND NEW.state IN ('validation_failed', 'awaiting_approval', 'committing'))
            OR (OLD.state = 'awaiting_approval' AND NEW.state = 'committing')
            OR (OLD.state = 'awaiting_approval' AND NEW.state = 'rejected')
            OR (OLD.state = 'committing'        AND NEW.state IN ('completed', 'completed_with_conflicts'))
          ) THEN
            RAISE EXCEPTION
              'pricing_bulk_operation: state % -> % is not an edge of the bulk state machine',
              OLD.state, NEW.state;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_bulk_operation_transitions BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_bulk_operation FOR EACH ROW EXECUTE FUNCTION bss.pricing_bulk_operation_transitions()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_bulk_operation",
    "DROP FUNCTION IF EXISTS bss.pricing_bulk_operation_transitions()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_bulk_operation (
            tenant_id    text NOT NULL,
            operation_id text NOT NULL,
            client_key   text NOT NULL,
            completed_at text,
            kind         text NOT NULL,
            report       text NOT NULL DEFAULT '{}',
            request_hash blob NOT NULL DEFAULT X'',
            state        text NOT NULL,
            submitted_at text NOT NULL,
            submitted_by text NOT NULL,
            PRIMARY KEY (operation_id),
            CONSTRAINT chk_pricing_bulk_operation_completed_at CHECK ((completed_at IS NOT NULL) = (state IN ('validation_failed', 'completed', 'completed_with_conflicts', 'rejected'))),
            CONSTRAINT chk_pricing_bulk_operation_import_never_awaits CHECK (NOT (kind = 'import' AND state = 'awaiting_approval')),
            CONSTRAINT chk_pricing_bulk_operation_kind CHECK (kind IN ('import', 'repricing')),
            CONSTRAINT chk_pricing_bulk_operation_state CHECK (state IN ('validating', 'validation_failed', 'awaiting_approval', 'committing', 'completed', 'completed_with_conflicts', 'rejected'))
        )",
    "CREATE INDEX idx_pricing_bulk_operation_live ON pricing_bulk_operation (tenant_id, state, submitted_at)",
    "CREATE UNIQUE INDEX uq_pricing_bulk_operation_client_key ON pricing_bulk_operation (tenant_id, kind, client_key)",
    "CREATE TRIGGER trg_pricing_bulk_operation_born_validating BEFORE INSERT ON pricing_bulk_operation FOR EACH ROW WHEN NEW.state <> 'validating' BEGIN SELECT RAISE(ABORT, 'pricing_bulk_operation: a run is born validating and in no other state'); END",
    "CREATE TRIGGER trg_pricing_bulk_operation_frozen_columns BEFORE UPDATE ON pricing_bulk_operation FOR EACH ROW WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.tenant_id IS NOT OLD.tenant_id OR NEW.kind IS NOT OLD.kind OR NEW.client_key IS NOT OLD.client_key OR NEW.submitted_by IS NOT OLD.submitted_by OR NEW.submitted_at IS NOT OLD.submitted_at OR NEW.request_hash IS NOT OLD.request_hash BEGIN SELECT RAISE(ABORT, 'pricing_bulk_operation: the operation is frozen; only state, report and completed_at move'); END",
    "CREATE TRIGGER trg_pricing_bulk_operation_no_delete BEFORE DELETE ON pricing_bulk_operation FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bulk_operation: DELETE of an operation is not permitted; a run is a record, not a draft'); END",
    "CREATE TRIGGER trg_pricing_bulk_operation_transitions BEFORE UPDATE ON pricing_bulk_operation FOR EACH ROW WHEN NEW.state IS NOT OLD.state AND NOT ((OLD.state = 'validating' AND NEW.state IN ('validation_failed', 'awaiting_approval', 'committing')) OR (OLD.state = 'awaiting_approval' AND NEW.state = 'committing') OR (OLD.state = 'awaiting_approval' AND NEW.state = 'rejected') OR (OLD.state = 'committing' AND NEW.state IN ('completed', 'completed_with_conflicts'))) BEGIN SELECT RAISE(ABORT, 'pricing_bulk_operation: that state move is not an edge of the bulk state machine'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_bulk_operation"];

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
