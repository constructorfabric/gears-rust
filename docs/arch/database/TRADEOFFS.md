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

This is not an instruction. It lists approaches with what they give and what they cost; each case is decided by the
people who own the code and know its numbers. Load is one axis; data distribution, contention pattern, dialect, how
bad a violation is and whether a populated table must migrate without downtime all change the answer.

Each topic gives one paragraph per option and ends with **Test recommended**: a test that fails if the chosen option
is later changed, built with the tools in
[`14_db_behavior_testing.md`](../../toolkit_unified_system/14_db_behavior_testing.md). Record the choice at
the call site, in the gear's design doc, or in an ADR when callers or consumers depend on the guarantee. Rules with no
exception are in
[`14_db_behavior_testing.md`, Rules](../../toolkit_unified_system/14_db_behavior_testing.md#rules)
(the R1-R12 and T1-T5 links below point there) and are not repeated here. Examples use the fictional `Cat` /
`Kitten` entities.

## Check-then-act invariants

A read establishes a fact ("this cat has no kittens"), the code decides, then writes; a concurrent commit in between
invalidates the fact. One `READ COMMITTED` transaction around both does not help, since each statement takes its own
snapshot; even a single `DELETE .. WHERE NOT EXISTS (..)` re-checks only the parent row after a lock wait and misses a
new child. `REPEATABLE READ` still allows write skew.

**Database constraint.** A unique index, an FK with `ON DELETE RESTRICT`, or a `CHECK` makes the write fail when the
invariant would break; the violation is mapped to a domain error. Always correct and the default whenever the
invariant fits, but it cannot express "at most N kittens" or "no cycles". The caller gets a new error to handle; on
PostgreSQL a unique violation aborts the transaction, so insert-or-keep is `INSERT .. ON CONFLICT DO NOTHING`; an FK
check on a child insert takes `FOR KEY SHARE` on the parent; parent deletes stay cheap only with an index on the
child's FK column ([Indexes](#indexes)).

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

**Parent row lock.** Lock the parent first (`.lock(LockType::Update)` before `.secure()`), then check and write. Cheap
while contention is rare; a hot parent becomes a queue and a deadlock source. It holds only if every writer takes the
same lock in the same order, nothing external runs under it, and the child has an FK to the parent (`NoKeyUpdate`
would let child inserts through); a writer that does not lock is delayed, not made to re-check. SQLite has no row
locks.

**`SERIALIZABLE` with retry.** The database aborts one of two conflicting transactions with `40001` and
`transaction_with_retry` re-runs it. No lock design, and it covers set-shaped predicates such as a cycle check, but
aborts grow with contention and transaction length, and every writer that can break the predicate must run
`SERIALIZABLE` too.

**Advisory lock.** The toolkit `LockManager` gives mutual exclusion per key, such as one sweep or job at a time. One
session per process holds all keys, so a lost connection drops them; a writer that skips the lock is not stopped; on
SQLite it works per host only. For jobs, not rows.

**Accept the race.** Keep the pre-check alone: violations will happen and need detection and repair. Fine for an
advisory check (a UX hint), not for money, authorization or identity.

**Test recommended:** a SQLite test (deleting a cat with kittens returns `Conflict`, a missing cat `NotFound`) and a
PostgreSQL [barrier test](../../toolkit_unified_system/14_db_behavior_testing.md#barrier-test-template) on the
post-state invariant; a `SERIALIZABLE` path also asserts `rec.all_in_serializable_transaction()` on itself and its
sibling writers. An accepted race goes into the gear's design doc.

## Conditional writes and lost updates

Two writers update the same row; what survives depends on how the write is phrased.

**Whole-row write.** A whole-`ActiveModel` `.update()` is simple, but the last writer wins and silently overwrites
another writer's change to a different field. Fine when one writer owns the row.

**Changed columns only.** Changes to different columns survive; the same column is still last-writer-wins, with no
signal.

**Guarded transition.** `UPDATE .. WHERE state = <expected>` with `rows_affected` checked: zero rows is the only sign
that the guard rejected the write. The gear decides whether zero is an error the caller must handle (`Conflict`,
`StaleVersion`) or an idempotent success.

```rust
let res = CatEntity::update_many().col_expr(cat::Column::State, Expr::value("active"))
    .filter(cat::Column::Id.eq(id)).filter(cat::Column::State.eq("pending"))
    .secure().scope_with(&scope).exec(runner).await?;
if res.rows_affected == 0 { return Err(DomainError::conflict("cat is not pending")); }
```

**Version column.** An optimistic-concurrency column in the `WHERE` rejects any stale write; the client must carry
the version, and users see the conflicts.

**Row lock.** The parent row lock above, applied to the row itself: writers of that row queue.

**Test recommended:** call the transition twice with the same precondition; for lost updates, write two copies that
change different fields and check both with a direct entity query
([`12_unit_testing.md`, Table State](../../toolkit_unified_system/12_unit_testing.md#table-state-direct-db-queries)).

## Isolation levels

`READ COMMITTED` is the PostgreSQL default and what the toolkit gets, since it passes no level: each statement sees a
new snapshot, so invariants need a constraint or a lock. `REPEATABLE READ` is the MySQL/InnoDB default: one snapshot
per transaction, serialization failures on write conflicts, write skew still possible. `SERIALIZABLE` prevents write
skew by aborting with `40001`, at the price of retries and tracking memory; longer transactions fail more.

At small load the level hardly matters; at large load the abort rate becomes a capacity limit. The same code runs at
different levels on different dialects; SQLite is always serializable, with one writer.

**Test recommended:** `rec.all_in_serializable_transaction()` on a path that requests `SERIALIZABLE`
([`14_db_behavior_testing.md`, How to run an audit, step 6](../../toolkit_unified_system/14_db_behavior_testing.md#how-to-run-an-audit)).

## Retrying aborted transactions

A transaction that can abort (a deadlock, `40001` under `SERIALIZABLE`) either retries or reports the abort.

**Retry.** `transaction_with_retry` absorbs the abort by re-running the whole closure: more latency and repeated
work, extra load exactly when contention is high, and the closure must satisfy
[R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again) and
[R2](../../toolkit_unified_system/14_db_behavior_testing.md#r2-the-retry-can-fire).

```rust
// the body is FnMut and runs again on retry, so clone captures per attempt
db.transaction_with_retry(TxConfig::default(), as_db_err, |tx| {
    let (repo, scope) = (repo.clone(), scope.clone());
    Box::pin(async move { repo.swap_kittens(tx, &scope, cat_a, cat_b).await })
}).await
```

**Report the abort.** Return a retryable error: no hidden repeated work, but every caller must retry and sees the
error. Better when the caller owns a wider operation.

**Test recommended:** a unit round-trip for
[R2](../../toolkit_unified_system/14_db_behavior_testing.md#r2-the-retry-can-fire)
and a looped barrier test for the abort path.

## Row locks and dialects

- **PostgreSQL** has four lock strengths. A child insert's FK check takes `FOR KEY SHARE` on the parent, which
  conflicts with `FOR UPDATE` (`LockType::Update`) but not with `FOR NO KEY UPDATE`; the weaker lock buys concurrency
  and gives up that serialization.
- **MySQL.** InnoDB FK checks take shared locks on the parent, so an `Update` lock serializes child inserts too; the
  usable lock types are in
  [R9](../../toolkit_unified_system/14_db_behavior_testing.md#r9-on-mysql-only-update-and-share-locks).
- **SQLite** has no row locks
  ([T4](../../toolkit_unified_system/14_db_behavior_testing.md#t4-lock--and-type-dependent-behaviour-is-tested-on-a-real-postgresql));
  writers are serialized per database, and a read-then-write transaction fails with `SQLITE_BUSY`
  (`SQLITE_BUSY_SNAPSHOT` in WAL mode) instead of waiting; both are retryable.
- **`NOWAIT` / `SKIP LOCKED`** suit queues (the toolkit outbox dead-letter reclaim uses `SKIP LOCKED`), not
  check-then-act, where waiting is the point.
- **Deadlocks** in loops are
  [R3](../../toolkit_unified_system/14_db_behavior_testing.md#r3-rows-locked-one-at-a-time-are-locked-in-key-order).
  For multi-row statements, FK checks and cascades: lock first with `SELECT .. ORDER BY id FOR UPDATE` (an extra
  round trip, locks held longer), retry, or accept the abort.
- **JSON.** Plain `json` has no equality operator on PostgreSQL; `jsonb` has one but drops key order and whitespace
  and keeps only the last of duplicate keys.

**Test recommended:** a PostgreSQL barrier test on the lock-dependent path; note the lock strength and why at the
call site.

## Indexes

**Index on an FK column.** An index led by the FK column lets a parent delete or key update find the children
without a scan; a partial index helps only if its predicate covers every referencing row. It costs a write on every
child change and storage, and adding it to a populated table has a lock cost
([Migrations on populated tables](#migrations-on-populated-tables)). Worth it when parents are deleted or re-keyed and
the child table grows.

**No FK index.** No write tax, but every parent delete or key update scans the whole child table under locks. Fine
when parents are never deleted or re-keyed and nothing looks children up by the FK.

Other index questions: equality columns go before the sort column (for pagination see the cost of
[R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order)); PostgreSQL uses
a partial index only when the query's `WHERE` implies its predicate; every index taxes writes with bloat and vacuum
work; plans change with data size, so check `EXPLAIN (ANALYZE, BUFFERS)` on production-scale data, not on SQLite or a
small development database.

**Test recommended:** none fits, since a statement recorder cannot see a missing index; check it in migration review.

## Pagination

**Offset.** `LIMIT .. OFFSET` gives random access to page N with simple code, but the database reads and discards the
skipped rows, so deep pages slow down, and writes between requests shift pages (duplicates, skips). For small,
bounded lists and admin tools.

**Keyset.** "Rows after this key" costs the same on every page and is stable under writes; it needs a unique stable
key and a cursor encoding and cannot jump to page N. The OData layer uses it and refuses `$skip`.

Both need a total order
([R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order)). A request-path
read that grows with data either pages or has a `LIMIT`; reading everything is acceptable only when an enforced rule
(validation, a constraint) bounds the set, and its memory and latency grow with data.

**Test recommended:** the test of
[R5](../../toolkit_unified_system/14_db_behavior_testing.md#r5-a-paginated-query-has-a-total-order);
for an unpaged read, a test or constraint that shows the bound.

## Counting and existence

Pages carry no total
([`07_odata_pagination_select_filter.md`, Unsupported system query options](../../toolkit_unified_system/07_odata_pagination_select_filter.md#unsupported-system-query-options));
existence is
[R6](../../toolkit_unified_system/14_db_behavior_testing.md#r6-existence-is-checked-with-limit-1). When a number is
truly needed:

**Exact `COUNT(*)`** scans every match and is not stable under writes; only when the exact number is part of the
contract and the set is small or bounded by a selective indexed predicate.

**Capped count**, `SELECT COUNT(*) FROM (SELECT 1 .. LIMIT k)`, is exact up to `k` and reads at most `k` rows: enough
for "more than k" checks and badges.

**Planner estimate** is cheap, stale and PostgreSQL-only.

**Maintained counter**, updated in the same transaction as the change, is exact and cheap to read, but every writer
contends on the counter row: the same hot-row problem as a parent lock.

**Test recommended:** assert the operation's statements with the query recorder
([`14_db_behavior_testing.md`, How to run an audit, step 6](../../toolkit_unified_system/14_db_behavior_testing.md#how-to-run-an-audit)),
plus a barrier test for a counter.

## Batching and bind budgets

**Per-row queries** cost a round trip each and grow with data size; for a few indexed lookups they can be the simpler
code at no measurable cost.

**One batched statement** (`INSERT .. VALUES`, `WHERE id IN (..)`) is one round trip, but it spends the bind budget
([R4](../../toolkit_unified_system/14_db_behavior_testing.md#r4-no-statement-binds-a-list-that-can-outgrow-the-bind-limit)),
holds its row locks longer inside a transaction (blocking writers, raising deadlock odds), and a large `IN` list can
change the plan.

**Staying under the cap.** Bound the input with a validated limit, which becomes part of the API; chunk against
`max_bind_params_for(runner)` with headroom (several statements, consistent with each other only inside one
transaction); or, on PostgreSQL only, use an array parameter (`= ANY($1)`) or a temporary table, which need their own
tests.

```rust
const RESERVED: usize = 2; // binds used by the other predicates, including the scope's tenant filter
let step = max_bind_params_for(runner) - RESERVED;
for chunk in ids.chunks(step) {
    rows.extend(CatEntity::find().filter(cat::Column::Id.is_in(chunk.iter().copied()))
        .secure().scope_with(&scope).all(runner).await?);
}
```

**Test recommended:** the statement-count test of
[`14_db_behavior_testing.md`, How to run an audit, step 7](../../toolkit_unified_system/14_db_behavior_testing.md#how-to-run-an-audit).

## Transactions and external work

An external call (network, another service) never runs inside a `transaction_with_retry` closure
([R1](../../toolkit_unified_system/14_db_behavior_testing.md#r1-a-retried-closure-is-safe-to-run-again)). Otherwise:

**Inside the transaction.** The call's result and the write act on rows already locked, but the locks are held for
the whole call and a slow dependency stalls writers. Only for a fast call with a timeout.

**Before `BEGIN`.** No locks held, but the result can be stale by `COMMIT`, and if the transaction fails the effect
stays. For read-only, independent or compensable calls.

**After `COMMIT`.** Runs only for a durable change, but a crash or failure after `COMMIT` loses it unless something
retries it. An effect that must not be lost goes through the outbox ([Event delivery](#event-delivery)).

**Test recommended:** for the before-`BEGIN` and after-`COMMIT` options, a `#[test]` that reads the source with
`include_str!` and fails if the call appears inside the transaction closure
([`14_db_behavior_testing.md`, How to run an audit, step 8](../../toolkit_unified_system/14_db_behavior_testing.md#how-to-run-an-audit)).

## Event delivery

**Direct publish after `COMMIT`.** Simple, no extra table; a crash between `COMMIT` and the publish loses the event,
and publishing before `COMMIT` can announce a change that rolls back.

**Transactional outbox.** `outbox::in_transaction` commits the event row with the state change; the `Wake` fires only
after `COMMIT`, and a lost wake waits for the reconciler (about once a minute by default) or the next start. Ordering
holds only within the partition the producer names, never across partitions, and every instance runs background
workers that poll the database. The handler is one of two kinds:

```rust
toolkit_db::outbox::in_transaction(&db, |tx| Box::pin(async move {
    repo.insert(tx, &scope, &cat).await?;
    let wake = producer.enqueue(tx, CatCreated { id }).await?;
    Ok(((), wake))
})).await?;
```

- **Leased handler:** at-least-once. A lease expiry, a crash or a `Retry` redelivers, so handlers must be idempotent
  (a key from the partition id and sequence number). A `Retry` holds the whole partition, with backoff from 1 s to
  60 s and no built-in attempt limit; a `Reject` dead-letters the message and the partition moves on, and a replay
  does not restore its place in the order.
- **Transactional handler:** its database writes and the acknowledgement commit together in the outbox's database,
  so they happen exactly once on success. Effects outside that database can repeat, a `Retry` commits the handler's
  writes and redelivers the message, and a `Reject` dead-letters the whole batch.

**Test recommended:** on the write-plus-event trace, `rec.all_in_one_transaction()`, an empty
`rec.writes_outside_tx()`, and the outbox `INSERT` in `rec.stats()`. Record the delivery guarantee in an ADR:
consumers depend on it.

## Migrations on populated tables

**Single step.** Simple, but its locks last as long as the statement, and a constraint added in one step passes on an
empty test database and fails or blocks on a populated one. For small tables where a short lock is fine.

**Expand / contract.** Add the column nullable, backfill in batches, validate, then tighten (on PostgreSQL
`ADD CONSTRAINT .. NOT VALID`, then `VALIDATE CONSTRAINT`). Locks stay short and old and new code can run together,
at the cost of more migrations and deploy steps.

Many `ALTER TABLE` forms take an exclusive lock, and whether they rewrite the table depends on dialect and version:
fine in development, an outage on a large table. On PostgreSQL a plain `CREATE INDEX` blocks writes for the whole
build, and `CREATE INDEX CONCURRENTLY` cannot run through the toolkit runner, which wraps `up()` in a transaction.
SQLite rebuilds and `down()` are rules:
[R11](../../toolkit_unified_system/14_db_behavior_testing.md#r11-a-sqlite-table-rebuild-keeps-its-child-rows) and
[R12](../../toolkit_unified_system/14_db_behavior_testing.md#r12-down-says-what-it-does).

**Test recommended:** a migration test with a violating row
([T5](../../toolkit_unified_system/14_db_behavior_testing.md#t5-a-migration-that-adds-a-constraint-meets-a-violating-row)).

## Unreadable stored values

A mapper meets a value that does not parse: a migration gap, a manual fix, or a value written by a newer version
during a rolling deploy.

**Fail the read.** `TryFrom` makes corruption visible, but one bad row fails the whole read, a list page included,
and an older instance fails on values a newer one writes. Right when a wrong value is worse than an error
(authorization, ownership, money).

```rust
impl TryFrom<cat::Model> for Cat {
    type Error = DomainError;
    fn try_from(m: cat::Model) -> Result<Self, DomainError> {
        let breed = Breed::parse(&m.breed).ok_or_else(|| DomainError::internal("cat.breed"))?;
        Ok(Self { id: m.id, breed })
    }
}
```

**Explicit `Unknown`.** Reads keep working and stay forward-compatible; every consumer must handle `Unknown`, and it
must never grant access or ownership.

**Silent default** (`unwrap_or_default`). Reads never fail, but corrupt or newer data turns into a valid-looking value
that may decide authorization or ownership. Only for a cosmetic field, with a log line.

**Test recommended:** a SQLite test that inserts a garbage value directly and asserts the chosen outcome.
