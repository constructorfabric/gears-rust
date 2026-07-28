# DB behavior audit — resource-group

<!-- Created: 2026-07-27 by Constructor Tech -->

What a systematic audit of this gear's database behavior found, how it was
found, and how to repeat it on another module. The detection tooling is meant
to be reused — this gear is the worked example, not the point.

## What was found

**Most of it is the N+1 query problem.** That is worth stating plainly rather
than dressing up: 6 of the 15 findings are textbook N+1 — a loop issuing one
statement per row where one statement would do — and 3 more are the same family
of avoidable round trips (a write followed by a re-read of the row just
written, or the same lookup performed twice). Nine of fifteen findings are
"this code talks to the database more times than it needs to". The remaining
six are transaction-boundary and error-handling mistakes.

This is the oldest mistake in ORM code, and it was generated at scale: the
per-row loops are individually reasonable-looking and only become a problem
multiplied by subtree size. `move` on a 10 000-node subtree under a
depth-10 parent issued roughly 100 000 separate `INSERT`s.

| ID | Class | Severity | Where | Status |
|----|-------|----------|-------|--------|
| RG-01 | no-tx-write | **Critical** | `membership_service.rs` `add_membership_inner` — check-then-insert on a bare connection; the PK includes `group_id`, so two concurrent first-memberships in different tenants both commit and the "one tenant per resource" invariant breaks | Fixed |
| RG-02 | no-tx-write | Medium | `type_service.rs` `delete_type` — resolve/count/delete outside any transaction | Fixed |
| RG-03 | no-retry-serializable | **Critical** | `type_service.rs` `create_type`/`update_type` — `SERIALIZABLE` without retry, so a `40001` reaches the caller raw. Live call sites: account-management's gear-init type registration, i.e. a startup path, not latent code | Fixed |
| RG-04 | n-plus-one | High | `group_repo.rs` `rebuild_subtree_closure` — one `INSERT` per closure row, `A×N` of them | Fixed |
| RG-05 | n-plus-one | Medium | `group_service.rs` `move_group_internal_impl` — `is_descendant` + `get_relative_depth` per descendant; both answers were already in the rows loaded a moment earlier | Fixed |
| RG-06 | n-plus-one | Medium-High | `group_repo.rs` `insert_ancestor_closure_rows` — one `INSERT` per ancestor | Fixed |
| RG-07 | n-plus-one | Low-Medium | `type_repo.rs` — junction rows inserted one per allowed parent/membership type | Fixed |
| RG-08 | redundant-io | Low-Medium | `group_repo.rs`/`type_repo.rs`/`membership_repo.rs` — insert discards the model it just got back, then re-reads the row by id | Partially fixed (insert paths; the `update_many` + re-read shape is left, see Deferred) |
| RG-09 | external-call-in-tx | High | `group_service.rs` — a cross-gear `types_registry` call plus JSON-Schema compilation inside a `SERIALIZABLE` transaction, repeated on every retry | Fixed |
| RG-10 | n-plus-one | High | `group_service.rs` `force_delete_subtree` — ~4 statements per node | Fixed |
| RG-11 | redundant-io | Low | `group_service.rs` — the same type resolved twice per create/update (`resolve_id` then `find_by_code`) | Fixed |
| RG-12 | n-plus-one | Medium | `type_repo.rs` `list_types` — junction reads per row, so a page of N types costs `2N+1` queries. **Read path** — the only finding not on a write path | Fixed |
| RG-13 | redundant-io | Low | `type_service.rs` — the duplicate-check loads full junction data just to test existence | Fixed |
| RG-14 | no-tx-write | Medium | `membership_service.rs` `remove_membership` — same check-then-write shape as RG-01 | Fixed |
| RG-15 | error-shape-swallowing | **Critical** | Platform-wide: repositories map every `sea_orm::DbErr` through `.to_string()` into `DbErr::Custom`, and `is_retryable_contention` only recognized `Exec`/`Query`. `transaction_with_retry` could therefore never tell a serialization failure was retryable when it surfaced inside a repository call — the retry loop was dead code on every write path in the gear | Fixed |

RG-11 through RG-15 were **not** in the original problem statement — the
detector found them. RG-15 is the one worth remembering: it was found by a
*negative control*, a test written to confirm that a correctly-retried path
behaves correctly. It did not.

## How it was found

Three mechanisms, all deterministic and CI-runnable:

- **SQL trace capture.** A `QueryRecorder` attached through SeaORM's metric
  callback records every statement with its kind, table and parameter count.
  It is attached via a `test-support`-gated toolkit-db constructor, because
  `DBProvider` deliberately does not hand out the raw connection.
- **Transaction membership.** Read from toolkit-db's task-local guard, not by
  parsing SQL: `BEGIN`/`COMMIT`/`ROLLBACK` never reach the metric callback,
  since SeaORM issues them straight through sqlx's `TransactionManager`. Gives
  `writes_outside_tx()`, which is what catches `no-tx-write`.
- **Scale invariance.** Run an operation at small and large N and compare the
  *slope*, not the offset: statement count must not grow with N. This is what
  makes N+1 detection mechanical rather than a matter of noticing a loop.
- **Static source scans** for the two classes that leave no SQL trace
  (`SERIALIZABLE` without retry; an external client call inside a transaction).
  Text heuristics, explicitly interim until a dylint late lint replaces them.
- **Real PostgreSQL races.** Barrier-synchronized task pairs on
  `testcontainers`, each ending in a post-state invariant check against the
  tables — closure agrees with `parent_id`, depths are right, no cycles, one
  tenant per resource. Checking the two callers' return values is not enough:
  both can return 200 while the closure table is corrupt.

Validation, so the detector's output means something: all 10 previously known
defects were rediscovered by general, class-based rules (no rule keyed to a
file or line); every class was additionally confirmed by injecting a synthetic
defect, watching the rule fire, and reverting; negative controls check that
read paths produce no write statements and that SSI-protected invariants are
not flagged.

Empirically SSI does hold where the design assumed it would: mutual `move`
(A→B against B→A) left the closure table intact across 21 runs with the loser
getting a clean `CycleDetected`, and force-delete races produced no orphans in
60+ runs.

## Running this audit on another module

The point of the exercise. Nothing needs copying: the recorder lives in
`toolkit_db::test_support` behind the `test-support` feature. Method:
[`14_db_behavior_testing.md`](../../../../docs/toolkit_unified_system/14_db_behavior_testing.md).

1. Add `toolkit-db` with the `test-support` feature to the gear's
   dev-dependencies and use `toolkit_db::test_support::{QueryRecorder,
   connect_with_recorder, snapshot_trace}`. The gear supplies only its own
   migrations and service wiring.
2. Write one trace test per write operation. Dump the trace
   (`DB_AUDIT_TRACE_DIR=… cargo nextest run …`) and read it once — most findings
   are visible on that first read, before any rule fires.
3. Assert `rec.writes_outside_tx().is_empty()` on every write operation.
4. Add a scale test per operation whose cost could depend on input size:
   build N=small and N=large, assert the statement count does not grow.
5. Add `rec.redundant_reads_after_write()` where writes are followed by reads.
6. Add a PostgreSQL suite behind the `integration` feature for anything whose
   correctness depends on concurrency, with a post-state invariant helper
   called from every scenario.
7. Pin each known defect as an executable `#[ignore = "known defect …"]`
   assertion so it starts passing the day it is fixed.

## What this does not cover

- **Cost of one statement.** Scale invariance proves the query's shape doesn't
  multiply; it says nothing about a single statement with a 10 000-item `IN`
  list. Parameter counts are recorded but that is a count, not a cost model.
- **Transaction duration.** Everything here counts statements. An
  in-transaction call to another gear contributes zero statements while
  plausibly dominating the transaction's wall-clock length.
- **Isolation level.** `SET TRANSACTION ISOLATION LEVEL` also bypasses the
  metric callback, so the recorder cannot tell `SERIALIZABLE` from
  `READ COMMITTED`. Downgrading a write path's isolation would pass every rule
  here.
- **Predicate correctness.** The trace shows a `WHERE tenant_id = ?` is
  present, not that the bound value is the right one. That is the AccessScope
  suite's job.
- **Constraint inventory**, `EXPLAIN`/index usage, lock-ordering deadlocks
  (`40P01` as opposed to `40001`), pool starvation under load, and migration
  drift on an existing schema — none are examined.
- **The `in_tx` probe's boundary** is `tokio::spawn`: task-locals don't cross
  it. A detached spawn that outlives the test's assertions is a genuine blind
  spot. This gear has no `spawn` in `src/`.

## Deferred

- **`TxRunner` marker and a dylint lint series** — the compile-time and
  lint-time layers that would prevent reintroduction. The static rules here are
  text heuristics standing in for them.
- **Membership ownership guard table** — a schema-level alternative to RG-01's
  fix. SSI plus retry is sufficient for correctness; the guard row would be
  stronger, opt-in hardening.
- **RG-08's `update` re-read** — `update_many` returns only a row count, so
  removing the follow-up read means restructuring the write to a read-then-
  `ActiveModel::update` shape, trading one extra read for another.
- **Two contract drifts**, both pinned as executable `#[ignore]`d tests rather
  than silently accepted: DESIGN.md promises an exhausted retry maps to
  `ServiceUnavailable` (503) where the code returns `Internal` (500), and
  promises a 5s transaction timeout that `TxConfig` has no mechanism for. Both
  are contract changes, not remediation.
- **Backoff with jitter, statement/lock timeouts, single-snapshot hierarchy
  reads, `EXPLAIN` verification** — all identified, none in this pass.
