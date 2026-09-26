# DB Behavior Testing & Audit Guide

<!-- Created: 2026-09-24 by Constructor Tech -->

<!-- toc -->

- [Why and when](#why-and-when)
- [Defect catalog](#defect-catalog)
  - [Check-then-act outside a transaction (TOCTOU)](#check-then-act-outside-a-transaction-toctou)
  - [CAS without checking `rows_affected`; lost update](#cas-without-checking-rows_affected-lost-update)
  - [External effects inside a transaction; events not in the same transaction as the write](#external-effects-inside-a-transaction-events-not-in-the-same-transaction-as-the-write)
  - [Non-idempotent steps under bounded retry](#non-idempotent-steps-under-bounded-retry)
  - [N+1, query-in-loop, unchunked `IN (...)`, missing `LIMIT`](#n1-query-in-loop-unchunked-in--missing-limit)
  - [Non-deterministic `ORDER BY` under offset pagination](#non-deterministic-order-by-under-offset-pagination)
  - [Missing indexes for hot queries and FK columns](#missing-indexes-for-hot-queries-and-fk-columns)
  - [`COUNT` used to check existence](#count-used-to-check-existence)
  - [SQLite / PostgreSQL / MySQL divergence](#sqlite--postgresql--mysql-divergence)
  - [Migration hazards](#migration-hazards)
  - [Deadlocks and lock ordering](#deadlocks-and-lock-ordering)
  - [Mappers defaulting instead of erroring](#mappers-defaulting-instead-of-erroring)
  - [Tests that prove nothing](#tests-that-prove-nothing)
- [How to run an audit](#how-to-run-an-audit)
- [Where to keep which test](#where-to-keep-which-test)
- [Audit report template](#audit-report-template)
- [Related documents](#related-documents)

<!-- /toc -->

This document defines **DB-behavior testing** — a third test layer, alongside unit tests
([`12_unit_testing.md`](12_unit_testing.md)) and E2E tests ([`13_e2e_testing.md`](13_e2e_testing.md)) —
and the method for auditing a gear's database code for it. A unit test asks *"is the result correct?"* An
E2E test asks *"does the seam work end-to-end, happy path?"* This layer asks *"how did the code talk to
the database to get there, and does that survive a second concurrent caller?"* A check-then-insert with no
transaction returns the right answer every time it's called once, and corrupts data only when two callers
overlap — unit tests, which call one thing at a time, are structurally blind to that; E2E tests are built
for zero-flake happy-path stability and must not grow a barrier and a race. Neither layer can catch this
class of defect by design — this document is where it lives instead.

[`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md) answers *which control catches a
given class of defect* across a whole gear (tenant bypass, missing gate, N+1, write-skew, …). This document
specializes its "N+1 and query-count regressions" and "concurrency and write-skew" rows for the database
axis specifically: SeaORM / toolkit-db's secure ORM, against PostgreSQL, SQLite, and occasionally MySQL.

This is a method and a catalog, not a report. Gear-specific findings — which defects a given gear has, how
severe, whether fixed — live in that gear's own audit report (see [the template](#audit-report-template)
and the worked example,
[`gears/system/resource-group/docs/db-behavior-audit.md`](../../gears/system/resource-group/docs/db-behavior-audit.md)).

## Why and when

Run this audit — in full, or as a targeted pass over the affected write paths during code review — when
one of these is true:

- **A new table or write path is added.** Every mutating operation needs a transaction map (see
  [How to run an audit](#how-to-run-an-audit)) before it ships, not after a bug report.
- **A check-then-act or CAS sequence is touched.** Anything shaped "read a predicate, decide, then write"
  is a candidate for [the first class below](#check-then-act-outside-a-transaction-toctou), regardless of
  how obviously correct the single-threaded case looks.
- **A concurrent or background operation is added or changed** — a cleanup sweep, a lease/lock takeover, a
  retry loop, anything with more than one plausible caller touching the same rows.
- **A migration changes a constraint, column type, or the shape of a table** that already has rows in any
  deployed environment — see [Migration hazards](#migration-hazards).
- **A hierarchy, batch, or list operation is added** whose cost could depend on data size rather than
  request size — the [N+1 class](#n1-query-in-loop-unchunked-in--missing-limit) is the single most common
  finding in every audit run so far.
- **An isolation level, retry wrapper, or error-mapping layer is touched.** These are invisible to ordinary
  review: the regressed code still compiles, still returns the right answer on a sequential call, and often
  still passes every existing unit test.
- **Symptom-driven**: a lost update, a duplicate row after a retry, an orphaned blob or child row nobody
  can find again, a flaky concurrency-adjacent test, or an incident whose root cause was a query issued
  outside a transaction.

## Defect catalog

Each class below states what it is, a general scenario (not tied to any one gear's code), how to find it
in review, how to catch it with a test, and the typical fix. "Seen in" at the end of a class points to a
worked example — read the cited document for the specifics, this catalog stays gear-agnostic on purpose.

### Check-then-act outside a transaction (TOCTOU)

**What it is.** A read establishes a fact ("no membership of this pair exists yet", "this group has no
children", "this file has no other versions"), a decision follows from it, and a write acts on that
decision — with the read and the write on a bare connection, or in two separate transactions. A concurrent
caller can commit between the read and the write and invalidate the fact the write still assumes. Looks
like: "resolve, count, then delete"; "check no row exists, then insert"; "list children, if empty then
delete the parent".

**How to find it.** Grep for a `SELECT`/`find()` immediately preceding an `INSERT`/`UPDATE`/`DELETE` in the
same function with no `in_transaction_mapped`/`transaction_with_retry` wrapping both. Ask, for every hit:
*what does a second caller running the identical sequence, interleaved one statement at a time, leave in
the tables?*

**How to catch it with a test.** SQLite unit test with the query recorder
(`toolkit_db::test_support`) attached: assert `rec.writes_outside_tx().is_empty()`, and
`rec.untransacted_read_modify_write().is_empty()` for the specific shape (a read of table `T` followed by a
write to `T` with nothing transactional between them). Neither catches the *race* itself, only that the
shape exists — prove the race is real with a barrier test against PostgreSQL (two tasks synchronized on a
`tokio::sync::Barrier`, both starting at the same instant) asserting the post-state invariant, not the two
callers' return values: both can return success while the invariant is broken.

**Typical fix.** Wrap the read and the write in one transaction. If the invariant is a write-skew hazard —
two transactions each read a predicate the other's write invalidates — the fix is `SERIALIZABLE` plus a
retry wrapper, not a lower isolation level "for symmetry"; only the read/write pair whose predicate is
actually shared needs it. If the shape is "delete the parent only if it currently has no children" under
`READ COMMITTED`, a plain `DELETE ... WHERE NOT EXISTS (...)` is **not** enough even inside a transaction —
see [`11_database_patterns.md`'s "Row locks"](11_database_patterns.md#row-locks-select--for-update) for why
the subquery doesn't re-evaluate against a concurrent insert landing in the gap, and for the
`SELECT ... FOR UPDATE` pattern that closes it (lock the parent first, same order at every call site).

*Seen in*: resource-group's `db-behavior-audit.md` (RG-01, the tenant-membership write-skew, and its
`TX-nn` isolation pass); file-storage's `concurrency-and-failure-model.md` race catalog (delete-vs-
concurrent-version and orphan-reclaim-vs-concurrent-insert are exactly this shape, fixed with the row lock).

### CAS without checking `rows_affected`; lost update

**What it is.** A conditional `UPDATE ... WHERE <expected state>` (or `DELETE`) is the right pattern for a
state transition guarded by a predicate — but only if the caller checks how many rows it actually changed;
an unchecked CAS that silently affects zero rows looks identical, from the return value, to one that
succeeded. A related but distinct failure: an update that reads a whole row, changes one field, and writes
every field back — a concurrent writer's change to a *different* field is silently overwritten (lost
update), even though no isolation level was violated and no predicate was wrong. Looks like:
`update_many().filter(...).exec(conn)` with the result discarded; a service that reads a struct, mutates
one field, and calls `.update()`/`.save()` on the whole `ActiveModel` instead of setting only the touched
columns.

**How to find it.** For every CAS: is `rows_affected` checked and turned into the right domain outcome
(conflict, not-found, stale) rather than treated as success either way? For every update: does it write
only the columns that changed, or does it write back fields it merely read?

**How to catch it with a test.** Unit test on SQLite: call the CAS twice with the same expected-state
precondition; the second call must observe zero rows affected and return the domain error. For lost update:
two sequential updates to *different* fields of the same row must both be visible afterward — assert the
field the second update didn't touch still holds what the first set, via a direct entity query (12's
"Direct DB assertions"). For the concurrent case, a barrier test against PostgreSQL with two writers
touching disjoint fields proves the write-set is actually disjoint, not merely narrow-looking in one path.

**Typical fix.** Check `rows_affected`; map zero to the specific domain outcome the caller needs
(`Conflict`, `NotFound`, `StaleVersion`) instead of `Ok(())`. For lost update, narrow the `UPDATE`'s column
list to exactly what changed, or fence the row with an optimistic-concurrency column checked in the
`WHERE`, so a concurrent writer is rejected rather than silently merged.

*Seen in*: file-storage's CAS methods (`bind_content_cas`, `touch_meta`, lease acquire/release) check
`rows_affected` everywhere; its one known gap (a CAS predicate missing a lease-owner fence, compensated at
the domain layer by a deterministic-convergence argument) is a pinned, non-`#[ignore]`d known-defect test
in `tests/db_behavior_audit_test.rs`. Resource-group's audit (see its "What this does not cover") records a
lost-update race between a group update and a group move found after both were lowered off `SERIALIZABLE` —
exactly the disjoint-write-set property this class checks.

### External effects inside a transaction; events not in the same transaction as the write

**What it is.** Network I/O, another gear's client call, JSON-Schema compilation, or any non-DB work runs
while a DB transaction is open — extending its duration and, at `SERIALIZABLE`, its conflict window,
without the external call being part of what the transaction protects. The opposite failure: an event or
audit record that *should* be part of the same atomic unit as the state change is written outside the
transaction instead — a crash between the two leaves a state change with no matching event, or an event for
a change that rolled back. Looks like: a service method calling a cross-gear SDK client or a
policy/schema resolver from inside a `transaction_with_retry` closure (every retry re-runs the call too);
or `store.write(...)` followed, outside any transaction, by `events_repo.enqueue(...)`.

**How to find it.** For the "external call inside": grep the transaction closure's body (and everything it
calls) for a parameter typed as an external client trait object (`&dyn SomeClient`/`Arc<dyn SomeClient>` —
the idiomatic injected-dependency shape) reachable from inside it; a whole-file scan isn't enough, since
sibling functions legitimately hold that type outside a transaction. For "event not in the same
transaction": confirm the audit/outbox emit is one more repository call inside the *same* transaction as
the state change, not a follow-up call after `COMMIT`.

**How to catch it with a test.** Static-scan version: a source-text rule matching the external-client type
shape inside the closure's text, with a negative control against a correctly-written transaction in the
same codebase (a rule that always fires teaches nothing). Dynamic version: `rec.all_in_one_transaction()`
on the write-plus-event trace — it compares `tx_id` across every `in_tx` statement, catching the event
write landing in a second, sequential transaction even though both individually look transactional.

**Typical fix.** Move the external call before `BEGIN` (validate first, open the transaction with only the
values it produced) or after `COMMIT` if it's a notification tolerant of being sent for an already-durable
change. For events/audit: put the `INSERT` into the outbox table inside the same transaction as the state
change — a transactional outbox: the event becomes visible iff the state change committed, and delivery is
a separate, idempotent, at-least-once concern outside the transaction entirely.

*Seen in*: resource-group's RG-09 (a cross-gear types-registry call plus JSON-Schema compilation inside a
`SERIALIZABLE` transaction, repeated on every retry). File-storage's structural fix for the same class —
only the storage layer ever holds a transaction handle, the domain layer has no way to reach one — makes
the mistake unrepresentable by construction, stronger than catching it after the fact; every write-plus-
audit-plus-event triple in that gear already lives in one transaction for exactly this reason.

### Non-idempotent steps under bounded retry

**What it is.** A bounded-retry helper (`transaction_with_retry`/`transaction_with_bounded_retry`) is the
right response to a retryable contention error (`40001` serialization failure, `40P01` deadlock) — but only
if every statement inside the retried closure is safe to run again from scratch. Two ways this goes wrong:
(1) the retry helper re-runs the **whole closure**, including a step that already had an externally visible
partial effect (a non-transactional side effect, or a second, independent transaction nested in the first
attempt) — the retry then repeats it. (2) A `SERIALIZABLE` transaction is opened directly, with no retry
wrapper, so an expected `40001` abort reaches the caller as a raw database error instead of a clean, retried
outcome — a wiring gap invisible to any unit test, since nothing sequential ever produces a `40001`. Looks
like: `db.transaction_ref_mapped_with_config(TxConfig::serializable(), ...)` next to sibling operations in
the same file that correctly use `db.transaction_with_retry(...)`.

**How to find it.** Grep both method names per file; a bare `SERIALIZABLE` open next to sibling operations
using the retry helper is the tell. For non-idempotent retry: does anything inside the closure durably
commit *outside* its own transaction, or call an external system non-idempotently, such that re-running it
repeats the effect?

**How to catch it with a test.** Static source-scan counting the unretried vs. retried method name per
file, with a negative control against a correctly-written sibling. Separately, confirm the error path is
retryable end-to-end: a contention error must survive every `map_err`/`From` conversion between the driver
and the retry classifier with its *type* intact, not stringified — a classifier recognizing only
`DbErr::Exec`/`DbErr::Query` returns `false` unconditionally for a repository's `DbErr::Custom(message)`, so
the retry loop can be present and correctly wired and still never fire. Cover this with a round-trip test:
construct the real error value the repository layer produces for a contention error, run it through the
actual `map_err` chain, and assert the classifier still says yes.

**Typical fix.** Wrap every `SERIALIZABLE` (and any transaction whose failure mode includes
`40001`/`40P01`) in the retry helper. Preserve the error's structured type through every mapping layer —
match on variants, not `.to_string()`. Keep anything non-idempotent outside the retried closure entirely.

*Seen in*: resource-group's RG-03 (`SERIALIZABLE` without retry on a startup path) and RG-15
(`error-shape-swallowing`: the retry helper was present and correct, and a platform-wide `.to_string()`
mapping made it dead code on every write path until a negative-control test caught it).

### N+1, query-in-loop, unchunked `IN (...)`, missing `LIMIT`

**What it is.** An operation over N related rows — a subtree, a batch of junction rows, a page of results —
issues one statement per row instead of one batched statement for the whole collection: `O(N)` (or
`O(A×N)` for a hierarchy with `A` ancestors) instead of `O(1)`. Two related mistakes: an `IN (...)` list
bounded only by the *data* rather than a fixed chunk size, so a large enough dataset exceeds the driver's
bind-parameter limit; and a `SELECT` with no `LIMIT` on a table with no natural upper bound. Looks like: a
`for` loop over rows just fetched issuing one `INSERT`/`SELECT` per iteration where a batched statement
would do; a write immediately followed by a re-read of the row it just wrote (same family — an avoidable
round trip, found the same way).

**How to find it.** Any loop whose body issues a repository call is a candidate — bounded by the *request*
(fine) or by the *data* (an N+1)? For every list-valued predicate built from a collection, confirm it goes
through the project's bind-parameter chunking helper (`max_bind_params_for` or equivalent), at a value below
the true driver limit with headroom for the query's other predicates. For every unbounded `SELECT`, confirm
a `LIMIT`/keyset bound exists — including "background sweep" reads, which are often also reachable from a
live request path.

**How to catch it with a test.** Scale-invariance: run the identical operation at a small N and a much
larger N (far enough apart that a linear term is unmistakable, e.g. N=3 vs N=15), group statements by
`(kind, table)` via `rec.stats()`, and assert `small == large` directly — not "not too large". Budget
`rec.total_params()` separately: a chunked `IN (...)` keeps statement count flat while parameter count
still scales with N. `rec.redundant_reads_after_write()` catches "insert, discard the model, re-read by
id" directly. Caveat: without `RETURNING` support, SeaORM's SQLite `insert()` issues an implicit re-`SELECT`
per row as a fallback — inflates absolute redundant-read counts by a constant offset (invisible on
PostgreSQL), but doesn't affect a scale-invariance verdict.

**Typical fix.** One batched `INSERT ... VALUES (...), (...), ...` or `WHERE col IN (...)`, chunked against
the bind-parameter budget when the list length follows the data. Add `LIMIT`/keyset pagination to any
unbounded read, sweeps included. Return what a write already gave back instead of re-reading it.

*Seen in*: resource-group's `db-behavior-audit.md` — most findings (RG-04 through RG-13) are this class: 6
textbook N+1s and 3 redundant round trips of 15 total, plus a known, deliberately-unfixed unchunked
`IN (...)` bounded only by a column's domain type. File-storage's index/N+1 pass found the same shape in a
cleanup sweep (per-candidate lookups that should have been one batched `list_by_ids` call) after the gear
had already eliminated it from every client-facing hot path.

### Non-deterministic `ORDER BY` under offset pagination

**What it is.** A paginated list query sorts by a non-unique column (typically a timestamp) and paginates
with `LIMIT`/`OFFSET` without a second, unique tie-breaker in the `ORDER BY`. When two rows share the
sorted value, the database doesn't guarantee the same relative order across two queries with different
`OFFSET`s — a row can be duplicated across a page boundary, or skipped, with no error and no client-visible
signal. Looks like: `.order_by_desc(Column::CreatedAt).limit(n).offset(m)` alone, on a table where two rows
plausibly share a `created_at` (a burst of creates, or coarse timestamp precision).

**How to find it.** Grep every `.offset(` call's paired `.order_by*` clause: is there a second, unique
column? A useful cross-check: does the *same file* already add a tie-breaker for a different query (often
a background sweep) — inconsistent use of a known-good pattern within one file signals an oversight.

**How to catch it with a test.** Unit test on SQLite: insert several rows with an identical sort-column
value (forcing the ambiguous case directly), page through, and assert the full set of ids across all pages
equals the inserted set with no duplicates and no omissions — the same assertion 13's cursor-pagination
pattern uses.

**Typical fix.** Add a second, unique `order_by` column (typically the primary key), matching sort
direction, and extend the covering index with it as the trailing key so the tie-breaker doesn't turn an
index-only sort into a separate sort step.

*Seen in*: file-storage's list/versions endpoints, found by index review — the same file's own sweep
queries already used the pattern correctly, making the omission on the two paginated endpoints a clear
inconsistency rather than a considered choice.

### Missing indexes for hot queries and FK columns

**What it is.** A query on a hot endpoint, or an FK column with no index of its own, has no covering
index — forcing a sequential scan, or forcing a cascading `DELETE` on the referenced table to scan the
*entire* referencing table to find rows to cascade. A subtler variant: a partial index exists but its
predicate doesn't textually match what the query filters on, so the planner never picks it. Looks like: a
`WHERE`/`ORDER BY` with no index whose leading columns match the equality predicates and trailing column
matches the sort; a child table's FK column with no index at all; a partial index built `WHERE status =
'x'` next to a query filtering `WHERE status IN ('x', 'y')` — never selected, since its predicate doesn't
subsume the query's.

**How to find it.** Build the "query → serving index → hot or background path" table from
[How to run an audit](#how-to-run-an-audit) for every repository method. For each row: does an index exist
with matching leading (equality) then trailing (sort) columns? Is every FK column independently indexed?

**How to catch it with a test.** Mostly invisible to a statement-count recorder — the statement runs
exactly once either way, only its cost differs. Where an `EXPLAIN`-capable PostgreSQL integration harness
exists, assert an index scan, not a sequential scan. Absent that, this is a manual review question backed
by `EXPLAIN ANALYZE` on production-scale data, not a mechanized rule — record the gap explicitly.

**Typical fix.** Add the missing index in a new migration, column order matching the predicate. Index
every FK column independently. When a partial index doesn't match a query, loosen the query or add a
second index shaped for it.

*Seen in*: resource-group's audit lists index/`EXPLAIN` verification under "what this does not cover" —
deliberately deferred. File-storage's index review found a composite index missing its trailing tie-breaker
column (interacting with [the pagination class above](#non-deterministic-order-by-under-offset-pagination))
and confirmed several FK columns that used to force full-table scans on cascade delete were closed by a
dedicated index-hardening migration.

### `COUNT` used to check existence

**What it is.** `SELECT COUNT(*) FROM t WHERE ...` used to answer "does at least one row match" is slower
than it needs to be — the database can scan and count every matching row instead of stopping at the first.
This project's convention is stricter: **`COUNT` queries are forbidden outright**, since pagination here is
cursor-based, not offset/total-based, so there is rarely a legitimate reason to need a total count at all.
Looks like: `Entity::find().filter(...).count(conn).await? > 0` as an existence check; `.count(conn)`
computing a "total" for a response a cursor-paginated API shouldn't return in the first place.

**How to find it.** Grep for `.count(` in repository code. Is it answering "exists" (replace with
`.filter(...).limit(1).one(conn)` and check `Option::is_some()`) or "total for a paginated response" (the
API shouldn't expose one at all under cursor pagination — see
[`07_odata_pagination_select_filter.md`](07_odata_pagination_select_filter.md))?

**How to catch it with a test.** Static source-scan for `.count(`, allow-listing only a genuine aggregate
the caller needs. Pair with a functional test asserting the existence check's normalized SQL contains a
`LIMIT`, not a bare `COUNT(*)`.

**Typical fix.** Replace with `.filter(predicate).limit(1).one(runner)`, branch on `Option`.

*Seen in*: file-storage's index review confirmed every existence-style check in scope already uses
`LIMIT 1` (`has_active_for_file`), not `COUNT`, citing it as the positive example for this class.

### SQLite / PostgreSQL / MySQL divergence

**What it is.** A single code path behaves differently across backends in ways SQLite's permissiveness
hides during unit testing and PostgreSQL/MySQL enforce in production. Four recurring sub-cases:

- **Locking.** `.lock(LockType::Update)` renders a real `FOR UPDATE` on PostgreSQL/MySQL but is a literal
  no-op on SQLite (no row-lock support in that backend) — a race test for a lock-dependent pattern passes
  on SQLite for the wrong reason (single-writer serialization) and proves nothing about the actual lock.
  See [`11_database_patterns.md`'s Row locks section](11_database_patterns.md#row-locks-select--for-update).
- **Error classification.** A unique-violation, FK-violation, serialization failure or deadlock must be
  read from the error's *structured* shape (an error code, a typed variant), not the message text, which
  varies by backend and locale — exactly what the [previous class](#non-idempotent-steps-under-bounded-retry)'s
  `error-shape-swallowing` failure mode destroys before it reaches the classifier.
- **Types.** UUID/JSON/domain types compare and constrain differently: SQLite silently accepts a UUID
  compared against `TEXT`; PostgreSQL raises `operator does not exist: uuid = text` for the same predicate.
  Plain `json` (not `jsonb`) has no equality operator on PostgreSQL, breaking a naive `SELECT DISTINCT`.
- **`CHECK`/DDL.** A constraint widened or narrowed after rows exist behaves differently per backend —
  SQLite requires a table rebuild rather than an in-place `ALTER`; see [Migration hazards](#migration-hazards).

**How to find it.** For every `.lock(...)` call, confirm the test states it only proves anything against
PostgreSQL. For every error-mapping `match`, confirm it matches the driver's typed variant or SQLSTATE code,
never `.to_string()` contents. For every UUID/JSON/domain-typed predicate, confirm a real-engine test
exercises it — this class is invisible on a permissive engine by definition.

**How to catch it with a test.** An executing test against the real target engine for every such predicate
— this is [`16_defect_class_to_control_map.md`'s "wire ↔ storage type mismatch"
row](16_defect_class_to_control_map.md#wire--storage-type-mismatch): a loosely typed backend silently
returning an empty page where production raises an error is the canonical failure here, and only a
real-engine test tells "no rows matched" from "the query itself would not run" apart.

**Typical fix.** Structured error matching everywhere; explicit casts/matching column types on both sides
of a comparison; route anything lock-dependent or type-divergent to a PostgreSQL-backed test.

*Seen in*: resource-group's RG-15 (`error-shape-swallowing`) and its Row-locks documentation in
`11_database_patterns.md`. File-storage's migration review manually confirmed a SQLite table-rebuild
migration correctly evacuates and restores data under `PRAGMA foreign_keys=ON`, with JSON-typed columns
carrying regression tests confirming the entity type still matches the migration's declared column type.

### Migration hazards

**What it is.** A family of mistakes specific to schema evolution rather than steady-state queries:

- **Editing an already-shipped migration** instead of adding a new one — anyone on the old version now has
  a schema disagreeing with anyone on the edited one, with no migration step to reconcile them.
- **SQLite table rebuild and cascades.** SQLite can't `ALTER` many constraint changes in place; the
  workaround (create-new-table / copy-data / drop-old / rename) done carelessly under
  `PRAGMA foreign_keys = ON` lets dropping the old table implicitly cascade into a child table before its
  data is safely evacuated — verify child rows survive the drop, not just that the migration runs.
- **A `down()` that doesn't actually roll back.** A documented no-op `down()` (common where SQLite
  historically can't `DROP COLUMN`) is acceptable **only when documented as such** — undocumented, it reads
  as a working rollback and silently isn't one.
- **A constraint that fails on existing data under a rolling deploy.** Adding `NOT NULL`/`CHECK`/FK that
  some already-committed row violates breaks the migration against a populated table — expand/contract (add
  nullable → backfill → validate → tighten, e.g. `NOT VALID` then a separate validate) avoids a migration
  that only ever worked against an empty test database.

**How to find it.** Never edit a shipped migration — add a new one. For a SQLite rebuild, trace what
happens to every child table under `PRAGMA foreign_keys = ON` at the moment the old table drops. Does every
`down()` actually undo `up()`, or is it an undocumented no-op? For every new constraint, what happens if the
migration runs against a table with rows that would violate it?

**How to catch it with a test.** Run `up()` → `down()` → `up()` against both target backends, asserting data
survives and stays correctly typed. For SQLite rebuilds, verify against a real `sqlite3` invocation with
`PRAGMA foreign_keys=ON` set explicitly — a test harness can silently leave it off. For the rolling-deploy
case, seed a row that would violate the new constraint before running the migration and confirm the
expand/contract path still succeeds (or fails loud and attributable, if the design is "block until
backfilled").

**Typical fix.** New migration, never an edit to history. Document any intentional `down()` no-op with the
reason. Expand/contract for any constraint added to a populated table.

*Seen in*: file-storage's migration review manually verified a SQLite CHECK-widening migration correctly
evacuates a child table into an unrelated staging table before dropping its parent, surviving the implicit
cascade in both `up()` and `down()`. Its migration suite table-drives ~25 constraint-rejection cases,
though several assert only `is_err()` without pinning which constraint fired — see
[Tests that prove nothing](#tests-that-prove-nothing).

### Deadlocks and lock ordering

**What it is.** Two operations take row locks on the same two tables in opposite order — A locks X then Y,
B locks Y then X. Run concurrently, each can wait on a lock the other holds: a deadlock, reported as
`40P01` on PostgreSQL and resolved by aborting one transaction. Different error code and cause from a
`40001` serialization failure, but the same retry response: the contention classifier needs to recognize
both, and bounded retry needs to wrap every transaction that can hit either.

**How to find it.** For every pair of operations taking explicit (`FOR UPDATE`) or FK-driven implicit
(`FOR KEY SHARE`) locks on the same two tables, confirm the same acquisition order. Where order genuinely
can't be unified (cross-cutting code, runtime-dependent lock choice), confirm bounded retry wraps both —
deadlock becomes a recoverable outcome instead of a design defect.

**How to catch it with a test.** A barrier test against PostgreSQL running the two opposite-order
operations concurrently, looped enough to actually trigger the deadlock, asserting both eventually complete
rather than one hanging or erroring uncleanly. A static check that the retry classifier's match arms include
the deadlock code alongside the serialization-failure one is a cheap, always-on companion.

**Typical fix.** Unify lock order across call sites where possible (removes the hazard). Where it can't be
unified, rely on bounded retry and ensure the classifier treats deadlock and serialization failure alike.

*Seen in*: file-storage's transaction review found two call sites locking `files`/`file_versions` in
opposite order by design (an auto-bind path on finalize vs. a delete/rebind path), both wrapped in
bounded retry rather than a unified order — a reviewed, accepted choice, not an oversight.

### Mappers defaulting instead of erroring

**What it is.** A row-to-domain mapper hits a column value that doesn't parse into the expected enum/domain
type (one the application-level `CHECK` should normally prevent, but a migration gap, manual data fix, or
other bug could still produce) and silently substitutes a default instead of returning an error. If the
mapped field feeds an authorization or ownership decision, the mapper has quietly made that decision on the
caller's behalf. Looks like: `SomeEnum::parse(&raw).unwrap_or_else(|| { tracing::error!(...);
SomeEnum::default() })` inside a `From<Model>` impl, next to sibling mappers returning
`Err(DomainError::database(...))` for the identical situation.

**How to find it.** For every `From<Model>` mapper, does an unparseable column produce `Err`, or log and
default? A sibling mapper in the same gear doing it correctly is the cheapest signal the defaulting one is
an oversight.

**How to catch it with a test.** Unit test on SQLite: insert a row with a garbage value in the column
(bypassing the domain layer), call the mapper, assert `Err`, not a default.

**Typical fix.** Return `Result` and propagate, matching whatever sibling mappers already do.

*Seen in*: file-storage's mapper review found one mapper defaulting on an unparseable `owner_kind`/`status`
while three siblings in the same gear (policy scope, retention scope, multipart state) all correctly
return `Err` — flagged for the inconsistency, rated low severity only because the `CHECK` constraint makes
the bad value practically unreachable today.

### Tests that prove nothing

**What it is.** Not a database defect — a defect in the test suite that lets the classes above through
unnoticed. Four recurring shapes:

- **Silent skip with no Docker.** A PostgreSQL suite that quietly skips (or passes with zero assertions
  run) when testcontainers isn't reachable, with no CI-level guarantee it *does* run somewhere — the suite
  reads green either way, and CI can't tell whether the concurrency assertions ran at all.
- **Tautological assertions.** `assert!(result.is_ok())` with nothing checked about the value; a
  concurrency test asserting only that both callers returned success without checking table state — both
  can return 200 while the invariant they're supposed to protect is already broken.
- **`is_err()` without checking which error.** Passes for the wrong reason — an unrelated bug also returns
  `Err`, indistinguishable from the specific constraint the test was written to pin.
- **`sleep` instead of a barrier.** Using `tokio::time::sleep` to "make room" for a race is a coin flip
  biased toward one machine's scheduling, not a repeatable test. Only a `tokio::sync::Barrier` forcing both
  tasks past their first `.await` at the same instant is reliable across hardware and load.

**How to find it.** For every PostgreSQL-gated suite, confirm CI runs it with a fail-closed flag (a
`<GEAR>_PG_REQUIRE_DOCKER=1`-shaped env var) rather than only best-effort locally — otherwise "green" and
"ran" are different facts. For every assertion: if the code were subtly wrong in the way this test exists
to catch, would it actually fail? Replace every `sleep()` in a concurrency test with a barrier and confirm
it still catches the bug when the fix is reverted.

**How to catch it with a test** (of the suite itself): not generally mechanizable — a review question
applied to the tests. The closest backstop is [`16_defect_class_to_control_map.md`'s "coverage that proves
nothing" row](16_defect_class_to_control_map.md#coverage-that-proves-nothing): for every known defect,
confirm an executable, currently-pinned assertion exists for it, named after the defect, not a comment.

**Typical fix.** Fail closed on missing Docker in CI. Assert table state and specific error variants, not
`is_ok()`/`is_err()` alone. Replace `sleep` with a barrier. Pin every known, unfixed defect by name.

*Seen in*: resource-group's and file-storage's PostgreSQL suites both fail closed via a
`<GEAR>_PG_REQUIRE_DOCKER=1` CI variable (`.github/workflows/ci.yml`, `make test-rg-pg`/`test-fs-pg`) —
skipping gracefully without Docker locally, hard-failing in CI where the flag is set. File-storage's own
review flagged new sidecar idle-timeout tests using real `sleep` where `start_paused = true` plus
`tokio::time::advance()` would be deterministic, and ~25 migration-rejection assertions checking only
`is_err()` without the constraint name — findings against its own test suite, not the database code.

## How to run an audit

Nothing needs building from scratch: `toolkit_db::test_support` (behind the `test-support` feature) carries
the query recorder; the steps below are the same ones both worked-example audits followed.

1. **Inventory.** List every table, every repository method that issues a statement, and every place a
   transaction is opened. Grep is enough — a completeness check, not analysis yet.
2. **Transaction map.** For every state-changing operation, write down: what runs *before* the transaction
   (validation, external calls), what runs *inside* it (which repository calls, in what order, at what
   isolation level), what external effects it has and exactly where they sit relative to `BEGIN`/`COMMIT`,
   what happens on a crash between any two steps, and what happens if a second, concurrent caller runs the
   same (or a related) operation at the same time. The single highest-yield step — most findings in both
   worked examples came from filling in this table honestly, before any tooling ran.
3. **Query-vs-index check.** For every method's `WHERE`/`ORDER BY`, confirm a serving index with matching
   leading columns; see [Missing indexes](#missing-indexes-for-hot-queries-and-fk-columns).
4. **Loop check.** Grep for a repository/service call inside a loop over a collection just fetched;
   classify each as request-bounded (fine) or data-bounded ([N+1](#n1-query-in-loop-unchunked-in--missing-limit)).
5. **Trace and read.** Add `toolkit_db::test_support::{QueryRecorder, connect_with_recorder,
   snapshot_trace}` as a dev-dependency (feature `test-support`), write one trace test per write operation,
   dump it with `DB_AUDIT_TRACE_DIR=target/db-behavior-traces cargo nextest run -p <gear> --test
   db_behavior_audit_test`, and **read the dump once, by eye, before writing any assertion** — most findings
   in both worked examples were visible on that first read.
6. **Assert the shapes.** `rec.writes_outside_tx().is_empty()` per write operation;
   `rec.untransacted_read_modify_write().is_empty()` for check-then-act; `rec.redundant_reads_after_write()`
   where a write is followed by a read; `rec.all_in_one_transaction()`/`rec.all_in_serializable_transaction()`
   where an invariant depends on several statements sharing one transaction or isolation level — otherwise
   invisible, since `SET TRANSACTION ISOLATION LEVEL` doesn't reach the metric callback and SQLite is
   serializable regardless of what it's asked for.
7. **Scale-invariance.** For every operation whose cost could depend on collection size, build N=small and
   N=large fixtures and assert the statement count (and `rec.total_params()`) doesn't grow between them.
8. **Static rules for what SQL can't see.** `no-retry-serializable`/`external-call-in-tx`/`no-count-queries`
   aren't observable in a trace — write them as plain `#[test]`s over `include_str!`'d source, each with a
   negative control against a correctly-written sibling operation, marked as interim source-text heuristics.
9. **Barrier tests for real concurrency**, with a post-state invariant check called from every scenario —
   checking the two callers' return values is not enough: both can return success while the invariant is
   already broken.
10. **Pin every known defect as an executable assertion**, named after the defect (`RG-01`, `DBS-02`, ...),
    so a silent regression or a silent fix both become visible: `#[ignore = "known defect XX-NN: ..."]` when
    directly observable as a statement count or concurrency outcome; otherwise assert the count/behavior
    that *is* there and say in the doc comment what changes when the fix lands.

**Barrier-test template** (PostgreSQL via `testcontainers`, shared across a file via a process-wide
`OnceCell` so the container starts once):

```rust
static PG: tokio::sync::OnceCell<Option<Arc<PgFixture>>> = tokio::sync::OnceCell::const_new();

async fn shared_pg() -> Option<Arc<PgFixture>> {
    PG.get_or_init(|| async {
        match ContainerRequest::from(Postgres::default()).with_tag("16-alpine").start().await {
            Ok(container) => Some(Arc::new(PgFixture::from(container))),
            Err(e) => { eprintln!("skipping PostgreSQL concurrency tests: {e}"); None }
        }
    }).await.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)] // NOT current_thread -- see below
async fn concurrent_callers_leave_the_invariant_intact() {
    let Some(pg) = shared_pg().await else {
        assert!(std::env::var("MYGEAR_PG_REQUIRE_DOCKER").is_err(), "Docker required in CI");
        return; // best-effort local skip only
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let t1 = tokio::spawn(async move { b1.wait().await; svc1.op(&ctx_a, ...).await });
    let t2 = tokio::spawn(async move { b2.wait().await; svc2.op(&ctx_b, ...).await });
    let (r1, r2) = tokio::join!(t1, t2);
    // Assert a post-state invariant against the tables -- both r1/r2 can be Ok while it's broken.
    assert_invariant_holds(&db).await;
}
```

`multi_thread`, not the default `current_thread`: two tasks on a single-threaded runtime can cooperatively
hand off at `.await` points without ever actually overlapping, making a real bug look intermittently absent
for reasons unrelated to whether it's fixed. Fail closed on missing Docker via a
`<GEAR>_PG_REQUIRE_DOCKER=1`-shaped CI variable, as both worked examples do, so an environment that should
have Docker but doesn't fails loudly instead of silently skipping.

## Where to keep which test

| Class | Test level | Dialect | Guide |
|---|---|---|---|
| Check-then-act / TOCTOU shape | SQLite unit test, query recorder | SQLite suffices for the shape | [`12_unit_testing.md`](12_unit_testing.md) |
| The *race* behind a TOCTOU shape | Barrier test, real concurrency | PostgreSQL only | this doc |
| CAS / `rows_affected` / lost update (sequential) | SQLite unit test | SQLite suffices | [`12_unit_testing.md`](12_unit_testing.md) |
| CAS under real concurrency | Barrier test | PostgreSQL only | this doc |
| External call / event-in-tx | Static source-scan + recorder (`all_in_one_transaction`) | SQLite suffices | this doc |
| Retry wiring / error-shape preservation | Static source-scan + unit round-trip test | SQLite suffices | this doc |
| N+1 / scale-invariance | Query recorder, small-N vs large-N | SQLite suffices | this doc |
| Pagination tie-breaker | SQLite unit test (forced duplicate sort key) | SQLite suffices | [`13_e2e_testing.md`](13_e2e_testing.md) for the HTTP-level cursor roundtrip |
| Missing index / plan shape | `EXPLAIN`-backed integration test, or manual review | PostgreSQL only | [`11_database_patterns.md`](11_database_patterns.md) |
| `COUNT`-for-existence | Static source-scan | N/A | this doc |
| Backend divergence (types, locking, errors) | Feature-gated Rust suite via `testcontainers`, in-process, no HTTP | PostgreSQL (and MySQL where supported) required | [`12_unit_testing.md`'s "Check which venue you actually have"](12_unit_testing.md) |
| Migration correctness (rebuild, `down()`, expand/contract) | Migration test against both target backends | Both; SQLite rebuild needs a raw `sqlite3` check too | [`11_database_patterns.md`](11_database_patterns.md) |
| Deadlock / lock ordering | Barrier test, looped | PostgreSQL only | this doc |
| Mapper defaulting | SQLite unit test (garbage column value) | SQLite suffices | [`12_unit_testing.md`](12_unit_testing.md) |

Where a case needs the full HTTP chain as well as the real database, it belongs in the E2E suite instead
(see [`13_e2e_testing.md`'s "Check which venue you actually have"](13_e2e_testing.md#coverage-goal-one-call-per-api-method));
where it needs real PostgreSQL/MySQL but not the transport, the feature-gated in-process `testcontainers`
suite above is the far more precise diagnosis. Either way, **this is a deliberate deviation from
[`12_unit_testing.md`](12_unit_testing.md) and [`13_e2e_testing.md`](13_e2e_testing.md), and both worked
examples write that deviation down explicitly in their own gear's docs** rather than leaving it implicit —
do the same: a short paragraph in the gear's own testing doc naming which suites exist outside the normal
unit/E2E split and why.

## Audit report template

A gear's own audit report is not this document — keep the catalog and method here, and record only the
gear-specific findings, so a later change to the catalog doesn't require rewriting every gear's report.
Mirror this skeleton (see
[`gears/system/resource-group/docs/db-behavior-audit.md`](../../gears/system/resource-group/docs/db-behavior-audit.md)
for a full worked example):

```markdown
# DB behavior audit — <gear name>

## Scope
What code/branch/PR this audit covers, and what it deliberately excludes.

## What was found
A table: ID | class | severity | where (repository/service, no line numbers) | status (fixed / not
applicable / known-and-not-fixed, with the reason).

## Known and not fixed
Findings deliberately left open, with the reason and the condition under which they'd need revisiting.

## Transaction-behaviour findings
Isolation-level and retry-wiring findings specifically, if reviewed as a separate pass from the
statement-count findings above — record why SERIALIZABLE was or wasn't the right tool for each.

## How it was found
Which mechanisms were used (query recorder, scale-invariance, static scans, PostgreSQL barrier suite) and
what validated that they mean something (known defects rediscovered by general rules, synthetic defects
injected and reverted, negative controls).

## Deviation from the unit/E2E testing guide
Which suites exist outside the normal 12/13 split, and why — see "Where to keep which test" above.

## Running this audit on another module
A pointer back to this document's "How to run an audit" section — do not re-explain the method per gear.

## What this does not cover
Explicitly out of scope for this pass: cost of a single statement, transaction duration, predicate
correctness (vs. presence), constraint inventory, `EXPLAIN`/index usage, the `tokio::spawn` blind spot in
the transaction-membership probe, etc. — whatever is genuinely untested, named plainly rather than implied.

## Deferred
Real findings, not yet actioned, with enough detail that the next person doesn't have to rediscover them.
```

## Related documents

- [`11_database_patterns.md`](11_database_patterns.md) — transaction execution mechanics, the repository
  pattern, and row locks (`SELECT ... FOR UPDATE`) referenced throughout the catalog above.
- [`12_unit_testing.md`](12_unit_testing.md) — where the non-concurrency, non-scale half of a DB-backed
  test still belongs; "Check which venue you actually have" for routing a dialect-specific case to a
  feature-gated PostgreSQL suite instead of pytest E2E.
- [`13_e2e_testing.md`](13_e2e_testing.md) — HTTP-level pagination/cursor roundtrips, and why concurrency
  correctness under the `Service ↔ PostgreSQL` seam is this document's job, not E2E's.
- [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md) — the whole-gear inverse lookup
  this document specializes for the database axis (its "N+1 and query-count regressions" and "Concurrency
  and write-skew" rows).
- [`gears/system/resource-group/docs/db-behavior-audit.md`](../../gears/system/resource-group/docs/db-behavior-audit.md) —
  the first worked example: statement-count/N+1/transaction-boundary findings, `RG-nn`/`TX-nn` IDs.
- [`gears/file-storage/docs/concurrency-and-failure-model.md`](../../gears/file-storage/docs/concurrency-and-failure-model.md) —
  the second worked example's race catalog: the "delete the parent if it has no children" family of races,
  fixed with the row-lock pattern this document's first defect class points to.
