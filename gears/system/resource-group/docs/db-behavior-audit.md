# DB behavior audit — resource-group


<!-- toc -->

- [What was found](#what-was-found)
  - [Findings added after this report](#findings-added-after-this-report)
  - [Transaction-behaviour findings (`TX-nn`)](#transaction-behaviour-findings-tx-nn)
- [How it was found](#how-it-was-found)
- [Deviation from the unit/E2E testing guide](#deviation-from-the-unite2e-testing-guide)
- [Running this audit on another module](#running-this-audit-on-another-module)
- [What this does not cover](#what-this-does-not-cover)
- [Deferred](#deferred)

<!-- /toc -->

<!-- Created: 2026-07-27 by Constructor Tech -->

What a systematic audit of this gear's database behavior found, how it was
found, and how to repeat it on another module. The detection tooling is meant
to be reused — this gear is the worked example, not the point.

## Scope of this branch

This report is the full audit. The branch carrying it fixes the **query-count**
subset only — the N+1 and redundant-I/O findings — and leaves the
transaction-boundary, retry, isolation and error-classification findings to the
branch they came from. The Status column says which is which, and the audit
suite carries only the tests that exercise what is here: a test asserting a
fix that is absent would be noise, not coverage, so those tests stay with the
branch that carries their fixes.

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
| RG-01 | no-tx-write | **Critical** | `membership_service.rs` `add_membership_inner` — check-then-insert on a bare connection; the PK includes `group_id`, so two concurrent first-memberships in different tenants both commit and the "one tenant per resource" invariant breaks | Fixed on the source branch; not in this one — a transaction boundary, not a query count |
| RG-02 | no-tx-write | Medium | `type_service.rs` `delete_type` — resolve/count/delete outside any transaction | Fixed on the source branch; not in this one — a transaction boundary, not a query count |
| RG-03 | no-retry-serializable | **Critical** | `type_service.rs` `create_type`/`update_type` — `SERIALIZABLE` without retry, so a `40001` reaches the caller raw. Live call sites: account-management's gear-init type registration, i.e. a startup path, not latent code | Fixed. Both moved to `transaction_with_retry`, each attempt on its own clones. Guarded by a Section 4 rule |
| RG-04 | n-plus-one | High | `group_repo.rs` `rebuild_subtree_closure` — one `INSERT` per closure row, `A×N` of them | Fixed |
| RG-05 | n-plus-one | Medium | `group_service.rs` `move_group_internal_impl` — `is_descendant` + `get_relative_depth` per descendant; both answers were already in the rows loaded a moment earlier | Fixed |
| RG-06 | n-plus-one | Medium-High | `group_repo.rs` `insert_ancestor_closure_rows` — one `INSERT` per ancestor | Fixed |
| RG-07 | n-plus-one | Low-Medium | `type_repo.rs` — junction rows inserted one per allowed parent/membership type | Fixed |
| RG-08 | redundant-io | Low-Medium | `group_repo.rs`/`type_repo.rs`/`membership_repo.rs` — insert discards the model it just got back, then re-reads the row by id | Fixed in all three, both halves. Inserts return what they wrote or assemble it from the values they were given; the `update_many` + re-read shape is gone too — `group_repo::update` returns `()` and `type_repo::update_type` returns only the timestamp it chose, and both callers build the response from what they wrote. `find_by_code_with_id` now hands back the row rather than just its id, so `update_type` has the immutable `created_at` without a second read |
| RG-09 | external-call-in-tx | High | `group_service.rs` — a cross-gear `types_registry` call plus JSON-Schema compilation inside a `SERIALIZABLE` transaction, repeated on every retry | Fixed. Both entry points validate before `BEGIN`; the transaction-inner functions no longer take a `TypesRegistryClient`, so the call cannot return by accident. Guarded by a Section 4 rule |
| RG-10 | n-plus-one | High | `group_service.rs` `force_delete_subtree` — ~4 statements per node | Fixed |
| RG-11 | redundant-io | Low | `group_service.rs` — the same type resolved twice per create/update (`resolve_id` then `find_by_code`) | Fixed |
| RG-12 | n-plus-one | Medium | `type_repo.rs` `list_types` — junction reads per row, so a page of N types costs `2N+1` queries. **Read path** — the only finding not on a write path | Fixed |
| RG-13 | redundant-io | Low | `type_service.rs` — the duplicate-check loads full junction data just to test existence | Fixed |
| RG-14 | no-tx-write | Medium | `membership_service.rs` `remove_membership` — same check-then-write shape as RG-01 | Fixed on the source branch; not in this one — a transaction boundary, not a query count |
| RG-15 | error-shape-swallowing | **Critical** | Platform-wide: repositories map every `sea_orm::DbErr` through `.to_string()` into `DbErr::Custom`, and `is_retryable_contention` only recognized `Exec`/`Query`. `transaction_with_retry` could therefore never tell a serialization failure was retryable when it surfaced inside a repository call — the retry loop was dead code on every write path in the gear | Fixed. `is_retryable_contention` matches `DbErr::Custom` on the same message patterns, with five regression tests. Without it the jittered backoff added alongside these fixes would never have fired |

RG-11 through RG-15 were **not** in the original problem statement — the
detector found them. RG-15 is the one worth remembering: it was found by a
*negative control*, a test written to confirm that a correctly-retried path
behaves correctly. It did not.

### Findings added after this report

The table above is the first pass. A second N+1 pass and the tenant-scope
work added two findings that code comments reference by letter rather than by
`RG-NN`, so a reader arriving from the code will not find them above:

| ID | Kind | Where | Status |
|----|------|-------|--------|
| (a) | redundant-io | `group_repo.rs` `find_model_by_id_scoped` — the membership tenant gate needed a scoped single-row read; reusing the unscoped `find_model_by_id` plus a separate scope check would have cost an extra statement per call (`repo.rs`, `membership_service.rs`) | Fixed on the source branch; not in this one — it serves the membership tenant gate |
| (b) | n-plus-one | `type_repo.rs` — one `resolve_id` per value while resolving a type filter, and one violation lookup per candidate parent in the hierarchy safety check (slope 2.0) | Fixed: `collect_type_filter_paths` + a single `WHERE schema_id IN (...)`, and `find_groups_violating_removed_parents` |

| (c) | n-plus-one | `group_service.rs` `classify_children_for_delete` — the non-force delete rejection resolved each blocking child's type path with its own `SELECT`, memoized per `gts_type_id`, so the cost grew with the number of *distinct* child types. Landed after this report, in the unfinished `name blocking children on delete` commit | Fixed on the source branch; not in this one — the code it fixes arrived with an unrelated feature |

Either give these `RG-16`/`RG-17` numbers or keep this table; what must not happen
is a code comment pointing at an audit that does not mention the finding.

Finding (c) is worth reading as a statement about the method rather than about
the code. The detector did not miss it — nothing was watching. Of the ten scale
tests, none covered `delete_group(force = false)`: the nine written during the
audit covered create, move, force delete, the two `$filter` paths and the type
operations, and the rejection path had no operation-level test at all. A defect
introduced there afterwards was invisible by construction. The tenth test now
exists and was validated the same way as the rest — the loop was restored, the
test went from 17 statements at N=12 against 7 at N=2 to a clean failure, and
the batch version brought both back to equal.

### Known and not fixed: `IN (...)` lists bounded only by a column's domain

Every list-valued predicate whose length follows the *data* or the *request*
is now chunked against `max_bind_params_for`. Two are chunked against nothing,
and are recorded here rather than fixed:

- `group_repo.rs` `resolve_type_paths_batch` and `type_repo.rs`
  `load_full_types_batch` bind one parameter per distinct GTS type id. The
  column is `SMALLINT`, so the list cannot exceed 32 767 — which is above
  SQLite's 30 000 ceiling and below PostgreSQL's 60 000.

Reaching it needs more than 30 000 distinct types registered in one
deployment, at which point the failure is a driver error on a read path, not
data loss. The fix is the same three lines used everywhere else; what makes it
not worth landing blind is that no test can reach the boundary without
building 30 000 rows. If the type count ever gets within an order of magnitude
of that, chunk these two the same way.

Not on this list: `build_hierarchy_page`, which *is* bounded by the data
(subtree size) and is chunked — at half the ceiling, because the caller's
`AccessScope` compiles to predicates binding an unknown number of parameters
into the same statement.

### Transaction-behaviour findings (`TX-nn`)

A companion pass reviewed every write transaction in the gear — isolation level, retry budget,
external calls inside the transaction, and whether the declared invariant actually needs
`SERIALIZABLE`. Its conclusion was that nothing in the branch sat in the "must fix" class: no
unprotected write-skew and no cross-gear call inside a transaction remained. Three items were
"worth doing", and all three were done. They are recorded here because the code cites them.

| ID | Finding | Why `SERIALIZABLE` was not the right tool | Status |
|----|---------|-------------------------------------------|--------|
| TX-01 | `TypeRepository::insert` did not classify `is_unique_violation()`, unlike `GroupRepository` and `MembershipRepository` | The invariant is `UNIQUE(schema_id)`, held by the schema at every isolation level. Only the *error shape* depended on `SERIALIZABLE`: without the mapping, a duplicate code surfaced as a raw database error unless an SSI abort happened to retry into a clean one. | Fixed on the source branch; not in this one — isolation, not a query count |
| TX-02 | `update_group` always opened `SERIALIZABLE`, including for a pure rename | The predicates that need SSI — cycle detection, depth and width limits, closure rebuild — are reachable only when `parent_id` changes. | Fixed on the source branch; not in this one — isolation, not a query count |
| TX-03 | `remove_membership_in_tx` did not need `SERIALIZABLE` | It deletes by the exact composite primary key `(group_id, gts_type_id, resource_id)` — no predicate a concurrent writer can invalidate. The commit that introduced the transaction documented the level as "for symmetry" with `add_membership`, not as a correctness requirement. | Fixed on the source branch; not in this one — isolation, not a query count |

Bounded retry is kept on every downgraded path. Lowering the isolation level removes SSI aborts
(`40001`), not lock-ordering deadlocks (`40P01`), and `is_retryable_contention` treats both alike.

Two further observations from that pass, neither acted on:

- `check_hierarchy_safety` inside `update_type_in_tx` loops over the removed allowed-parent types
  with three queries per iteration. It scales with the size of the *request*, not of the database,
  so it is not in the N+1 class above — but a very long list holds a `SERIALIZABLE` transaction open
  proportionally longer. Low risk in practice: type definitions are administered by hand.
- `11_database_patterns.md` documents transactions exclusively through
  `SecureConn::in_transaction_mapped`, which has neither retry nor a configurable isolation level.
  The `Db` / `TxConfig::serializable()` / `transaction_with_retry` path this gear uses — and so do
  `ledger` and `account-management` — is absent from the guide, which therefore misleads anyone
  writing a new gear against it. This is a gap in the guide, not a deviation by the gear.

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

## Deviation from the unit/E2E testing guide

**None of the PostgreSQL suites below are in this branch.** This branch
carries the query-count half of the audit — SQLite plus the statement
recorder — and the concurrency findings reported above were measured on the
branch that carries the isolation half. The rationale is recorded here
because it is the audit's, not that branch's, and the suites land against it.

`pg_concurrency_test.rs` (this audit's own suite), and the three narrower
repro suites that followed it — `pg_membership_filter_test.rs` and
`pg_group_filter_test.rs` (both in this gear) and
`secure_group_scope_postgres.rs` (in `libs/toolkit-db`) — are Rust
`#[tokio::test]`s that talk to a real PostgreSQL via `testcontainers`. That
is a deliberate, written-down deviation from the general testing guide:
[`12_unit_testing.md`](../../../../docs/toolkit_unified_system/12_unit_testing.md)
routes PostgreSQL-specific behavior (FK `RESTRICT`, `SERIALIZABLE`
isolation, domain types) to E2E, and
[`13_e2e_testing.md`](../../../../docs/toolkit_unified_system/13_e2e_testing.md)
defines E2E as pytest against a running `cf-gears-server`. None of these
four suites are that: they call repository/service code directly,
in-process, no HTTP, no running server.

Why the deviation stands:

- **Precedent, not improvisation.** This audit (`pg_concurrency_test.rs`)
  established the pattern first: a `--features integration` suite, a
  dedicated `test-rg-pg` `Makefile` target, and a matching step in
  `.github/workflows/ci.yml`'s `integration` job
  ("Test resource-group (pg concurrency, integration)", `RG_PG_REQUIRE_DOCKER=1`).
  The three later suites reuse that exact wiring rather than inventing a
  new one.
- **Feature-gated, not part of the default suite.** All four are behind
  `#![cfg(feature = "integration")]` (`secure_group_scope_postgres.rs`
  additionally requires `feature = "pg"`); `cargo nextest run -p
  cf-gears-resource-group` with no `--features integration` neither builds
  nor runs them. They do not count against the unit-testing guide's "full
  suite < 5s" target — that target is measured without `--features
  integration`.
- **A diagnosis pytest cannot give.** Each of the three narrower suites
  reproduces a real Postgres-dialect rejection —
  `pg_membership_filter_test.rs` / `pg_group_filter_test.rs`: "operator
  does not exist: smallint = text" (comparing the wire-level GTS
  type-path string against the SMALLINT `gts_type_id` column);
  `secure_group_scope_postgres.rs`: "operator does not exist: uuid = text"
  (the `uuid = text` defect, comparing a `TEXT` membership column against a
  `Uuid` resource column). A pytest test hitting the same regression
  through HTTP would only ever observe the resulting 500 status code —
  these tests surface, in the failure message should the guard ever
  regress, the actual backend error text that an HTTP status code
  swallows.

This is scoped narrowly: it covers this specific pattern (dialect-rejection
reproduction, plus the concurrency harness above), not a general license to
write Rust-level Postgres tests instead of E2E. New PostgreSQL-dependent
behavior should still default to E2E per the guide unless it has the same
shape — a bug invisible above the SQL layer that would otherwise surface
only as an opaque HTTP status code.

## Running this audit on another module

The point of the exercise. Nothing needs copying: the recorder lives in
`toolkit_db::test_support` behind the `test-support` feature. Method:
[`16_defect_class_to_control_map.md`](../../../../docs/toolkit_unified_system/16_defect_class_to_control_map.md).

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
7. Pin each known defect as an executable assertion, so that fixing it breaks
   the pin and the fix cannot land silently. `#[ignore = "known defect …"]`
   works when the defect is observable as a statement count. When it is not —
   `SERIALIZABLE` without retry is the case here — assert the count that *is*
   there rather than the count that ought to be, and say in the doc comment
   what to change when the fix lands. See
   `static_rule_pins_type_service_serializable_without_retry`.

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
  here. This blind spot was exercised after the audit: two write paths were
  lowered to the backend default, and an independent review later found a
  lost-update race between the group update and the group move that the rules
  here could not have caught. Isolation belongs to the concurrency suite, not
  to statement counting — and a concurrency suite only covers the pairs it
  enumerates, so a new write path needs a new pair.
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
- **Two contract questions**, both pinned as executable `#[ignore]`d tests rather
  than silently accepted. They were written as drifts against DESIGN.md; since
  then DESIGN.md has been corrected to describe what the code actually does, so
  what remains is the open question of what the contract *should* be: whether an
  exhausted retry deserves a dedicated status (the code returns 500 through
  `Internal`, and there is no `ServiceUnavailable` variant to return), and
  whether a transaction timeout should exist at all (`TxConfig` has no
  mechanism). The first is entangled with a wider question — the platform guide
  requires fail-closed 403 for an unreachable PDP, DESIGN said 503, the code
  returns 500 — so it belongs to an error-taxonomy pass, not to this one.
- **Statement/lock timeouts, single-snapshot hierarchy reads, `EXPLAIN`
  verification** — identified, not addressed in this pass. Backoff was on this
  list and has since been implemented: jittered exponential, base 2 ms, factor
  5, capped at 100 ms, in `toolkit-db`'s retry helper. The immediate-retry loop
  it replaced turned contention into a thundering herd.
