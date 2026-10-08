<!-- Created: 2026-10-05 by Constructor Tech -->

# Database Trade-offs

## Table of Contents

- [How to read this](#how-to-read-this)
- [Check-then-act invariants](#check-then-act-invariants)
- [Conditional writes and lost updates](#conditional-writes-and-lost-updates)
- [Isolation levels](#isolation-levels)
- [Retrying aborted transactions](#retrying-aborted-transactions)
- [Row locks and dialects](#row-locks-and-dialects)
- [JSON columns](#json-columns)
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

This is not an instruction and gives no recommendations. Each topic lists the options with what each one gives and
what it costs. Which cost to accept is decided per case by the people who own the code and know its numbers: load,
data distribution, contention pattern, dialect, how bad a violation is and whether a populated table must migrate
without downtime all change the answer.

How a chosen option is recorded and pinned by a test is described in
[`14_db_behavior_testing.md`, How to run an audit](../../toolkit_unified_system/14_db_behavior_testing.md#how-to-run-an-audit).
Rules with no exception are in
[`14_db_behavior_testing.md`, Rules](../../toolkit_unified_system/14_db_behavior_testing.md#rules)
(the R1-R12 and T1-T5 links below point there) and are not repeated here. Examples use the fictional `Cat` /
`Kitten` entities.

## Check-then-act invariants

A read establishes a fact ("this cat has no kittens"), the code decides, then writes; a concurrent commit in between
invalidates the fact. One `READ COMMITTED` transaction around both does not help, since each statement takes its own
snapshot; even a single `DELETE .. WHERE NOT EXISTS (..)` re-checks only the parent row after a lock wait and misses a
new child. `REPEATABLE READ` still allows write skew.

**Database constraint.** A unique index, an FK with `ON DELETE RESTRICT`, or a `CHECK` makes the write fail when the
invariant would break, and the violation is mapped to a domain error. It is correct for every invariant a constraint
can express, and cannot express "at most N kittens" or "no cycles". The caller gets a new error to handle. On
PostgreSQL a unique violation aborts the whole transaction, which `INSERT .. ON CONFLICT DO NOTHING` avoids. An FK check
on a child insert takes `FOR KEY SHARE` on the parent, and parent deletes scan the child table unless its FK column is
indexed ([Indexes](#indexes)).

```rust
// the FK (kitten.cat_id REFERENCES cat ON DELETE RESTRICT) decides; no pre-check, no extra isolation
let res = CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(runner).await;
match res {
    Ok(r) if r.rows_affected == 0 => Err(DomainError::not_found()),
    Ok(_) => Ok(()),
    Err(e) if e.is_foreign_key_violation() => Err(DomainError::conflict("cat has kittens")),
    Err(e) => Err(e.into()),
}
```

**Parent row lock.** The transaction locks the parent first (`.lock(LockType::Update)` before `.secure()`), then
checks and writes. It costs little while contention is rare; a hot parent becomes a queue and a deadlock source. It
holds only if every writer takes the same lock in the same order, nothing external runs under it, and the child has
an FK to the parent (`NoKeyUpdate` would let child inserts through); a writer that does not lock is delayed, not made
to re-check. SQLite has no row locks.

**`SERIALIZABLE` with retry.** The database aborts one of two conflicting transactions with `40001` and
`transaction_with_retry` re-runs it. It needs no lock design and covers set-shaped predicates such as a cycle check;
aborts grow with contention and transaction length, and a writer that can break the predicate is covered only if it
runs `SERIALIZABLE` too.

**Advisory lock.** The toolkit `LockManager` gives mutual exclusion per key, such as one sweep or job at a time, not
per row. One session per process holds all keys, so a lost connection drops them; a writer that skips the lock is not
stopped; on SQLite it coordinates processes on one host only.

**Accept the race.** The pre-check stays with nothing behind it: violations happen under concurrency and need
detection and repair. The cost is whatever a violation breaks, from nothing for a UX hint to money, authorization or
identity.

## Conditional writes and lost updates

Two writers update the same row; what survives depends on how the write is phrased.

**Whole-row write.** A whole-`ActiveModel` `.update()` is simple; the last writer wins and silently overwrites
another writer's change to a different field. Nothing is lost while a single writer owns the row.

**Changed columns only.** Changes to different columns survive; the same column is still last-writer-wins, with no
signal.

**Guarded transition.** `UPDATE .. WHERE state = <expected>` with `rows_affected` checked: zero rows is the only sign
that the guard rejected the write. Zero can mean an error the caller has to handle (`Conflict`, `StaleVersion`) or an
idempotent success.

```rust
let res = CatEntity::update_many().col_expr(cat::Column::State, Expr::value("active"))
    .filter(cat::Column::Id.eq(id)).filter(cat::Column::State.eq("pending"))
    .secure().scope_with(&scope).exec(runner).await?;
if res.rows_affected == 0 { return Err(DomainError::conflict("cat is not pending")); }
```

**Version column.** An optimistic-concurrency column in the `WHERE` rejects any stale write; the client has to carry
the version, and users see the conflicts.

**Row lock.** The parent row lock above, applied to the row itself: writers of that row queue.

## Isolation levels

By default (`TxConfig::default()`) the toolkit passes no level, so each backend's default applies: `READ COMMITTED` on
PostgreSQL, `REPEATABLE READ` on MySQL/InnoDB, serializable on SQLite, which has one writer. The same code therefore
runs at different levels on different dialects.

**`READ COMMITTED`.** Each statement sees a new snapshot, so an invariant across statements needs a constraint or a
lock.

**`REPEATABLE READ`.** One snapshot per transaction. On PostgreSQL a concurrent update of a row this transaction
then writes aborts it with `40001`; on InnoDB the writer waits for the row lock and then overwrites. Write skew is
possible on both.

**`SERIALIZABLE`.** PostgreSQL prevents write skew with serializable snapshot isolation, aborting with `40001`, at
the price of retries and predicate-tracking memory; longer transactions fail more, and at large load the abort rate
becomes a capacity limit. InnoDB turns every read into a shared lock instead, so conflicts block or deadlock. At
small load the level hardly matters, since anomalies need overlapping transactions.

## Retrying aborted transactions

A transaction that can abort (a deadlock, `40001` under `SERIALIZABLE`) either retries or reports the abort.

**Retry.** `transaction_with_retry` absorbs the abort by re-running the whole closure: more latency and repeated
work, extra load exactly when contention is high, and the closure has to satisfy
[R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again) and
[R2](../../toolkit_unified_system/14_db_behavior_testing.md#r2-the-retry-can-fire).

```rust
// the body is FnMut and runs again on retry, so clone captures per attempt;
// as_db_err is the gear's extractor, fn(&DomainError) -> Option<&DbErr>
db.transaction_with_retry(TxConfig::default(), as_db_err, |tx| {
    let (repo, scope) = (repo.clone(), scope.clone());
    Box::pin(async move { repo.swap_kittens(tx, &scope, cat_a, cat_b).await })
}).await
```

**Report the abort.** A retryable error goes to the caller: no hidden repeated work, but every caller has to retry and
sees the error; a caller that owns a wider operation can retry all of it.

## Row locks and dialects

- **PostgreSQL** has four lock strengths. A child insert's FK check takes `FOR KEY SHARE` on the parent, which
  conflicts with `FOR UPDATE` (`LockType::Update`) but not with `FOR NO KEY UPDATE`; the weaker lock buys concurrency
  and gives up that serialization.
- **MySQL.** InnoDB FK checks take shared locks on the parent, so an `Update` lock serializes child inserts too; the
  usable lock types are in
  [R9](../../toolkit_unified_system/14_db_behavior_testing.md#r9-on-mysql-only-update-and-share-locks).
- **SQLite** has no row locks
  ([T4](../../toolkit_unified_system/14_db_behavior_testing.md#t4-lock--and-type-dependent-behaviour-is-tested-on-a-real-server-engine));
  writers are serialized per database, and a read-then-write transaction fails with `SQLITE_BUSY`
  (`SQLITE_BUSY_SNAPSHOT` in WAL mode) instead of waiting; both are retryable.
- **`NOWAIT` / `SKIP LOCKED`** do not wait: the statement fails or skips the locked rows. A queue-style dequeue gets
  rows nobody else holds (the toolkit outbox dead-letter reclaim uses `SKIP LOCKED`); a check-then-act gets no view of
  the locked writer's result.
- **Deadlocks** in loops are
  [R3](../../toolkit_unified_system/14_db_behavior_testing.md#r3-rows-locked-one-at-a-time-are-locked-in-key-order).
  For multi-row statements, FK checks and cascades the options are locking first with
  `SELECT .. ORDER BY id FOR UPDATE` (an extra round trip, locks held longer), a retry, or accepting the abort.

## JSON columns

PostgreSQL has two JSON types. MySQL's `JSON` (8.0.3+) normalizes like `jsonb` and compares as JSON; MariaDB's `JSON` is
`LONGTEXT` with a validity check, kept as written and compared as text. SQLite has no JSON column type: the value is
text (or a `jsonb()` blob since 3.45), and `=` compares bytes.

**`json`.** Keeps the text as written: key order, whitespace and duplicate keys survive; PostgreSQL has no equality
operator for it, so it cannot be compared or deduplicated directly.

**`jsonb`.** Supports equality and GIN indexes; it stores a normalized form that drops key order and whitespace and
keeps only the last of duplicate keys.

## Indexes

**Index on an FK column.** An index led by the FK column lets a parent delete or key update find the children
without a scan. A partial index serves that check only if the check's `WHERE fk = $1` implies its predicate (as with
`fk IS NOT NULL`). It costs a write on every child change and storage, and adding it to a populated table has a lock
cost ([Migrations on populated tables](#migrations-on-populated-tables)). Without parent deletes, key updates or
lookups by the FK, only the cost remains.

**No FK index.** No write tax; every parent delete or key update scans the whole child table under locks.

Other index facts: a composite index serves an equality filter plus a sort only with the equality columns first (for
pagination see the cost of
[R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order)); PostgreSQL uses
a partial index only when the query's `WHERE` implies its predicate; every index taxes writes with bloat and vacuum
work; plans change with data size, so a plan seen on SQLite or a small development database says little about
production, while `EXPLAIN (ANALYZE, BUFFERS)` on production-scale data shows the real one.

## Pagination

**Offset.** `LIMIT .. OFFSET` gives random access to page N with simple code; the database reads and discards the
skipped rows, so deep pages slow down, and writes between requests shift pages (duplicates, skips). On short lists
these costs stay small.

**Keyset.** "Rows after this key" costs the same on every page and is stable under writes; it needs a unique stable
key and a cursor encoding and cannot jump to page N. The OData layer uses it and refuses `$skip`.

Both need a total order
([R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order)). A read with no
bound costs memory and latency that grow with the data; a bound enforced by validation or a constraint caps them
without paging, and paging caps them at the price of a different API for callers.

## Counting and existence

Pages carry no total
([`07_odata_pagination_select_filter.md`, Unsupported system query options](../../toolkit_unified_system/07_odata_pagination_select_filter.md#unsupported-system-query-options));
existence is
[R6](../../toolkit_unified_system/14_db_behavior_testing.md#r6-existence-is-checked-with-limit-1). When a number is
needed:

**Exact `COUNT(*)`** reads every matching row, so its cost grows with the matching set, and the number can change
before the caller uses it.

**Capped count**, `SELECT COUNT(*) FROM (SELECT 1 .. LIMIT k + 1) AS t`, reads at most `k + 1` rows and is exact only up
to `k`: a result of `k + 1` means "more than `k`".

**Planner estimate** (`pg_class.reltuples` or `EXPLAIN` on PostgreSQL, `TABLE_ROWS` or `EXPLAIN` on InnoDB) is
cheap and stale, and can be far off for a filtered set.

**Maintained counter**, updated in the same transaction as the change, is exact and cheap to read, but every writer
contends on the counter row: the same hot-row cost as a parent lock.

## Batching and bind budgets

**Per-row queries** cost a round trip each and grow with data size; for a few indexed lookups the difference can be
unmeasurable, and the code is simpler.

**One batched statement** (`INSERT .. VALUES`, `WHERE id IN (..)`) is one round trip, but it spends the bind budget
([R4](../../toolkit_unified_system/14_db_behavior_testing.md#r4-no-statement-binds-a-list-that-can-outgrow-the-bind-limit));
an `UPDATE`, `DELETE` or locking `SELECT` over an `IN` list locks its rows in plan order rather than list order, so two
overlapping batches can deadlock (the loop case is
[R3](../../toolkit_unified_system/14_db_behavior_testing.md#r3-rows-locked-one-at-a-time-are-locked-in-key-order));
and a large `IN` list can change the plan.

**Ways to stay under the cap.** A validated input limit keeps one statement but becomes part of the API. Chunking
against `max_bind_params_for(runner)` with headroom handles any size with several statements; they see one snapshot only
inside one transaction at `REPEATABLE READ` or above, while at `READ COMMITTED` each chunk sees its own. On PostgreSQL
an array parameter (`= ANY($1)`) avoids the cap. A temporary table works on every backend, but it is filled by inserts
under the same cap (PostgreSQL `COPY` excepted) and lives on one connection, so only inside one transaction; both need
dialect-specific code and tests.

```rust
const RESERVED: usize = 2; // binds used by the other predicates, including the scope's tenant filter
let step = max_bind_params_for(runner) - RESERVED;
for chunk in ids.chunks(step) {
    rows.extend(CatEntity::find().filter(cat::Column::Id.is_in(chunk.iter().copied()))
        .secure().scope_with(&scope).all(runner).await?);
}
```

## Transactions and external work

An external call (network, another service) inside a `transaction_with_retry` closure is excluded by
[R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again). Otherwise:

**Inside the transaction.** The call's result and the write act on rows already locked; the locks are held for the
whole call, so a slow dependency stalls writers, and only a timeout bounds that.

**Before `BEGIN`.** No locks are held; the result can be stale by `COMMIT`, and if the transaction fails the effect
stays, which matters unless the call is read-only, independent of the change, or compensated.

**After `COMMIT`.** The call runs only for a durable change; a crash or failure after `COMMIT` loses it unless
something retries it. The outbox ([Event delivery](#event-delivery)) closes that window.

## Event delivery

**Direct publish after `COMMIT`.** Simple, no extra table; a crash between `COMMIT` and the publish loses the event,
and publishing before `COMMIT` can announce a change that rolls back.

**Transactional outbox.** `outbox::in_transaction` commits the event row with the state change; the `Wake` fires only
after `COMMIT`, and a lost wake waits for the reconciler (about once a minute by default) or the next start. Ordering
holds only within the partition the producer names, never across partitions, and every instance runs background
workers that poll the database. The handler is one of two kinds:

```rust
// the closure's error type implements From<DbError>, From<OutboxError> and From<the repository's error>
toolkit_db::outbox::in_transaction(&db, |tx| Box::pin(async move {
    repo.insert(tx, &scope, &cat).await?;
    let wake = outbox.enqueue(tx, Record::to("cats", partition)
        .payload(payload, "application/json")
        .build()?).await?;
    Ok(((), wake))
})).await?;
```

- **Leased handler:** at-least-once. A lease expiry or a crash redelivers what was not acknowledged and a `Retry`
  redelivers the unprocessed rest of the batch, so a non-idempotent handler repeats its effect (a key from the partition
  id and sequence number identifies duplicates). A `Retry` holds the whole partition, with backoff from 1 s to 60 s by
  default and no built-in attempt limit; a `Reject` dead-letters the message (from a batch handler, the unprocessed rest
  of the batch) and the partition moves on, and a replay does not restore its place in the order.
- **Transactional handler:** its database writes and the acknowledgement commit together in the outbox's database, so on
  success they happen exactly once. A `Retry` commits the writes made so far and redelivers the whole batch, so those
  writes run again; a `Reject` dead-letters the whole batch; effects outside that database can repeat.

## Migrations on populated tables

**Single step.** Simple; its locks last as long as the statement, and a new constraint is checked against every
existing row in that one step, failing on a violating row and holding its lock while it scans.

**Expand / contract.** Add the column nullable, backfill in batches, validate, then tighten (on PostgreSQL
`ADD CONSTRAINT .. NOT VALID`, then `VALIDATE CONSTRAINT`). Locks stay short and old and new code can run together,
at the cost of more migrations and deploy steps.

Many `ALTER TABLE` forms take an exclusive lock, and whether they rewrite the table depends on dialect and version:
the same statement is short on a small table and an outage on a large one. On PostgreSQL a plain `CREATE INDEX` blocks
writes for the whole build, and `CREATE INDEX CONCURRENTLY` cannot run through the toolkit runner, which wraps `up()`
in a transaction. SQLite rebuilds and `down()` are rules:
[R11](../../toolkit_unified_system/14_db_behavior_testing.md#r11-a-sqlite-table-rebuild-keeps-its-child-rows) and
[R12](../../toolkit_unified_system/14_db_behavior_testing.md#r12-down-says-what-it-does).

## Unreadable stored values

A mapper meets a value that does not parse: a migration gap, a manual fix, or a value written by a newer version
during a rolling deploy.

**Fail the read.** `TryFrom` makes corruption visible and keeps a wrong value out of every decision; one bad row fails
the whole read, a list page included, and an older instance fails on values a newer one writes.

```rust
impl TryFrom<cat::Model> for Cat {
    type Error = DomainError;
    fn try_from(m: cat::Model) -> Result<Self, DomainError> {
        let breed = Breed::parse(&m.breed).ok_or_else(|| DomainError::internal("cat.breed"))?;
        Ok(Self { id: m.id, breed })
    }
}
```

**Explicit `Unknown`.** Reads keep working and stay forward-compatible; every consumer has to handle `Unknown`, and
one that treats it as a grant leaks access or ownership.

**Silent default** (`unwrap_or_default`). Reads never fail; corrupt or newer data turns into a valid-looking value,
which decides whatever the field decides, authorization or ownership included.
