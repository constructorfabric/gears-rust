//! `pricing_repricing_journal` — the per-row idempotency spine of a mass
//! repricing run (`design/12-operator-efficiency.md` §6, `inst-mr-journal`,
//! `inst-mp-journal`, `inst-mp-pending`, O3).
//!
//! Slice 12's second group. A run expands its selector into a **frozen row
//! set** and writes one `pending` row here per selected price; the apply loop
//! then moves each to `applied` or `failed` inside the same transaction that
//! writes the successor row and its outbox record. That co-transaction is the
//! whole point: a crash mid-run leaves a journal that agrees with the store, so
//! the lease takeover re-drives from it and applies nothing twice.
//!
//! # The state machine, and where its INSERT arm matters most
//!
//! ```text
//! pending ─┬─> applied      (the successor row committed)
//!          └─> failed       (this row, or its whole plan under D-134)
//! ```
//!
//! **A journal row is born `pending` and in no other state**, and on this table
//! that arm is worth more than the edges are. `pricing_bulk_operation` had no INSERT
//! arm for one commit and a run could be born past its own machine; here the
//! same hole is worse in kind, because a row born `applied` is a row the re-drive
//! **skips**. The failure is silent and permanent: the operator sees a completed
//! run, the price was never touched, and no state anywhere disagrees.
//!
//! **A decided row never moves again**, which is one guard rather than three:
//! `applied` and `failed` are terminal, and freezing the whole row at the moment
//! it stops being `pending` also stops `applied_price_id` being rewritten to name
//! a different successor. Written as `OLD.state <> 'pending'` rather than as a
//! disjunct list of the two sanctioned edges — with the vocabulary `CHECK`
//! already limiting `state` to three values, an edge list would be a rule that
//! cannot fail, which is indistinguishable from a rule that holds.
//!
//! # `not-attempted` is not a fourth state
//!
//! `inst-bs-abort` reports an aborted run's uncommitted rows as `not-attempted`.
//! That is how the **report** renders a row this table holds as `pending`; §6
//! gives the journal three states and this one keeps to them. The consequence is
//! deliberate and is why nothing here requires a terminal run's journal to be
//! free of `pending` rows: an aborted run leaves them behind forever, and §6's
//! "a run is complete when no `pending` rows remain" is a statement about the
//! runner's work rather than about the operation's state.
//!
//! # Three keys and one kind
//!
//! `run_id` names the operation; `price_id` and `applied_price_id` name the
//! selected row and the successor it produced, keyed for the reason
//! `pricing_price_tier_band` and `pricing_price_window` key theirs. A journal row
//! naming a price that does not exist is `pending` forever: the re-drive can
//! never apply it, §6's "a run is complete when no `pending` rows remain" is
//! never satisfied, and the only signal is the stalled-run alarm — which cannot
//! be told apart from a dead runner.
//!
//! **Only a repricing run journals here.** A bulk import's per-row outcomes live
//! in the operation's own `report` (`inst-bi-commit`, `inst-bk-idem`) and nothing
//! drives an import through this table, so a row here under an import is a record
//! no code will ever complete. Enforced rather than asserted in prose, because a
//! claim in a doc comment that the schema does not hold is the same class of
//! defect as a rule with no operand.
//!
//! **A journal row's tenant is its run's tenant.** `fk_pricing_repricing_journal_run`
//! covers `run_id` alone, so without this arm one tenant's run could journal
//! progress against a row carrying another tenant's `tenant_id` — and the
//! completion predicate §6 states, *"a run is complete when no `pending` rows
//! remain"*, would then be evaluated by a `SecureORM` reader over a set that does
//! not include those rows. `pricing_bulk_row_lock` states the same property for
//! the same reason one migration later (`pricing_bulk_row_lock`), out of the same
//! lookup, and the two tables carrying one rule apart was the asymmetry rather
//! than a decision.
//!
//! It is **schema hardening rather than the closing of a reachable hole**, and
//! that is worth saying precisely because the sibling's note does not: the only
//! production writer of journal rows is `repricing_journal_repo::open_rows`,
//! called from `api/rest/repricing_runs.rs`, which mints the run and journals its
//! rows from one scope inside one transaction — and `scope_with_model` pins the
//! row's own `tenant_id` to the caller's scope before the insert reaches the
//! store. So no request can produce the mismatch; direct SQL and another gear
//! can, which is the population every guard on this table is written for.
//!
//! **All parent-reading arms defer to their foreign key when the run does not
//! exist.** A `BEFORE` trigger answers ahead of the key on either engine, so an
//! arm written as "the run is not a repricing run" — or "the run is not this
//! tenant's" — would fire for a row naming *no* run, reporting a fault the caller
//! does not have and leaving the key unobservable and therefore unassertable.
//!
//! # What this table deliberately does not carry
//!
//! **No `plan_id`, though D-134 commits per plan.** §6 specifies four columns
//! beside the key and a plan is reachable from `price_id`; denormalizing it here
//! would be this group inventing a column for a group that has not been written
//! yet, and the run would then have two sources for one fact.
//!
//! **No secondary index.** Every read is run-scoped — the re-drive, the report,
//! and O3's "skip the `applied` rows" — and the primary key already leads with
//! `run_id`. The one genuinely cross-run question, *is this price row inside an
//! in-flight run*, is `pricing_bulk_row_lock`'s to answer, which is a large part
//! of why §6 makes the lock a table of its own.
//!
//! **It is revision-independent**, like `pricing_bulk_operation` and unlike
//! `pricing_composite_meter` (D-256): a run is not part of any plan revision's
//! shape, so it owes no copy-forward, no drop-on-abandon, and the closed-set
//! guard does not reach it.
//!
//! # `SQLite`
//!
//! Systematic transforms only: `bss.` dropped, `uuid` -> `text`, `timestamptz`
//! -> `text`, and the one PL/pgSQL function split into fixed-message
//! `RAISE(ABORT, …)` triggers, one per guarded condition. Four of the six read
//! only `OLD`/`NEW` and are plain `WHEN` clauses. The other two read the parent
//! run and go in the trigger **body** instead
//! (`SELECT RAISE(ABORT, …) WHERE …`), `pricing_composite_meter`'s shape exactly.
//!
//! The body rather than a `WHEN` clause is this chain's spelling for a guard that
//! reads another row, and **not** an engine limitation: a `SQLite` `WHEN` does
//! accept a subquery — `pricing_approval_key` puts a scalar `SELECT` in four of them
//! and they enforce, and sqlite 3.51 admits `WHEN NOT EXISTS (…)` directly. The
//! body form is taken so each arm's condition sits beside the message it raises,
//! one trigger per arm of the PL/pgSQL function.
//!
//! One arm of the Postgres function is one `SQLite` trigger,
//! `pricing_bulk_row_lock`'s split exactly, so the two engines' rules stay comparable
//! one to one rather than one to a conjunction.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_repricing_journal (
            tenant_id        uuid        NOT NULL,
            run_id           uuid        NOT NULL,
            price_id         uuid        NOT NULL,
            applied_at       timestamptz,
            applied_price_id uuid,
            failure_reason   text,
            state            text        NOT NULL,
            CONSTRAINT chk_pricing_repricing_journal_applied CHECK ((applied_price_id IS NOT NULL) = (state = 'applied') AND (applied_at IS NOT NULL) = (state = 'applied')),
            CONSTRAINT chk_pricing_repricing_journal_failed CHECK ((state = 'failed') = (failure_reason IS NOT NULL)),
            CONSTRAINT chk_pricing_repricing_journal_state CHECK (state IN ('pending', 'applied', 'failed')),
            CONSTRAINT chk_pricing_repricing_journal_successor_is_new CHECK (applied_price_id IS NULL OR applied_price_id <> price_id),
            CONSTRAINT fk_pricing_repricing_journal_applied_price FOREIGN KEY (applied_price_id) REFERENCES bss.pricing_price(price_id),
            CONSTRAINT fk_pricing_repricing_journal_price FOREIGN KEY (price_id) REFERENCES bss.pricing_price(price_id),
            CONSTRAINT fk_pricing_repricing_journal_run FOREIGN KEY (run_id) REFERENCES bss.pricing_bulk_operation(operation_id),
            CONSTRAINT pricing_repricing_journal_pkey PRIMARY KEY (run_id, price_id)
        )",
    "CREATE OR REPLACE FUNCTION bss.pricing_repricing_journal_progress() RETURNS trigger AS $$
        DECLARE
          run_kind   text;
          run_tenant uuid;
        BEGIN
          -- Born pending. A row born `applied` is a row the re-drive skips, so
          -- the price is never touched and nothing anywhere disagrees -- the
          -- one failure this table exists to make impossible.
          IF TG_OP = 'INSERT' THEN
            IF NEW.state <> 'pending' THEN
              RAISE EXCEPTION
                'pricing_repricing_journal: a journal row is born pending, not %', NEW.state;
            END IF;

            -- The journal is mass repricing's spine. A bulk import's per-row
            -- outcomes live in the operation's own report (inst-bi-commit,
            -- inst-bk-idem) and nothing drives an import through this table, so a
            -- row here under an import is a record no code will ever complete.
            SELECT kind, tenant_id INTO run_kind, run_tenant
              FROM bss.pricing_bulk_operation
             WHERE operation_id = NEW.run_id;
            -- No such run: the foreign key is the accurate refusal and these arms
            -- have no opinion. Deferring keeps the key **observable** -- a BEFORE
            -- trigger answers ahead of it -- and stops them reporting a kind or a
            -- tenancy fault for a run that does not exist.
            IF FOUND AND run_kind <> 'repricing' THEN
              RAISE EXCEPTION
                'pricing_repricing_journal: operation % is a %, and only a repricing run journals per-row progress',
                NEW.run_id, run_kind;
            END IF;
            -- The run exists; this proves the journal row is its own tenant's.
            -- `fk_pricing_repricing_journal_run` covers the operation id alone,
            -- so without this arm one tenant's run could journal a row carrying
            -- another tenant's id -- invisible to the scoped reader whose
            -- completion predicate is that no `pending` rows remain.
            -- `pricing_bulk_row_lock` carries the same arm out of the same lookup.
            IF FOUND AND run_tenant IS DISTINCT FROM NEW.tenant_id THEN
              RAISE EXCEPTION
                'pricing_repricing_journal: operation % belongs to another tenant and may not journal this row',
                NEW.run_id;
            END IF;
            RETURN NEW;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION
              'pricing_repricing_journal: DELETE of the row for price % of run % is not permitted; the journal is the idempotency spine and a missing row re-applies',
              OLD.price_id, OLD.run_id;
          END IF;

          -- The key and its tenant are what the row is about.
          IF NEW.run_id    IS DISTINCT FROM OLD.run_id
          OR NEW.price_id  IS DISTINCT FROM OLD.price_id
          OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id THEN
            RAISE EXCEPTION
              'pricing_repricing_journal: the row for price % of run % is keyed and its key is frozen',
              OLD.price_id, OLD.run_id;
          END IF;

          -- Decided is final, outcome columns included.
          IF OLD.state <> 'pending' THEN
            RAISE EXCEPTION
              'pricing_repricing_journal: the row for price % of run % is already %, and a decided row never moves again',
              OLD.price_id, OLD.run_id, OLD.state;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_repricing_journal_progress BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_repricing_journal FOR EACH ROW EXECUTE FUNCTION bss.pricing_repricing_journal_progress()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_repricing_journal",
    "DROP FUNCTION IF EXISTS bss.pricing_repricing_journal_progress()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_repricing_journal (
            tenant_id        text NOT NULL,
            run_id           text NOT NULL,
            price_id         text NOT NULL,
            applied_at       text,
            applied_price_id text,
            failure_reason   text,
            state            text NOT NULL,
            PRIMARY KEY (run_id, price_id),
            CONSTRAINT chk_pricing_repricing_journal_applied CHECK ((applied_price_id IS NOT NULL) = (state = 'applied') AND (applied_at IS NOT NULL) = (state = 'applied')),
            CONSTRAINT chk_pricing_repricing_journal_failed CHECK ((state = 'failed') = (failure_reason IS NOT NULL)),
            CONSTRAINT chk_pricing_repricing_journal_state CHECK (state IN ('pending', 'applied', 'failed')),
            CONSTRAINT chk_pricing_repricing_journal_successor_is_new CHECK (applied_price_id IS NULL OR applied_price_id <> price_id),
            CONSTRAINT fk_pricing_repricing_journal_applied_price FOREIGN KEY (applied_price_id) REFERENCES pricing_price(price_id),
            CONSTRAINT fk_pricing_repricing_journal_price FOREIGN KEY (price_id) REFERENCES pricing_price(price_id),
            CONSTRAINT fk_pricing_repricing_journal_run FOREIGN KEY (run_id) REFERENCES pricing_bulk_operation(operation_id)
        )",
    "CREATE TRIGGER trg_pricing_repricing_journal_born_pending BEFORE INSERT ON pricing_repricing_journal FOR EACH ROW WHEN NEW.state <> 'pending' BEGIN SELECT RAISE(ABORT, 'pricing_repricing_journal: a journal row is born pending and in no other state'); END",
    "CREATE TRIGGER trg_pricing_repricing_journal_decided_is_final BEFORE UPDATE ON pricing_repricing_journal FOR EACH ROW WHEN OLD.state <> 'pending' BEGIN SELECT RAISE(ABORT, 'pricing_repricing_journal: the journal row is already decided, and a decided row never moves again'); END",
    "CREATE TRIGGER trg_pricing_repricing_journal_frozen_key BEFORE UPDATE ON pricing_repricing_journal FOR EACH ROW WHEN NEW.run_id IS NOT OLD.run_id OR NEW.price_id IS NOT OLD.price_id OR NEW.tenant_id IS NOT OLD.tenant_id BEGIN SELECT RAISE(ABORT, 'pricing_repricing_journal: the journal row is keyed and its key is frozen'); END",
    "CREATE TRIGGER trg_pricing_repricing_journal_no_delete BEFORE DELETE ON pricing_repricing_journal FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_repricing_journal: DELETE of a journal row is not permitted; the journal is the idempotency spine and a missing row re-applies'); END",
    "CREATE TRIGGER trg_pricing_repricing_journal_only_under_a_repricing_run BEFORE INSERT ON pricing_repricing_journal FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_repricing_journal: only a repricing run journals per-row progress') WHERE EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.run_id) AND NOT EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.run_id AND kind = 'repricing'); END",
    "CREATE TRIGGER trg_pricing_repricing_journal_same_tenant_as_its_run BEFORE INSERT ON pricing_repricing_journal FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_repricing_journal: the run belongs to another tenant and may not journal this row') WHERE EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.run_id) AND NOT EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.run_id AND tenant_id = NEW.tenant_id); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_repricing_journal"];

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
