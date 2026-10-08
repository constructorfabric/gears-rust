<!-- Created: 2026-10-05 by Constructor Tech -->

# Database Trade-offs

## Table of Contents

- [How to read this](#how-to-read-this)
- [Check-then-act invariants](#check-then-act-invariants)
- [Conditional writes and lost updates](#conditional-writes-and-lost-updates)
- [Isolation levels](#isolation-levels)
- [Retrying aborted transactions](#retrying-aborted-transactions)
- [Row locks and dialects](#row-locks-and-dialects)
- [Indexes](#indexes)
- [Pagination](#pagination)
- [Counting and existence](#counting-and-existence)
- [Batching and bind budgets](#batching-and-bind-budgets)
- [Transactions and external work](#transactions-and-external-work)
- [Event delivery](#event-delivery)
- [Migrations on populated tables](#migrations-on-populated-tables)
- [Unreadable stored values](#unreadable-stored-values)

---

## How to read this

This is not an instruction. It is a list of approaches with their pros and cons. What is convenient at small load
often fails at large load, and the reverse also happens: a heavy mechanism is wasted where contention never occurs.
Each case is decided individually, by the people who own the code and know its numbers.

Load is not the only axis. Data distribution (one hot parent row or many cold ones), contention pattern (the same
key or random keys), dialect (PostgreSQL, MySQL, SQLite behave differently), correctness needs (a lost update is a
bug in one table and a tolerable blip in another) and operational constraints (a table that is already populated and
must be migrated without downtime) all change the answer.

Each topic describes its options, then compares them in one table (Guarantees, Costs and consequences, Choose when,
Avoid when), then ends with **Pin and record**: how to pin the chosen option in a test (the tools are in
[`14_db_behavior_testing.md`](../../toolkit_unified_system/14_db_behavior_testing.md): the barrier template, the query
recorder, the static rules; they are linked, not re-explained here) and where to record the choice: a comment at the
call site for a local choice, the gear's design doc, and an ADR when callers or consumers depend on the guarantee
(delivery semantics, an error code in the API).

The rules that have no exception are in 14 and are not repeated here. The examples below use the same fictional
`Cat` / `Kitten` entities.

## Check-then-act invariants

The shape: read a fact ("this cat has no kittens", "this name is free"), decide, write. A concurrent commit between
the read and the write invalidates the fact. A bare connection, two transactions and one `READ COMMITTED` transaction
all leave that window open.

```rust
// BAD: a kitten inserted after the check is cascaded away, or the FK fails the delete unmapped
let has_kitten = KittenEntity::find().filter(kitten::Column::CatId.eq(id))
    .secure().scope_with(&scope).one(runner).await?.is_some();
if !has_kitten {
    CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(runner).await?;
}
```

**Why a plain transaction is not enough.** At `READ COMMITTED` each statement takes its own snapshot, so a check
made by one statement is stale when the next statement writes, and one transaction around both is not enough. Even a
single `DELETE .. WHERE NOT EXISTS (..)` evaluates its subquery against its own snapshot; if it blocks on the parent
row, it re-checks its `WHERE` only against a newer version of that row, and a child insert does not create one.
`REPEATABLE READ` does not rescue lock-then-check either: its snapshot is taken before the lock wait, so the check
after the wait reads the old state, and write skew is still allowed.

Five approaches, from the most to the least preferred when they apply.

**(a) A database constraint.** A unique index, an FK with `ON DELETE RESTRICT`/`NO ACTION`, or a `CHECK`; the write
fails when the invariant would break, and the violation is mapped to a domain error.

```rust
// GOOD: the FK (kitten.cat_id REFERENCES cat ON DELETE RESTRICT) decides; no pre-check, no extra isolation
let res = CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(runner).await;
match res {
    Ok(r) if r.rows_affected == 0 => Err(DomainError::not_found()),
    Ok(_) => Ok(()),
    Err(e) if e.is_foreign_key_violation() => Err(DomainError::conflict("cat has kittens")),
    Err(e) => Err(e.into()),
}
```

**(b) A parent row lock at `READ COMMITTED`.** The first statement of the transaction is `SELECT .. FOR UPDATE` on the
parent (`.lock(LockType::Update)` on the sea-orm `Select`, before `.secure()`); then check, then write.

**(c) `SERIALIZABLE` inside `transaction_with_retry`.** The database detects the dangerous interleaving and aborts
one transaction with `40001`; the retry re-runs it and sees the new state.

**(d) An advisory lock** through the toolkit `LockManager` (PostgreSQL `pg_try_advisory_lock`, MySQL
`GET_LOCK(name, 0)`, file markers on SQLite): a non-blocking try with retry and backoff; one dedicated session per
process holds all keys.

**(e) Accept the race.** Do the pre-check and the write with no further protection.

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Constraint | Always correct for the invariant it expresses; no lock held beyond the statement; scales with the index | Only when the invariant is expressible as a constraint (not "at most N kittens per cat", not "no cycles"). The violation reaches the caller as an error the API must map (e.g. a conflict). On PostgreSQL a unique violation aborts the surrounding transaction, so insert-or-keep is `INSERT .. ON CONFLICT DO NOTHING` plus a rows-affected check, not catching the error mid-transaction. An FK check on a child insert takes `FOR KEY SHARE` on the parent row, so it waits on a concurrent `FOR UPDATE` of that parent. `ON DELETE RESTRICT` turns a delete with children into an error instead of a cascade. Keeping parent deletes cheap needs an index on the child's FK column (see [Indexes](#indexes)) | The invariant is a unique, FK or per-row bound; this is the default | The invariant spans several rows or a set (a count bound, a cycle check) |
| (b) Parent row lock | Check and write see one state of the parent while every writer of the invariant takes the same lock | Serializes every writer of that parent: a hot parent becomes a queue and a deadlock source. Preconditions the database does not enforce: every writer of the invariant takes the same lock in the same order; no external call under the lock; an FK from the child, so a child insert, which takes `FOR KEY SHARE` on the parent, conflicts with `Update` (it does not conflict with `NoKeyUpdate`). A lock only makes a non-locking writer wait, it does not make it re-check. Not available on SQLite, which renders no lock (see [Row locks and dialects](#row-locks-and-dialects)) | Contention is rare or the parent is cold, and the invariant is not expressible as a constraint | One parent receives most of the writes, or writers outside the gear's control touch the same rows without the lock |
| (c) `SERIALIZABLE` + retry | No lock discipline to design; catches set-shaped predicates (a pedigree cycle check, "may this cat become its own ancestor?") where there is no single row to lock | Aborts and retries grow with contention and transaction length, including false positives; read-heavy paths pay for predicate tracking. Every competing writer that can invalidate the predicate must also be `SERIALIZABLE`, because PostgreSQL's serializable snapshot isolation tracks only serializable transactions. The closure must satisfy [R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again) and [R2](../../toolkit_unified_system/14_db_behavior_testing.md#r2-the-retry-can-fire) | The predicate is set-shaped, no constraint or single row lock can express it, and transactions are short | Contention is high or transactions are long; the abort rate becomes the capacity limit |
| (d) Advisory lock | Coarse mutual exclusion per key: one sweep per key, one migration-like job at a time | Not a per-row lock: every acquisition goes through one session, and a lost connection drops all keys held by it. The lock lives outside the transaction, so it does not protect against writers that do not take it. On SQLite the keys are file markers on the local file system, so they coordinate processes on that host only | The unit of exclusion is a job or a key, not a row, and every actor takes the lock | Per-row throughput matters, or a writer that skips the lock exists |
| (e) Accept the race | None beyond the pre-check | The invariant breaks under concurrent writers; violations need detection and repair | The invariant is advisory (a UX pre-check) or a violation is harmless and repairable | The invariant guards money, authorization or identity |

**Pin and record.** A SQLite sequential test (deleting a cat with a kitten returns `Conflict` and leaves both rows;
deleting a missing cat returns `NotFound`) and a PostgreSQL barrier test asserting the post-state invariant (the
[barrier template](../../toolkit_unified_system/14_db_behavior_testing.md#barrier-test-template) in 14). Where a path
deliberately runs `SERIALIZABLE`, assert `rec.all_in_serializable_transaction()` on it and on every writer sharing its
predicate. Record the choice at the call site; an accepted race belongs in the gear's design doc.

## Conditional writes and lost updates

A conditional `UPDATE .. WHERE <expected state>` guards a transition only if `rows_affected` is checked; zero rows
looks like success when it is ignored. Lost update: reading a whole row, changing one field and writing all back
overwrites a concurrent change to another field.

**(a) A whole-row write by id** (a whole-`ActiveModel` `.update()`).

**(b) Write only the changed columns.**

**(c) A guarded transition:** `UPDATE .. WHERE <expected state>` with `rows_affected` checked.

```rust
// guard on the expected state, set only touched columns, check rows_affected
let res = CatEntity::update_many().col_expr(cat::Column::State, Expr::value("active"))
    .filter(cat::Column::Id.eq(id)).filter(cat::Column::State.eq("pending"))
    .secure().scope_with(&scope).exec(runner).await?;
if res.rows_affected == 0 { return Err(DomainError::conflict("cat is not pending")); }
```

**(d) An optimistic-concurrency column** checked in the `WHERE`.

**(e) A row lock** (option (b) of [Check-then-act invariants](#check-then-act-invariants)).

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Whole-row write | Simple code; the row ends in the state the last writer saw | Last writer wins, and it overwrites a concurrent change to another field | A single writer owns the row, or every field is always written together | Different callers change different fields of the same row |
| (b) Changed columns only | A concurrent change to a different column survives | Last writer wins per column; no signal that a concurrent change to the same column was overwritten | Fields are independent and overwriting the same field is acceptable | A change depends on the value read earlier (a transition, a counter) |
| (c) Guarded transition | A transition happens only from the expected state; a lost race is visible | Zero rows is the only signal that the guard rejected the write. The gear chooses whether zero is a domain error (`Conflict`/`NotFound`/`StaleVersion`, a new outcome the caller must handle) or an idempotent success (the transition already happened); ignoring `rows_affected` without that decision loses the signal | The write is a state transition with a known precondition | The caller cannot handle a new outcome and a repeat would be harmless |
| (d) Version column | Rejects stale writes of any field of the row | The client must carry the version, and conflicts become visible to users; an extra column | Users edit the same record from stale copies and a lost edit matters | Clients cannot carry a version, or conflicts are not meaningful to show |
| (e) Row lock | Read-modify-write sees one state of the row for locking writers | The costs of that option: writers of the row queue, deadlock exposure | The row is cold and the read-modify-write cannot be one guarded statement | The row is hot |

**Pin and record.** Call the guarded transition twice with the same precondition and assert the chosen outcome. For
lost update load two copies, change a different field in each, write both, and assert the chosen semantics by a
direct entity query ("Direct DB assertions" in 12). Record the outcome of a zero-row write at the call site; an ADR
when the API exposes it as an error code.

## Isolation levels

| Level | Prevents | Costs | Worth it when |
|---|---|---|---|
| `READ COMMITTED` (PostgreSQL default; the toolkit passes no level, so the backend default applies) | Dirty reads | Each statement sees a new snapshot; check-then-act needs a constraint or a lock | Almost always, with constraints doing the invariant work |
| `REPEATABLE READ` (MySQL/InnoDB default) | Non-repeatable reads, phantoms (PostgreSQL) | Serialization failures on write conflicts; still allows write skew | A multi-statement read that must see one consistent state |
| `SERIALIZABLE` | Write skew and other anomalies, by aborting | `40001` aborts and retries, more tracking memory, longer transactions fail more | A set-shaped predicate no constraint or single row lock can express |

- **Small load:** the level hardly matters; anomalies need overlapping transactions.
- **Large load:** a higher level converts silent anomalies into explicit aborts, and the abort rate becomes a
  capacity limit. Keep serializable transactions short and narrow.
- **Other axes:** the toolkit default is the backend default, so the same code runs at different levels on
  different dialects. SQLite is serializable regardless, with a single writer.

**Pin and record.** Assert the level the path requested with `rec.all_in_serializable_transaction()` (14 audit step
6); record a non-default level at the call site with the predicate it protects.

## Retrying aborted transactions

A transaction that can abort (a deadlock, or `40001` under `SERIALIZABLE`) either absorbs the abort by running again
or surfaces it.

**(a) `transaction_with_retry`** around transactions that can abort.

```rust
// without retry: a deadlock reaches the caller as a failed request
db.transaction_ref_mapped(|tx| Box::pin(async move {
    repo.swap_kittens(tx, &scope, cat_a, cat_b).await   // locks two cats: can deadlock (40P01)
})).await

// with retry: the body is FnMut and runs again on retry, so clone captures per attempt.
// as_db_err: fn(&DomainError) -> Option<&DbErr>; it must still find the DbErr after every map_err
db.transaction_with_retry(TxConfig::default(), as_db_err, |tx| {
    let (repo, scope) = (repo.clone(), scope.clone());
    Box::pin(async move { repo.swap_kittens(tx, &scope, cat_a, cat_b).await })   // idempotent
}).await
```

**(b) Surface the abort** to the caller as a retryable error.

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) `transaction_with_retry` | The abort is absorbed; the caller sees a result, not a contention error | Each attempt repeats the whole closure (latency, repeated work), retries add load exactly when contention is high, and the closure must satisfy [R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again) and [R2](../../toolkit_unified_system/14_db_behavior_testing.md#r2-the-retry-can-fire) | Aborts are expected (deadlocks, `SERIALIZABLE`) and the closure is cheap and re-runnable | The closure is expensive or cannot be made safe to re-run |
| (b) Surface the abort | No hidden repeated work | Every caller or client must retry, and the error is visible | The caller is better placed to decide whether to retry (it owns the wider operation) | Many callers would each need their own retry logic for the same abort |

**Pin and record.** A unit round-trip for R2 (14); a looped barrier test for the abort path. Record the chosen
behaviour at the call site; an ADR when the API documents a retryable error code.

## Row locks and dialects

PostgreSQL has four row lock strengths: `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, `FOR KEY SHARE`. The ones
that matter in practice: an FK check on a child insert takes `FOR KEY SHARE` on the parent; `FOR UPDATE` conflicts
with it, `FOR NO KEY UPDATE` does not. So a parent lock taken as `Update` makes concurrent child inserts wait, while
one taken as `NoKeyUpdate` does not. Choosing the weaker lock buys concurrency and gives up that serialization.

- **MySQL.** InnoDB FK checks take shared locks on the parent, so a parent `Update` lock serializes child inserts
  there too. The lock types available on MySQL are in
  [R9](../../toolkit_unified_system/14_db_behavior_testing.md#r9-on-mysql-only-update-and-share-locks).
- **SQLite** has no row locks (what that means for tests is
  [T4](../../toolkit_unified_system/14_db_behavior_testing.md#t4-lock--and-type-dependent-behaviour-is-tested-on-a-real-postgresql)).
  Writers are serialized per database (the reserved lock in rollback-journal mode, the write lock in WAL mode). A
  transaction that reads first and then tries to write while another writer is active does not wait on a row lock: it
  fails with `SQLITE_BUSY` (`SQLITE_BUSY_SNAPSHOT` in WAL mode); both are retryable.
- **`NOWAIT` / `SKIP LOCKED`** are for queue-style dequeue, where a worker takes whatever rows nobody else holds. They
  are wrong for check-then-act, where waiting is the point. The toolkit outbox dead-letter reclaim uses
  `FOR UPDATE SKIP LOCKED` on PostgreSQL and MySQL for that reason.
- **Deadlocks.** One order inside a loop is
  [R3](../../toolkit_unified_system/14_db_behavior_testing.md#r3-rows-locked-one-at-a-time-are-locked-in-key-order).
  For locks taken by one multi-row statement, an FK check or a cascade: lock first with
  `SELECT .. ORDER BY id FOR UPDATE` (an extra round trip, locks held longer), or run inside
  `transaction_with_retry` ([Retrying aborted transactions](#retrying-aborted-transactions)), or accept the abort. At
  large load the deadlock rate is itself a cost, since each abort discards work.
- **JSON columns.** Plain `json` has no equality operator on PostgreSQL; `jsonb` has one but stores a normalized form:
  it does not keep key order or whitespace and keeps only the last of duplicate keys.

**Pin and record.** A barrier test on PostgreSQL for the lock-dependent path (14 T4 explains why SQLite cannot show
it); record the lock strength and its reason at the call site.

## Indexes

**(a) Index every foreign key column:** an index whose leading column is the FK column (a partial index helps only if
its predicate covers every referencing row). **(b) No index on the FK column.**

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Index every FK column | Deleting or updating a parent row finds its children by index | Write amplification on every child write, and storage. Adding it to a populated table has a lock cost (see [Migrations on populated tables](#migrations-on-populated-tables)). A partial index helps only if its predicate covers every referencing row | Parent rows are deleted or re-keyed and the child table grows with data | Parents are never deleted or re-keyed and nothing looks children up by the FK |
| (b) No FK index | No write tax and no extra storage | Each parent delete or key update scans the whole child table, under locks | The child table is small and stays small, or parents are immutable | The child table grows with data and parents are deleted or re-keyed |

Further index questions:

- **Composite order.** Equality columns first, then the sort column; for a paginated query see the cost of
  [R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order).
- **Partial indexes.** PostgreSQL uses a partial index only when it can prove the query's `WHERE` implies the index
  predicate. `WHERE status = 'x'` as the index predicate is not used by a query filtering `status IN ('x', 'y')`.
- **What an index costs.** Every index is updated on every insert, update of its columns and delete: write
  amplification, more bloat, more vacuum work. Over-indexing is a real failure mode at high write rates.
- **The planner depends on data size.** A sequential scan on a small table is correct; the same query plan choice
  changes as statistics change. `EXPLAIN` on a development database says little about production: use
  production-scale data and `EXPLAIN (ANALYZE, BUFFERS)`, and remember that SQLite's planner differs from
  PostgreSQL's.
- **Small versus large load.** At small load almost any index set works and fewer indexes are better; at large load
  every missing hot-path index is a latency problem and every unused index is a write tax. The read/write ratio, row
  width, selectivity of the predicate and whether the table is append-only all shift the answer.

**Pin and record.** A statement recorder cannot see a missing index: pin it in migration review and with
`EXPLAIN (ANALYZE, BUFFERS)` on production-scale data. Record the reason for each non-obvious index (or its absence)
in the migration that adds the table.

## Pagination

**(a) Offset (`LIMIT .. OFFSET ..`).** **(b) Keyset / cursor:** the next page is "rows after this sort key". The
platform's OData layer takes this route: it refuses `$skip` and uses a continuation token.

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Offset | Random access to a page number; simple code | The database reads and discards `OFFSET` rows, so deep pages get slower; rows inserted or deleted between requests shift pages (duplicates, skips). The total order of [R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order) still applies | Lists are small and bounded, or admin tooling; clients need to jump to page N | Lists grow without bound, or are mutated while they are read |
| (b) Keyset | Constant cost per page; stable under concurrent writes | Needs a unique, stable sort key and a cursor encoding; cannot jump to page N | Lists grow with data or change while read | Clients need random access to a page number |

Whether a total is needed is covered in [Counting and existence](#counting-and-existence).

**Bounding a read.** A request-path read whose row count grows with data either pages / has a `LIMIT`, or reads
everything because the set is bounded by an enforced rule (validation or a constraint). Paging changes the API for
callers; reading everything grows memory and latency with data.

**Pin and record.** A SQLite test with rows sharing one sort value for the tie-breaker (14 R5); for an unpaged read,
a test or constraint that shows the bound. Record the choice in the gear's design doc; an ADR when callers rely on the
paging contract.

## Counting and existence

Whether a list page carries a total is decided in
[07](../../toolkit_unified_system/07_odata_pagination_select_filter.md) (it does not); existence is
[R6](../../toolkit_unified_system/14_db_behavior_testing.md#r6-existence-is-checked-with-limit-1). When a number is
truly needed: **(a)** exact `COUNT(*)`; **(b)** a capped count, `SELECT COUNT(*) FROM (SELECT 1 .. LIMIT k)`;
**(c)** a planner estimate; **(d)** a maintained counter updated in the same transaction as the change.

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Exact `COUNT(*)` | An exact number at the statement's snapshot | Scans every matching row; on a large table with a loose predicate it is the slowest part of a list endpoint, and it is not stable under concurrent writes either | The exact number is part of the contract and the matching set is small or bounded by an indexed, selective predicate | A yes/no or "more than k" answer is enough, or the predicate is loose on a large table |
| (b) Capped count | Exact up to `k`; the cost is bounded by `k` | Above `k` the answer is only "more than `k`" | The caller needs "how many, up to a limit" (a badge, a threshold) | The exact number above `k` matters |
| (c) Planner estimate | Cheap | Stale; PostgreSQL-specific | An approximate figure is enough and the gear runs on PostgreSQL only | The number drives a decision or must be exact, or other dialects are supported |
| (d) Maintained counter | Exact, cheap to read | Every writer contends on the counter row: the same hot-row trade-off as a parent lock | Reads of the number dominate and writes to the counted set are infrequent | The counted set has many concurrent writers |

**Pin and record.** Assert the trace of the operation (statement shape) with the query recorder (14 audit step 6); for
a counter, a barrier test on the counter row. Record the chosen approximation or counter at the call site.

## Batching and bind budgets

A loop issuing a query per row costs a round trip each and grows with data size, not request size; one
`INSERT .. VALUES` or `WHERE id IN (..)` costs one. Every backend caps bind parameters per statement
([R4](../../toolkit_unified_system/14_db_behavior_testing.md#r4-no-statement-binds-a-list-that-can-outgrow-the-bind-limit)),
so a batch over a list that follows the data needs one of the ways below.

**(a) Per-row queries.** **(b) One batched statement.** **(c) Bound the input** with a validated request limit.

**(d) Chunk against `max_bind_params_for(runner)`** with headroom.

```rust
// one query per chunk replaces both a per-id loop and an unbounded IN
const RESERVED: usize = 2; // binds used by the other predicates, including the scope's tenant filter
let step = max_bind_params_for(runner) - RESERVED;
for chunk in ids.chunks(step) {
    rows.extend(CatEntity::find().filter(cat::Column::Id.is_in(chunk.iter().copied()))
        .secure().scope_with(&scope).all(runner).await?);
}
```

**(e) A PostgreSQL array parameter (`= ANY($1)`) or a temporary table.**

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Per-row queries | One small statement per row | A round trip each, growing with data size | Few rows and indexed lookups, where it can be the cheaper code at no measurable cost | The row count follows the data |
| (b) One batched statement | One round trip regardless of row count | A bind budget; row locks held longer inside a transaction, which can block other writers and raise deadlock odds; a large `IN` list can change the plan | The row count follows the data, or the database is remote | The row count is small and bounded, and the extra code buys nothing |
| (c) Bounded input | One statement, always under the cap | Callers must split larger requests; a validated limit becomes part of the API | The request naturally has a small maximum size | The list comes from stored data |
| (d) Chunking | Any list size stays under the cap on every backend | Several statements, consistent with each other only inside one transaction; the statement count steps up each time the list crosses the chunk size | The list follows the data and the gear supports several dialects | The list is already bounded by validation |
| (e) Array parameter / temporary table | Avoids the bind cap for very large lists | Dialect-specific; needs its own tests | The gear runs on PostgreSQL only and lists are very large | Several dialects are supported |

**Pin and record.** Audit step 7 in 14 pins the statement count. Record the chunk size and its headroom at the call
site.

## Transactions and external work

Where an external call (network, another service) runs relative to the transaction. Never inside a
`transaction_with_retry` closure, see
[R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again).

**(a) Inside the transaction.**

```rust
// external call inside the transaction: holds locks for the whole call
db.transaction_ref_mapped(|tx| Box::pin(async move {
    vet.approve(&cat).await?;
    repo.insert(tx, &scope, &cat).await
})).await?;
```

**(b) Before `BEGIN`.**

```rust
vet.approve(&cat).await?;
db.transaction_ref_mapped(|tx| Box::pin(async move { repo.insert(tx, &scope, &cat).await })).await?;
```

**(c) After `COMMIT`.**

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Inside | Rows the transaction has already locked stay unchanged while the call runs, so the call's result and the write act on one state | Locks are held for the whole call; a slow dependency stalls writers; under retry it repeats (forbidden by R1) | The write depends on both the call's result and rows the transaction has locked, and the call is fast and bounded by a timeout | The dependency is remote or slow, or the transaction can be retried |
| (b) Before `BEGIN` | No locks held during the call | Its result can be stale by `COMMIT`, and if the transaction fails the external effect stays | The call is read-only or independent of the database change, or compensable | The result must hold at commit time and the effect cannot be undone |
| (c) After `COMMIT` | It runs only for a durable change | A crash or failure after `COMMIT` loses the call unless something retries it | Losing the call is tolerable or a retry exists | The effect is coupled to the change and must not be lost; use the outbox ([Event delivery](#event-delivery)) |

**Pin and record.** A static rule for (a) (14 audit step 8). Record the position of the call relative to `BEGIN` at the
call site.

## Event delivery

An event that follows a state change. Both outbox options share these facts: ordering holds only within a partition,
which the producer names explicitly when it enqueues, and there is no ordering across partitions; the `Wake` fires only
after `COMMIT`, and if it is lost (a crash right after `COMMIT`) delivery waits for the reconciler or for the next
start, which reconciles eagerly; every instance runs background workers that poll the database (a reconciler about
once a minute by default), in addition to being woken after commit.

**(a) Direct publish after `COMMIT`** (`producer.publish(..)`).

```rust
db.transaction_ref_mapped(|tx| Box::pin(async move { repo.insert(tx, &scope, &cat).await })).await?;
producer.publish(CatCreated { id }).await?;          // a crash before this line loses the event
```

**(b) A transactional outbox with a leased handler.** **(c) A transactional outbox with a transactional handler.**
Both enqueue the event in the state change's transaction:

```rust
// state change and outbox row in one transaction; the wake fires only on commit
toolkit_db::outbox::in_transaction(&db, |tx| Box::pin(async move {
    repo.insert(tx, &scope, &cat).await?;
    let wake = producer.enqueue(tx, CatCreated { id }).await?;
    Ok(((), wake))
})).await?;
```

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Direct publish | Simple, no extra table | A crash between `COMMIT` and publish loses the event; publishing before `COMMIT` emits an event for a change that may roll back | A lost event is tolerable | Consumers rely on seeing every change |
| (b) Outbox, leased handler | The event commits with the state change; at-least-once delivery | A message is delivered again after a lease expiry, a crash, or a handler `Retry`, so handlers must be idempotent (a key can be derived from the partition id and sequence number). A `Retry` does not advance the partition cursor: that message and everything after it in its partition wait, with exponential backoff (default 1 s growing to 60 s). No built-in attempt limit: the handler sees the attempt count and decides when to `Reject`. `Reject` moves the message to the dead-letter table and the partition moves on; replay from the dead-letter table hands messages back to the caller and does not restore their place in the partition order | A lost event is not acceptable and consumers are, or can be made, idempotent | The service may not run background workers that poll the database, or strict ordering across partitions is needed |
| (c) Outbox, transactional handler | The event commits with the state change; the handler's database writes and the acknowledgement commit in one transaction on the outbox's database: exactly once for writes to that database when the handler succeeds | Effects outside that database (HTTP, other databases) can repeat. On `Retry` the handler's writes are committed with the attempt and the message is delivered again, so those writes must tolerate a rerun. `Reject` dead-letters the whole batch | The handler's effect is a write to the outbox's own database and must happen exactly once | The handler's effect is outside that database, or the service may not run background workers that poll the database |

How bad a lost or duplicated event is, whether consumers are idempotent and what ordering is needed decide between
the options.

**Pin and record.** On the write-plus-event trace assert `rec.all_in_one_transaction()`, `rec.writes_outside_tx()` is
empty and `rec.stats()` contains the outbox `INSERT` (14 audit step 6). Record the delivery guarantee in an ADR, since
consumers depend on it.

## Migrations on populated tables

**(a) A single-step migration.** **(b) Expand / contract:** add the column nullable, backfill in batches, validate,
then tighten (`NOT NULL`, `CHECK`, FK). On PostgreSQL, `ADD CONSTRAINT .. NOT VALID` followed by
`VALIDATE CONSTRAINT` avoids holding a long exclusive lock while existing rows are checked.

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Single step | One step, no intermediate state | Locks last as long as the statement; a constraint added in one step passes on an empty test database and fails or blocks on a populated one | The table is small or empty, and the deploy tolerates a short lock | The table is populated and large, or old and new code run together |
| (b) Expand / contract | Bounded lock durations; old and new code can run together between steps | More migrations and deploy steps, a batched backfill to write and run, and a tightening step that must wait until the backfill is done | The table is populated, the deploy is rolling, or downtime is not tolerated | The table is small and a brief lock is acceptable |

- **What `ALTER` locks.** Many `ALTER TABLE` forms take an exclusive lock for their duration; whether a form rewrites
  the table or only touches the catalog depends on the dialect and version. A short statement on a small table
  becomes a long outage on a large one, so the same migration can be fine in development and unacceptable in
  production.
- **Index builds.** On PostgreSQL a plain `CREATE INDEX` blocks writes for the whole build;
  `CREATE INDEX CONCURRENTLY` cannot run through the toolkit runner, which wraps `up()` in a transaction.
- SQLite table rebuilds and `down()` are rules, not choices: see
  [R11](../../toolkit_unified_system/14_db_behavior_testing.md#r11-a-sqlite-table-rebuild-keeps-its-child-rows) and
  [R12](../../toolkit_unified_system/14_db_behavior_testing.md#r12-down-says-what-it-does).

**Pin and record.** A migration test with a violating row (14 T5). Record the rollout steps in the migration's
comment or the release notes.

## Unreadable stored values

A row-to-domain mapper meets a value that does not parse (a migration gap, a manual fix, or a value written by a newer
version during a rolling deploy).

**(a) Fail the read** (`TryFrom`).

```rust
impl TryFrom<cat::Model> for Cat {
    type Error = DomainError;
    fn try_from(m: cat::Model) -> Result<Self, DomainError> {
        let breed = Breed::parse(&m.breed).ok_or_else(|| DomainError::internal("cat.breed"))?;
        Ok(Self { id: m.id, breed })
    }
}
```

**(b) An explicit `Unknown` variant.** **(c) A silent default** (`unwrap_or_default`).

```rust
impl From<cat::Model> for Cat {
    fn from(m: cat::Model) -> Self { Self { id: m.id, breed: Breed::parse(&m.breed).unwrap_or_default() } }
}
```

| Option | Guarantees | Costs and consequences | Choose when | Avoid when |
|---|---|---|---|---|
| (a) Fail the read | Corruption is visible; the caller sees the corrupt row | One bad row fails the whole read, including a list page, and an older instance fails on values a newer one writes | A wrong value would be worse than a failed read (authorization, ownership, money) | A single bad row must not take down a list and a safe placeholder exists |
| (b) `Unknown` variant | Reads keep working; forward-compatible | Every consumer handles `Unknown`, and it must never grant access or ownership | Values may be written by a newer version during a rolling deploy | Consumers cannot handle an unknown value safely |
| (c) Silent default | Reads never fail | Corrupt or newer data becomes a valid-looking value that may decide authorization or ownership | Only a cosmetic field is affected, with a log line | The value feeds authorization, ownership or any decision |

**Pin and record.** A SQLite unit test inserting a garbage value directly and asserting the chosen outcome. Record the
choice at the mapper.
