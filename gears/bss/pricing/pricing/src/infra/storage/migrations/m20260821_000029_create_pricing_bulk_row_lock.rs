//! `pricing_bulk_row_lock` — the bulk lock, as a **side table**
//! (`design/12-operator-efficiency.md` §6, `inst-bk-lock`, `inst-bs-commit`,
//! `inst-bs-approval`, D-37; Foundation `fr-concurrent-edit`).
//!
//! Slice 12's third group. While a run is committing, the rows it holds are
//! marked here, and an interactive edit targeting one is refused with a conflict
//! **naming the operation** — which is the whole reason the marker is a row with
//! an `bulk_operation_id` on it rather than a boolean anywhere.
//!
//! # Why this is not a column on `pricing_price`
//!
//! A 2026-07-31 review fix, and the earlier "nullable column **or** a lock side
//! table, implementation choice" offered an option that cannot be built: the rows
//! a repricing run locks are **published**, and `pricing_price`'s append-only
//! trigger whitelists exactly two `UPDATE`s on a published row — the
//! `lifecycle_state` flip and a monotonic `grandfather_until` tightening
//! (Foundation §3.7). Writing a lock marker onto the row is refused by that
//! trigger. The side table also releases without touching truth data.
//!
//! # The custody rules, and the one this table deliberately does not have
//!
//! **A lock may be taken only under a run that is `committing`.** §4 is explicit
//! in both directions: `inst-bs-commit` starts the lock on entry to `committing`,
//! and `inst-bs-approval` says rows are **not** locked while a run awaits its
//! approval — so a lock taken early would bounce interactive supersessions on
//! keys the design says are free.
//!
//! **A lock is taken or released, never edited.** `UPDATE` is refused outright.
//! Moving a lock to another operation in place would tell the interactive editor
//! that an operation which never took the row holds it, and would leave the
//! release path — which keys on the operation — unable to find what it owns.
//!
//! **`DELETE` is not guarded at all, and that is the rule rather than an
//! omission.** Both sibling tables of this slice refuse `DELETE` outright, and
//! copying that here would be precisely wrong: a lock is transient and release
//! *is* a delete. Two grounds, both from the design set rather than from a
//! release actor it does not have:
//!
//! 1. **§6 orders the abort as one act** — `committing → completed_with_conflicts`
//!    with "the lock cleared" — so a guard admitting `DELETE` only under a
//!    `committing` run would refuse the clear inside the very transaction §6
//!    specifies, depending on which of the two statements ran first.
//! 2. **A crash between the terminal transition and the release strands the
//!    locks.** Under such a guard they would then be unreleasable *permanently*,
//!    the run being terminal — which is the freeze D-37's release path exists to
//!    prevent: a crashed import must never freeze interactive authoring
//!    indefinitely.
//!
//! The trigger is therefore bound to `INSERT OR UPDATE` and not to `DELETE`, so
//! the absence is structural rather than a fall-through nobody notices.
//!
//! **A lock is not restricted to a repricing run, and that absence is
//! load-bearing.** `pricing_repricing_journal` carries an arm admitting only
//! `kind = repricing`, and the symmetry is a trap: `inst-bk-lock` is the
//! **import's** own rule — "rows in an in-flight import are marked" — and
//! `inst-bs-commit` puts *every* import on the edge into `committing`. An arm
//! copied here from the sibling would leave every bulk import unable to take its
//! lock at commit. A case in each suite locks under an `import` run for exactly
//! that reason.
//!
//! **The lock's tenant is its run's tenant.** The foreign key covers
//! `bulk_operation_id` alone, so without this arm one tenant's run could mark
//! another tenant's price row — and the concurrent-edit check, scoped to the
//! second tenant, would refuse an edit naming an operation that tenant cannot
//! see. Checked in the same lookup the state arm needs, so it costs nothing.
//!
//! **Both `INSERT` arms defer to the foreign key when the run does not exist.**
//! A `BEFORE` trigger answers ahead of the key on either engine, so an arm
//! written as "the run is not this tenant's" would fire for a lock naming *no*
//! run — reporting a tenancy fault the caller does not have, and leaving the key
//! unobservable and therefore unassertable. `pricing_composite_meter` records
//! exactly that shadowing as a limitation; here it is designed out instead, which
//! costs one `EXISTS` conjunct per arm and makes each refusal mean what it says.
//!
//! # No `CHECK` constraints, and one index that is genuinely needed
//!
//! There is no vocabulary column here: every rule this table has is about
//! custody rather than about columns agreeing with each other. The index over
//! `(tenant_id, bulk_operation_id)` is the release path's own query — unlike
//! `pricing_repricing_journal`, whose reads are all run-scoped and covered by its
//! key, this table is keyed for the *reader* (the concurrent-edit check, by
//! price) and swept by the *writer* (release, by operation).
//!
//! # `SQLite`
//!
//! Systematic transforms only: `bss.` dropped, `uuid` -> `text`, `timestamptz`
//! -> `text`, and the one PL/pgSQL function split into fixed-message
//! `RAISE(ABORT, …)` triggers, one per guarded condition. Both `INSERT` arms read
//! the parent run, so they are written as a subquery in the trigger **body**
//! (`SELECT RAISE(ABORT, …) WHERE NOT EXISTS (…)`) rather than in a `WHEN` clause
//! — `pricing_composite_meter` carries the same shape for the same reason.
//!
//! The body rather than a `WHEN` clause is this chain's spelling for a guard that
//! reads another row, and **not** an engine limitation: a `SQLite` `WHEN` does
//! accept a subquery — `pricing_approval_key` puts a scalar `SELECT` in four of them
//! and they enforce, and sqlite 3.51 admits `WHEN NOT EXISTS (…)` directly. The
//! body form is taken so each arm's condition sits beside the message it raises,
//! one trigger per arm of the PL/pgSQL function.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_bulk_row_lock (
            tenant_id         uuid        NOT NULL,
            price_id          uuid        NOT NULL,
            bulk_operation_id uuid        NOT NULL,
            locked_at         timestamptz NOT NULL,
            CONSTRAINT fk_pricing_bulk_row_lock_operation FOREIGN KEY (bulk_operation_id) REFERENCES bss.pricing_bulk_operation(operation_id),
            CONSTRAINT fk_pricing_bulk_row_lock_price FOREIGN KEY (price_id) REFERENCES bss.pricing_price(price_id),
            CONSTRAINT pricing_bulk_row_lock_pkey PRIMARY KEY (tenant_id, price_id)
        )",
    "CREATE INDEX idx_pricing_bulk_row_lock_operation ON bss.pricing_bulk_row_lock USING btree (tenant_id, bulk_operation_id)",
    "CREATE OR REPLACE FUNCTION bss.pricing_bulk_row_lock_custody() RETURNS trigger AS $$
        DECLARE
          run_tenant uuid;
          run_state  text;
        BEGIN
          IF TG_OP = 'UPDATE' THEN
            RAISE EXCEPTION
              'pricing_bulk_row_lock: a lock is taken or released, never edited; operation % may not inherit the lock on price %',
              NEW.bulk_operation_id, OLD.price_id;
          END IF;

          SELECT tenant_id, state INTO run_tenant, run_state
            FROM bss.pricing_bulk_operation
           WHERE operation_id = NEW.bulk_operation_id;

          -- No such run: the foreign key is the accurate refusal and this
          -- trigger has no opinion. Deferring keeps the key **observable** --
          -- a BEFORE trigger answers ahead of it -- and stops the tenant arm
          -- below reporting a tenancy fault for a run that does not exist,
          -- which would send a reader looking for a bug that is not there.
          IF NOT FOUND THEN
            RETURN NEW;
          END IF;

          -- The run exists; this proves it is the locking tenant's own.
          IF run_tenant IS DISTINCT FROM NEW.tenant_id THEN
            RAISE EXCEPTION
              'pricing_bulk_row_lock: operation % belongs to another tenant and may not lock this row',
              NEW.bulk_operation_id;
          END IF;

          IF run_state <> 'committing' THEN
            RAISE EXCEPTION
              'pricing_bulk_row_lock: operation % is %, and the bulk lock takes effect only on entry to committing',
              NEW.bulk_operation_id, run_state;
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_bulk_row_lock_custody BEFORE INSERT OR UPDATE ON bss.pricing_bulk_row_lock FOR EACH ROW EXECUTE FUNCTION bss.pricing_bulk_row_lock_custody()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_bulk_row_lock",
    "DROP FUNCTION IF EXISTS bss.pricing_bulk_row_lock_custody()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_bulk_row_lock (
            tenant_id         text NOT NULL,
            price_id          text NOT NULL,
            bulk_operation_id text NOT NULL,
            locked_at         text NOT NULL,
            PRIMARY KEY (tenant_id, price_id),
            CONSTRAINT fk_pricing_bulk_row_lock_operation FOREIGN KEY (bulk_operation_id) REFERENCES pricing_bulk_operation(operation_id),
            CONSTRAINT fk_pricing_bulk_row_lock_price FOREIGN KEY (price_id) REFERENCES pricing_price(price_id)
        )",
    "CREATE INDEX idx_pricing_bulk_row_lock_operation ON pricing_bulk_row_lock (tenant_id, bulk_operation_id)",
    "CREATE TRIGGER trg_pricing_bulk_row_lock_no_update BEFORE UPDATE ON pricing_bulk_row_lock FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bulk_row_lock: a lock is taken or released, never edited'); END",
    "CREATE TRIGGER trg_pricing_bulk_row_lock_only_while_committing BEFORE INSERT ON pricing_bulk_row_lock FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bulk_row_lock: the bulk lock takes effect only on entry to committing') WHERE EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.bulk_operation_id) AND NOT EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.bulk_operation_id AND state = 'committing'); END",
    "CREATE TRIGGER trg_pricing_bulk_row_lock_same_tenant_as_its_run BEFORE INSERT ON pricing_bulk_row_lock FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bulk_row_lock: the operation belongs to another tenant and may not lock this row') WHERE EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.bulk_operation_id) AND NOT EXISTS (SELECT 1 FROM pricing_bulk_operation WHERE operation_id = NEW.bulk_operation_id AND tenant_id = NEW.tenant_id); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_bulk_row_lock"];

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
