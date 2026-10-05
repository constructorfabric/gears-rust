<!-- Created: 2026-10-05 by Constructor Tech -->

# Database Trade-offs

## Table of Contents

- [How to read this](#how-to-read-this)
- [Check-then-act invariants](#check-then-act-invariants)
- [Isolation levels](#isolation-levels)
- [Row locks and dialects](#row-locks-and-dialects)
- [Indexes](#indexes)
- [Pagination](#pagination)
- [Counting and existence](#counting-and-existence)
- [Batching and bind budgets](#batching-and-bind-budgets)
- [Transactions and external work](#transactions-and-external-work)
- [Migrations on populated tables](#migrations-on-populated-tables)
- [Summary](#summary)

---

## How to read this

This is not an instruction. It is a list of approaches with their pros and cons. What is convenient at small load
often fails at large load, and the reverse also happens: a heavy mechanism is wasted where contention never occurs.
Each case is decided individually, by the people who own the code and know its numbers.

Load is not the only axis. Data distribution (one hot parent row or many cold ones), contention pattern (the same
key or random keys), dialect (PostgreSQL, MySQL, SQLite behave differently), correctness needs (a lost update is a
bug in one table and a tolerable blip in another) and operational constraints (a table that is already populated and
must be migrated without downtime) all change the answer.

The checks that hold regardless of load live in
[`14_db_behavior_testing.md`](../../toolkit_unified_system/14_db_behavior_testing.md). The examples below use the
same fictional `Cat` / `Kitten` entities.

## Check-then-act invariants

The shape: read a fact ("this cat has no kittens", "this name is free"), decide, write. A concurrent commit between
the read and the write invalidates the fact. Four approaches, from the most to the least preferred when they apply.

**(a) A database constraint.** A unique index for check-then-insert, an FK with `ON DELETE RESTRICT`/`NO ACTION`
for delete-if-no-children, a `CHECK` for a per-row bound. The write simply fails when the invariant would break, and
the violation is mapped to a domain error.

| | |
|---|---|
| Small load | Always correct, no extra code path to maintain. |
| Large load | No lock held beyond the statement; scales with the index. |
| Other axes | Only when the invariant is expressible as a constraint (not "at most N kittens per cat", not "no cycles"). On PostgreSQL a unique violation aborts the surrounding transaction, so insert-or-keep is written as `INSERT .. ON CONFLICT DO NOTHING` and the caller checks rows affected, instead of catching the error mid-transaction. |

**(b) A parent row lock at `READ COMMITTED`.** The first statement of the transaction is `SELECT .. FOR UPDATE` on the
parent (`.lock(LockType::Update)` on the sea-orm `Select`, before `.secure()`); then check, then write.

| | |
|---|---|
| Small load | Cheap and easy to reason about; contention is rare. |
| Large load | Serializes every writer of that parent: a hot parent becomes a queue and a deadlock source. |
| Other axes | Preconditions the database does not enforce: every writer of the invariant takes the same lock in the same order; no external call under the lock; an FK from the child so a child insert, which takes `FOR KEY SHARE` on the parent, conflicts with `Update` (it does not conflict with `NoKeyUpdate`). A lock only makes a non-locking writer wait, it does not make it re-check. Not available on SQLite, which renders no lock (see [Row locks and dialects](#row-locks-and-dialects)). |

**(c) `SERIALIZABLE` inside `transaction_with_retry`.** The database detects the dangerous interleaving and aborts
one transaction with `40001`; the retry re-runs it and sees the new state.

| | |
|---|---|
| Small load | No lock discipline to design; catches set-shaped predicates (a pedigree cycle check, "may this cat become its own ancestor?") where there is no single row to lock. |
| Large load | Aborts and retries grow with contention and transaction length, including false positives; read-heavy paths pay for predicate tracking. |
| Other axes | Every writer that can invalidate the predicate must also be `SERIALIZABLE`, because PostgreSQL's serializable snapshot isolation tracks only serializable transactions. The retry closure must be safe to run again. |

**(d) An advisory lock** through the toolkit `LockManager` (PostgreSQL `pg_try_advisory_lock`, MySQL
`GET_LOCK(name, 0)`, file markers on SQLite). Acquisition is a non-blocking try with retry and backoff; one
dedicated session per process holds all keys.

| | |
|---|---|
| Small load | Simple coarse mutual exclusion: one sweep per key, one migration-like job at a time. |
| Large load | Not a per-row lock: every acquisition goes through one session, and a lost connection drops all keys held by it. |
| Other axes | The lock lives outside the transaction, so it does not protect against writers that do not take it. On SQLite the keys are file markers on the local file system, so they coordinate processes on that host only. |

**Why a plain transaction is not enough.** At `READ COMMITTED` each statement takes its own snapshot, so a check
made by one statement is stale when the next statement writes. Even a single `DELETE .. WHERE NOT EXISTS (..)`
evaluates its subquery against its own snapshot; if it blocks on the parent row, it re-checks its `WHERE` only
against a newer version of that row, and a child insert does not create one. `REPEATABLE READ` does not rescue
lock-then-check either: its snapshot is taken before the lock wait, so the check after the wait reads the old state,
and write skew is still allowed.

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

`transaction_with_retry` re-runs the whole closure on PostgreSQL `40001` and `40P01`, MySQL deadlocks and SQLite
`SQLITE_BUSY`/`SQLITE_BUSY_SNAPSHOT`. The backend-dispatched classifier matches the error by SQLSTATE or message
text, so the `DbErr` must stay reachable through every `map_err`; a `.to_string()` anywhere in the chain makes the
retry loop dead code. Retrying is correct only for a closure that is safe to run again: no external call, no
durable effect outside the transaction.

## Row locks and dialects

PostgreSQL has four row lock strengths: `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, `FOR KEY SHARE`. The ones
that matter in practice: an FK check on a child insert takes `FOR KEY SHARE` on the parent; `FOR UPDATE` conflicts
with it, `FOR NO KEY UPDATE` does not. So a parent lock taken as `Update` makes concurrent child inserts wait, while
one taken as `NoKeyUpdate` does not. Choosing the weaker lock buys concurrency and gives up that serialization.

- **MySQL** has only `FOR UPDATE` and `FOR SHARE` / `LOCK IN SHARE MODE`. sea-query does not translate
  `NoKeyUpdate` and `KeyShare` for it: they come out as PostgreSQL phrases and fail with a syntax error. A gear that
  also runs on MySQL uses only `LockType::Update` and `Share`. InnoDB FK checks take shared locks on the parent, so
  a parent `Update` lock serializes child inserts there too.
- **SQLite** renders no lock at all. Writers are serialized per database (the reserved lock in rollback-journal
  mode, the write lock in WAL mode). A transaction that reads first and then tries to write while another writer
  is active does not wait on a row lock: it fails with `SQLITE_BUSY` (`SQLITE_BUSY_SNAPSHOT` in WAL mode); both are
  retryable. A race test run on SQLite proves nothing about a lock-dependent pattern.
- **`NOWAIT` / `SKIP LOCKED`** are for queue-style dequeue, where a worker takes whatever rows nobody else holds. They
  are wrong for check-then-act, where waiting is the point. The toolkit outbox dead-letter reclaim uses
  `FOR UPDATE SKIP LOCKED` on PostgreSQL and MySQL for that reason.
- **Deadlocks.** Two operations locking the same rows in opposite order wait on each other; PostgreSQL aborts one
  with `40P01`. Take locks in one order at every call site (write a batch in id order, or lock it first with
  `SELECT .. ORDER BY id FOR UPDATE`); where that is impossible, run both inside `transaction_with_retry`. At large
  load the deadlock rate is itself a cost, since each abort discards work.

## Indexes

- **Foreign key columns.** Index every FK column on the child. Without it, deleting or updating a parent row scans
  the whole child table to find rows to cascade or check, and that scan runs under locks.
- **Composite order.** Equality columns first, then the sort column; add the unique tie-breaker as the trailing key
  when the query paginates (see [Pagination](#pagination)).
- **Partial indexes.** PostgreSQL uses a partial index only when it can prove the query's `WHERE` implies the index
  predicate. `WHERE status = 'x'` as the index predicate is not used by a query filtering `status IN ('x', 'y')`.
- **What an index costs.** Every index is updated on every insert, update of its columns and delete: write
  amplification, more bloat, more vacuum work. Over-indexing is a real failure mode at high write rates.
- **The planner depends on data size.** A sequential scan on a small table is correct; the same query plan choice
  changes as statistics change. `EXPLAIN` on a development database says little about production: use production-scale
  data and `EXPLAIN (ANALYZE, BUFFERS)`, and remember that SQLite's planner differs from PostgreSQL's.

| | |
|---|---|
| Small load | Almost any index set works; sequential scans are cheap. Prefer fewer indexes. |
| Large load | Every missing hot-path index is a latency problem, every unused index is a write tax. |
| Other axes | Read/write ratio, row width, selectivity of the predicate, and whether the table is append-only. |

## Pagination

- **Offset (`LIMIT .. OFFSET ..`).** The database reads and discards `OFFSET` rows, so deep pages get slower, and
  rows inserted or deleted between requests shift pages (duplicates, skips). With a non-unique sort column the order
  of equal rows can also change between queries, so a unique tie-breaker is mandatory.
- **Keyset / cursor.** The next page is "rows after this sort key"; it needs a unique, stable sort key and a
  cursor encoding, cannot jump to page N, but costs the same on every page and is stable under concurrent writes.
  The platform's OData layer takes this route: it refuses `$skip` and uses a continuation token.

| | |
|---|---|
| Small load | Offset is simpler and fine for small, bounded lists and admin tooling. |
| Large load | Deep offsets degrade; keyset keeps constant cost per page. |
| Other axes | Whether clients need random access to a page number, whether the list is mutated while it is read, and whether a total is needed (see [Counting and existence](#counting-and-existence)). |

## Counting and existence

- **`COUNT(*)`** scans every matching row. On a large table with a loose predicate it is the slowest part of a list
  endpoint, and it is not stable under concurrent writes either. The project forbids `COUNT` queries: pagination is
  cursor-based and returns no total.
- **Existence** is `LIMIT 1` (`.one(runner)`), which stops at the first match and can use an index-only scan.
- **When a number is truly needed:** approximate statistics from the planner (cheap, stale, PostgreSQL-specific), or a
  maintained counter updated in the same transaction as the change (exact, but every writer now contends on the
  counter row, so it is the same hot-row trade-off as a parent lock).

## Batching and bind budgets

- **One batched statement versus one per row.** A loop issuing a query per row costs a round trip each and grows
  with data size, not request size; one `INSERT .. VALUES` or `WHERE id IN (..)` costs one.
- **Bind limits.** Every backend caps bind parameters per statement, so a list bounded by data must be chunked
  against `max_bind_params_for(runner)` with headroom for the other predicates (including the tenant filter the
  scope adds). `rec.total_params()` grows with list size by design; the statement count steps up by one each time
  the list crosses the chunk size.
- **Very large lists.** On PostgreSQL an array parameter (`= ANY($1)`) or a temporary table avoids the bind cap; both
  are dialect-specific and need their own tests.
- **Locks.** Fewer round trips also mean fewer, longer-held statements: a huge batch inside a transaction holds its
  row locks until commit, which can block other writers and raise deadlock odds.

| | |
|---|---|
| Small load | A per-row loop is harmless; chunking is unnecessary code. |
| Large load | Round trips dominate, then bind caps and lock duration; chunk size becomes a tuning knob. |
| Other axes | Latency to the database (a remote database makes round trips expensive even at small row counts). |

## Transactions and external work

- **Nothing non-database inside a transaction.** A network call or other slow work inside an open transaction holds
  its locks for the whole call and, under `transaction_with_retry`, repeats on every attempt. Do it before `BEGIN`,
  or after `COMMIT` if the effect tolerates being sent for an already-durable change.
- **Events.** Two options:
  - **Direct publish after commit** (`producer.publish(..)`): simple, no extra table; a crash between commit and
    publish loses the event.
  - **Transactional outbox** (`toolkit_db::outbox::in_transaction`, `producer.enqueue(tx, ..)` returns a `Wake` that
    fires only after commit): the event commits with the state change and is delivered at least once, so consumers
    must be idempotent. The outbox table is itself write load and needs delivery workers and a dead-letter policy.

| | |
|---|---|
| Small load | Direct publish is often enough when losing an event on a crash is tolerable. |
| Large load | The outbox gives the delivery guarantee, and you pay for its write volume and workers. |
| Other axes | How bad a lost or duplicated event is; whether the consumer is idempotent; ordering needs. |

## Migrations on populated tables

- **Expand / contract.** Add the column nullable, backfill in batches, validate, then tighten (`NOT NULL`, `CHECK`,
  FK). On PostgreSQL, `ADD CONSTRAINT .. NOT VALID` followed by `VALIDATE CONSTRAINT` avoids holding a long exclusive
  lock while existing rows are checked. A constraint added in one step passes on an empty test database and fails or
  blocks on a populated one.
- **What `ALTER` locks.** Many `ALTER TABLE` forms take an exclusive lock for their duration; whether a form rewrites
  the table or only touches the catalog depends on the dialect and version. A short statement on a small table
  becomes a long outage on a large one, so the same migration can be fine in development and unacceptable in
  production.
- **SQLite rebuilds.** SQLite cannot change many constraints in place, so a migration creates a new table, copies,
  drops the old one and renames. The toolkit runner executes `up()` inside a transaction, where
  `PRAGMA foreign_keys = OFF` is a no-op; with foreign keys on, `DROP TABLE` runs an implicit `DELETE` that fires
  `CASCADE` or `SET NULL` on child rows. Copy child rows out and back, or rebuild the children as well.
- **`down()`.** The toolkit runner applies only `up()`. An irreversible `down()` returns
  `Err(DbErr::Migration(..))` stating why; a no-op `Ok(())` is for a `down()` with nothing to undo.

| | |
|---|---|
| Small load | A single-step migration is fine; short locks go unnoticed. |
| Large load | Staged migrations with batched backfill; lock duration is the budget. |
| Other axes | Whether the deploy is rolling (old and new code run together), downtime tolerance, row count, dialect. |

## Summary

| Approach | Small load | Large load | Other axes that change the answer |
|---|---|---|---|
| Constraint (unique, FK, `CHECK`) | Correct, free | Correct, scales | Must be expressible; PostgreSQL aborts the transaction on violation |
| Parent row lock (`READ COMMITTED`) | Cheap | Hot parent becomes a queue | Lock order, FK present, no SQLite, writers must all lock |
| `SERIALIZABLE` + retry | No lock design | Abort and retry rate | All competing writers serializable; closure idempotent |
| Advisory lock | Simple mutual exclusion | Single session, coarse | Outside the transaction; SQLite is file-based |
| Offset pagination | Simple | Deep pages degrade | Random page access, concurrent mutation |
| Keyset pagination | Slightly more code | Constant cost per page | Needs unique stable sort key; no jump to page N |
| Per-row queries | Harmless | Round trips dominate | Latency to the database |
| Batched, chunked statements | Extra code | Fewer round trips, bind caps | Lock duration of large batches |
| Direct publish after commit | Enough | Loses events on crash | Cost of a lost event |
| Transactional outbox | Extra table and workers | Delivery guarantee, write volume | Idempotent consumers, ordering |
| Single-step migration | Fine | Long locks | Rolling deploy, dialect |
| Expand / contract migration | More steps | Bounded locks | Backfill batch size, tolerance for mixed versions |
