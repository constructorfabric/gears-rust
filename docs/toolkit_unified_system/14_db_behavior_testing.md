# DB Behavior Testing & Audit Guide

<!-- Created: 2026-09-24 by Constructor Tech -->

<!-- toc -->

- [Why and when](#why-and-when)
- [Defect catalog](#defect-catalog)
  - [Check-then-act races (TOCTOU)](#check-then-act-races-toctou)
  - [CAS without rows_affected check; lost update](#cas-without-rows_affected-check-lost-update)
  - [External effects inside a transaction; events outside it](#external-effects-inside-a-transaction-events-outside-it)
  - [Retry wiring and non-idempotent retries](#retry-wiring-and-non-idempotent-retries)
  - [N+1, unchunked IN, missing LIMIT](#n1-unchunked-in-missing-limit)
  - [Non-deterministic ORDER BY under offset pagination](#non-deterministic-order-by-under-offset-pagination)
  - [Unindexed FK columns](#unindexed-fk-columns)
  - [COUNT used to check existence](#count-used-to-check-existence)
  - [SQLite / PostgreSQL / MySQL divergence](#sqlite--postgresql--mysql-divergence)
  - [Migration hazards](#migration-hazards)
  - [Deadlocks and lock ordering](#deadlocks-and-lock-ordering)
  - [Mappers defaulting instead of erroring](#mappers-defaulting-instead-of-erroring)
  - [Tests that prove nothing](#tests-that-prove-nothing)
- [How to run an audit](#how-to-run-an-audit)
  - [Barrier-test template](#barrier-test-template)
- [Where to keep which test](#where-to-keep-which-test)
- [Related documents](#related-documents)

<!-- /toc -->

This document describes a third test layer next to unit tests ([`12_unit_testing.md`](12_unit_testing.md)) and E2E
tests ([`13_e2e_testing.md`](13_e2e_testing.md)), and a method for auditing a gear's database code against it. Unit
and E2E tests ask whether the result is correct; this layer asks how the code talked to the database and whether that
survives a second concurrent caller. A check-then-insert with no unique constraint behind it is correct on every
sequential call and corrupts data only when two callers overlap. Unit tests call one thing at a time and E2E tests are
stability-first (13), so both are blind to it by design.
[`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md) maps defect classes to controls for a whole
gear; this document specializes its N+1 and concurrency rows for SeaORM / toolkit-db. Choices that depend on load or
data shape are not made here; they are laid out as trade-offs in
[`docs/arch/database/TRADEOFFS.md`](../arch/database/TRADEOFFS.md).

## Why and when
Run the audit in full, or as a targeted pass in code review, when:

- **A table or write path is added**, or a "read, decide, write" / CAS sequence is touched.
- **A concurrent or background operation changes** (cleanup job, takeover, retry loop).
- **A migration changes a constraint, column type or table shape** where rows already exist.
- **A batch, hierarchy or list operation sized by data is added**, an isolation level, retry wrapper or error
  mapping is touched, or a symptom appears (lost update, duplicate row after retry, orphaned child, flaky
  concurrency test).

## Defect catalog
Each class: what it is, how to find it, how to catch it with a test, the typical fix. `rec` is the `QueryRecorder`
of [step 5](#how-to-run-an-audit).

### Check-then-act races (TOCTOU)
- **What it is.** A read establishes a fact, a decision follows, and a write acts on it; a concurrent commit in
  between invalidates the fact. A bare connection, two transactions and one `READ COMMITTED` transaction all leave
  that window open.
  ```rust
  // BAD: a kitten inserted after the check is cascaded away, or the FK fails the delete unmapped
  let has_kitten = KittenEntity::find().filter(kitten::Column::CatId.eq(id))
      .secure().scope_with(&scope).one(runner).await?.is_some();
  if !has_kitten {
      CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(runner).await?;
  }

  // GOOD: the FK (kitten.cat_id REFERENCES cat ON DELETE RESTRICT) decides; no pre-check, no extra isolation
  let res = CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(runner).await;
  match res {
      Ok(r) if r.rows_affected == 0 => Err(DomainError::not_found()),
      Ok(_) => Ok(()),
      Err(e) if e.is_foreign_key_violation() => Err(DomainError::conflict("cat has kittens")),
      Err(e) => Err(e.into()),
  }
  ```
- **How to find it.** A `SELECT`/`find()` whose result decides a later write, with no constraint, lock or
  `SERIALIZABLE` retry path behind the decision; one transaction around both is not enough.
- **How to catch it with a test.** On SQLite (FKs are enforced): deleting a cat with a kitten returns `Conflict`
  and leaves both rows; deleting a missing cat returns `NotFound`. Reproduce the race with a PostgreSQL
  [barrier test](#barrier-test-template) asserting the post-state invariant. Where a path deliberately runs
  `SERIALIZABLE`, assert `rec.all_in_serializable_transaction()` on it and on every writer sharing its predicate.
- **Typical fix.** A constraint, whenever one can express the invariant: a unique index for check-then-insert, an
  FK with `ON DELETE RESTRICT`/`NO ACTION` for delete-if-no-children, a `CHECK` for a per-row bound; map the
  violation with `ScopeError::is_unique_violation()`/`is_foreign_key_violation()` (GOOD above). When no constraint
  can express it, the remaining options (a parent row lock, `SERIALIZABLE` with retry, an advisory lock) trade
  correctness, contention and operational cost against each other; see
  [Database trade-offs](../arch/database/TRADEOFFS.md#check-then-act-invariants).

### CAS without rows_affected check; lost update
- **What it is.** A conditional `UPDATE .. WHERE <expected state>` guards a transition only if `rows_affected`
  is checked; zero rows looks like success. Lost update: reading a whole row, changing one field and writing all
  back overwrites a concurrent change to another field (a whole-`ActiveModel` `.update()`).
  ```rust
  // BAD: no state guard and rows_affected ignored: a lost race looks like success
  CatEntity::update_many().col_expr(cat::Column::State, Expr::value("active"))
      .filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(runner).await?;

  // GOOD: guard on the expected state, set only touched columns, check rows_affected
  let res = CatEntity::update_many().col_expr(cat::Column::State, Expr::value("active"))
      .filter(cat::Column::Id.eq(id)).filter(cat::Column::State.eq("pending"))
      .secure().scope_with(&scope).exec(runner).await?;
  if res.rows_affected == 0 { return Err(DomainError::conflict("cat is not pending")); }
  ```
- **How to find it.** Is `rows_affected` mapped to a domain outcome? Do updates write only changed columns?
- **How to catch it with a test.** Call the CAS twice with the same precondition; the second must return the
  domain error. For lost update load two copies, change a different field in each, write both, and assert both
  changes by a direct entity query ("Direct DB assertions" in 12).
- **Typical fix.** Map zero rows to `Conflict`/`NotFound`/`StaleVersion`; update only changed columns or add an
  optimistic-concurrency column checked in the `WHERE`.

### External effects inside a transaction; events outside it
- **What it is.** (1) Network I/O or other slow work inside an open transaction holds locks longer and repeats on
  every retry. (2) An event that must not be lost is published after `COMMIT`, so a crash in between loses it.
  ```rust
  // BAD: external call inside the transaction (holds locks, repeats on retry); event published after COMMIT
  db.transaction_ref_mapped(|tx| Box::pin(async move {
      vet.approve(&cat).await?;
      repo.insert(tx, &scope, &cat).await
  })).await?;
  producer.publish(CatCreated { id }).await?;          // a crash before this line loses the event

  // GOOD: call out first; state change and outbox row in one transaction; the wake fires only on commit
  vet.approve(&cat).await?;
  toolkit_db::outbox::in_transaction(&db, |tx| Box::pin(async move {
      repo.insert(tx, &scope, &cat).await?;
      let wake = producer.enqueue(tx, CatCreated { id }).await?;
      Ok(((), wake))
  })).await?;
  ```
- **How to find it.** An injected client (`Arc<dyn Client>`) called inside a closure; an event write outside it.
- **How to catch it with a test.** Static: a source-text rule matching the client type inside the closure, with a
  negative control. Dynamic: on the write-plus-event trace assert `rec.all_in_one_transaction()`,
  `rec.writes_outside_tx().is_empty()` and that `rec.stats()` contains the outbox `INSERT`.
- **Typical fix.** Call out before `BEGIN`, or after `COMMIT` when the effect tolerates a lost or repeated call. An
  event that must survive a crash goes through `outbox::in_transaction`: its row commits with the state change and
  the wake fires only after `COMMIT`. Whether an event needs that guarantee is a trade-off; see
  [Database trade-offs](../arch/database/TRADEOFFS.md#transactions-and-external-work).

### Retry wiring and non-idempotent retries
- **What it is.** `transaction_with_retry` re-runs the whole closure on PostgreSQL `40001`/`40P01`, MySQL
  deadlocks and SQLite `SQLITE_BUSY`; that is correct only if the closure is safe to re-run. Failures: a durable
  side effect outside the transaction repeats; a transaction that can abort (a deadlock, or `40001` under
  `SERIALIZABLE`) has no retry wrapper; the `DbErr` is unreachable when the classifier runs.
  ```rust
  // BAD: this path locks two cats, so it can deadlock (40P01); the abort reaches the caller as a failed request
  db.transaction_ref_mapped(|tx| Box::pin(async move {
      repo.swap_kittens(tx, &scope, cat_a, cat_b).await
  })).await

  // GOOD: retry wrapper; the body is FnMut and runs again on retry, so clone captures per attempt.
  // as_db_err: fn(&DomainError) -> Option<&DbErr>; it must still find the DbErr after every map_err
  db.transaction_with_retry(TxConfig::default(), as_db_err, |tx| {
      let (repo, scope) = (repo.clone(), scope.clone());
      Box::pin(async move { repo.swap_kittens(tx, &scope, cat_a, cat_b).await })   // idempotent
  }).await
  ```
- **How to find it.** Grep per file: a `transaction_ref_mapped`/`transaction_with_config` on a path that locks
  rows or runs `SERIALIZABLE`, beside `transaction_with_retry` siblings, is the tell.
- **How to catch it with a test.** Static scan of retried vs unretried calls, with a negative control; a round-trip
  test passing the real contention error through the actual `map_err` chain into the classifier. The `DbErr` must
  stay reachable by `extract_db_err`; a `.to_string()` anywhere in the chain turns the retry loop into dead code.
- **Typical fix.** Wrap every transaction that can hit contention in the retry helper and keep non-idempotent
  work outside the closure.

### N+1, unchunked IN, missing LIMIT
- **What it is.** An operation over N rows issues one statement per row (`O(N)`, `O(A*N)` for a hierarchy with
  `A` ancestors) instead of a batch. Also: an `IN (..)` list bounded by data, not chunk size, can exceed the
  bind limit; a `SELECT` without `LIMIT` on an unbounded table.
  ```rust
  // BAD: one query per id, and an unbounded IN list
  for id in ids { CatEntity::find_by_id(id).secure().scope_with(&scope).one(runner).await?; }
  CatEntity::find().filter(cat::Column::Id.is_in(ids.clone())).secure().scope_with(&scope).all(runner).await?;

  // GOOD: one query per chunk replaces both the per-id loop and the unbounded IN
  const RESERVED: usize = 2; // binds used by the other predicates, including the scope's tenant filter
  let step = max_bind_params_for(runner) - RESERVED;
  for chunk in ids.chunks(step) {
      rows.extend(CatEntity::find().filter(cat::Column::Id.is_in(chunk.iter().copied()))
          .secure().scope_with(&scope).all(runner).await?);
  }
  ```
- **How to find it.** Classify each repository call in a loop as request- or data-bounded; collection `IN`s use
  `toolkit_db::secure::max_bind_params_for` with headroom; reads have a `LIMIT` or keyset bound.
- **How to catch it with a test.** Scale-invariance at small and large N via `rec.stats()` (audit step 7);
  `rec.redundant_reads_after_write()` finds "insert, discard, re-read".
- **Typical fix.** One batched `INSERT .. VALUES` or `IN (..)`, chunked when the list follows the data; add
  `LIMIT`/keyset bounds; return what the write gave back.

### Non-deterministic ORDER BY under offset pagination
- **What it is.** A list sorts by a non-unique column and paginates with `LIMIT`/`OFFSET` without a unique
  tie-breaker, so equal rows can swap order between pages: duplicates or skips, no error.
  The OData layer refuses `$skip` ([07](07_odata_pagination_select_filter.md)); this is about hand-written `.offset()`.
  ```rust
  // BAD: equal created_at values may swap order between pages
  CatEntity::find().order_by_desc(cat::Column::CreatedAt).limit(n).offset(m)

  // GOOD: unique tie-breaker, same direction
  CatEntity::find().order_by_desc(cat::Column::CreatedAt)
      .order_by_desc(cat::Column::Id).limit(n).offset(m)
  ```
- **How to find it.** For every `.offset(`, check `ORDER BY` ends with a unique column.
- **How to catch it with a test.** On SQLite insert rows sharing one sort value, page through, assert the ids
  across pages equal the inserted set, with no duplicates or omissions.
- **Typical fix.** Append the primary key in the same direction, and as the trailing key of the index that
  serves the sort. Whether offset paging is acceptable at all is a load question; see
  [Database trade-offs](../arch/database/TRADEOFFS.md#pagination).

### Unindexed FK columns
- **What it is.** A child table's FK column has no index, so deleting or updating a parent row scans the whole child
  table to cascade or to check, under locks.
- **How to find it.** List every FK column (audit step 3) and confirm an index starts with it.
- **How to catch it with a test.** Invisible to a statement recorder; a migration-review question. Which other
  indexes a query needs depends on the planner and the load; see
  [Database trade-offs](../arch/database/TRADEOFFS.md#indexes).
- **Typical fix.** A new migration adding the index.

### COUNT used to check existence
- **What it is.** `SELECT COUNT(*) .. > 0` counts all matches instead of stopping at the first. `COUNT` queries
  are forbidden here: pagination is cursor-based and returns no total.
  ```rust
  // BAD: counts every match to answer a yes/no question
  let exists = CatEntity::find().filter(cat::Column::Name.eq(name))
      .secure().scope_with(&scope).count(runner).await? > 0;

  // GOOD: one() limits to a single row and stops there
  let exists = CatEntity::find().filter(cat::Column::Name.eq(name))
      .secure().scope_with(&scope).one(runner).await?.is_some();
  ```
- **How to find it.** Grep `.count(` in repository code ([`07`](07_odata_pagination_select_filter.md): no totals).
- **How to catch it with a test.** A static scan for `.count(` (allow-list real aggregates), or assert the normalized
  SQL has `LIMIT` and no bare `COUNT(*)`.
- **Typical fix.** `.filter(predicate).one(runner)`, branch on the `Option`.

### SQLite / PostgreSQL / MySQL divergence
- **What it is.** One code path behaves differently per backend, and SQLite's permissiveness hides it:
  - **Locking.** `.lock(..)` renders differently per backend and SQLite renders no lock at all, so a SQLite race
    test proves nothing about a lock; see [Database trade-offs](../arch/database/TRADEOFFS.md#row-locks-and-dialects).
  - **Errors.** Map unique and FK violations with `ScopeError::is_unique_violation()`/`is_foreign_key_violation()`
    (SQLSTATE first), not a hand-written message match; leave contention classification to `transaction_with_retry`.
  - **Types.** SQLite compares a UUID with `TEXT` silently (often matching nothing); PostgreSQL raises
    `operator does not exist: uuid = text`: bind a `Uuid`. Plain `json` has no equality operator on PostgreSQL:
    use `jsonb`.
- **How to find it.** Each `.lock(..)` renders valid SQL on every supported backend; each error `match` uses a code
  or variant; each UUID/JSON predicate runs on a real engine.
- **How to catch it with a test.** Execute the predicate on the real engine, as in
  [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md#wire--storage-type-mismatch); only a
  real engine tells "no rows matched" from "the query would not run".
- **Typical fix.** Structured error matching; explicit casts; route lock- and type-sensitive cases to PostgreSQL.

### Migration hazards
- **What it is.** Schema-evolution mistakes:
  - editing a shipped migration;
  - a SQLite table rebuild (create new, copy, drop old, rename) with `foreign_keys = ON`, where `DROP TABLE`
    runs an implicit `DELETE` that fires `CASCADE`/`SET NULL` on children;
  - an undocumented no-op `down()`;
  - a new `NOT NULL`/`CHECK`/FK that existing rows violate, which passes on an empty test database.
  ```rust
  // BAD: no-op down() that reads as a working rollback
  async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> { Ok(()) }

  // GOOD: an irreversible migration says so; a documented Ok(()) is only for a down() with nothing to undo
  async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
      Err(DbErr::Migration("irreversible: `nickname` is UNIQUE, SQLite cannot DROP COLUMN it; \
                            rebuild `cat` (create, copy, drop, rename) by hand".into()))
  }
  ```
- **How to find it.** The diff adds a migration, not edits one; trace child tables when a rebuilt table drops;
  each `down()` undoes `up()` or documents why not; ask what violating rows do.
- **How to catch it with a test.** Run `up()`, `down()`, `up()` on both backends, calling `down()` directly (the
  toolkit runner applies only `up()`); an irreversible `down()` returns `Err(DbErr::Migration(..))` and is exempt.
  Check SQLite rebuilds in a real `sqlite3` session with the pragma set explicitly. Seed a violating row and
  confirm the migration fails loudly on it instead of passing only on an empty database.
- **Typical fix.** A new migration; `Err(DbErr::Migration(..))` for an irreversible `down()`, a documented `Ok(())`
  only when nothing needs undoing. Rebuilds and constraint changes on populated tables have options with different
  lock and downtime costs; see [Database trade-offs](../arch/database/TRADEOFFS.md#migrations-on-populated-tables).

### Deadlocks and lock ordering
- **What it is.** Two operations lock the same rows in opposite order and wait on each other; PostgreSQL aborts
  one with `40P01`, which `transaction_with_retry` treats like `40001`.
- **How to find it.** Operations that lock the same rows (by `UPDATE`/`DELETE`, `FOR UPDATE` or an FK check) take
  them in one order (id order for a batch, or `SELECT .. ORDER BY id FOR UPDATE` first), or run inside retry.
- **How to catch it with a test.** A looped PostgreSQL barrier test running the opposite-order operations and
  asserting both complete.
- **Typical fix.** One lock order at every call site; otherwise wrap both operations in `transaction_with_retry`.

### Mappers defaulting instead of erroring
- **What it is.** A row-to-domain mapper meets a value that does not parse (a migration gap or manual fix) and
  substitutes a default, which may silently decide authorization or ownership.
  ```rust
  // BAD: an unparseable column silently becomes Breed::default()
  impl From<cat::Model> for Cat {
      fn from(m: cat::Model) -> Self { Self { id: m.id, breed: Breed::parse(&m.breed).unwrap_or_default() } }
  }

  // GOOD: fallible conversion; the caller sees the corrupt row
  impl TryFrom<cat::Model> for Cat {
      type Error = DomainError;
      fn try_from(m: cat::Model) -> Result<Self, DomainError> {
          let breed = Breed::parse(&m.breed).ok_or_else(|| DomainError::internal("cat.breed"))?;
          Ok(Self { id: m.id, breed })
      }
  }
  ```
- **How to find it.** Does an unparseable column give `Err` or a logged default?
- **How to catch it with a test.** On SQLite insert a garbage value directly, run the mapper, assert `Err`.
- **Typical fix.** Return `Result` and propagate.

### Tests that prove nothing
- **What it is.** Suite defects that let the classes above through:
  - silent skip without Docker, so nothing guarantees the PostgreSQL suite runs in CI;
  - tautological assertions (`is_ok()`; both callers succeeded, no table-state check);
  - `is_err()` without the error;
  - `sleep` instead of a barrier, a timing guess.
  ```rust
  // BAD: sleep is a timing guess; the assertion ignores table state
  let t1 = tokio::spawn(async move { svc1.create(a).await });
  tokio::time::sleep(Duration::from_millis(5)).await;   // "head start" for t1
  let t2 = tokio::spawn(async move { svc2.create(b).await });
  assert!(t1.await?.is_ok() && t2.await?.is_ok());

  // GOOD: barrier releases both at once; assert what is in the table
  let bar = Arc::new(Barrier::new(2));
  let (b1, b2) = (bar.clone(), bar.clone());
  let t1 = tokio::spawn(async move { b1.wait().await; svc1.create(a).await });
  let t2 = tokio::spawn(async move { b2.wait().await; svc2.create(b).await });
  let _ = (t1.await?, t2.await?);
  let conn = db.conn()?;
  assert_eq!(CatEntity::find().filter(cat::Column::Name.eq("tom"))
      .secure().scope_with(&scope).all(&conn).await?.len(), 1);
  ```
- **How to find it.** Confirm CI sets a flag (e.g. `<GEAR>_PG_REQUIRE_DOCKER=1`) that makes the PostgreSQL suite
  fail instead of skip without Docker (see the template). Ask of each assertion whether it fails when the bug is
  present; revert a fix to confirm.
- **How to catch it with a test.** Not mechanizable; the backstop is
  [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md#coverage-that-proves-nothing):
  every known defect has a named assertion; an `#[ignore]`d one names the defect in its reason string.
- **Typical fix.** Fail closed in CI; assert table state and specific error variants; use barriers.

## How to run an audit
Run the audit as an LLM pass over the gear, with the catalog above as the checklist; the output is fixes and
pinned tests, not a stored report. Tooling: `toolkit_db::test_support` behind the `test-support` cargo feature
(enable it in the gear's `[dev-dependencies]`); `rec` below is its `QueryRecorder`.

1. **Inventory.** List every table, every repository method issuing a statement, every place a transaction opens.
2. **Transaction map.** Per state-changing operation: what runs before and inside the transaction (order,
   isolation), where external effects sit relative to `BEGIN`/`COMMIT`, and what a crash or a concurrent caller
   does between steps. This is the highest-yield step.
3. **FK index check.** For each FK column, confirm an index starts with it ([Unindexed FK columns](#unindexed-fk-columns));
   other index questions are planner- and load-dependent ([trade-offs](../arch/database/TRADEOFFS.md#indexes)).
4. **Loop check.** Find repository calls inside loops; classify as request- or data-bounded ([N+1](#n1-unchunked-in-missing-limit)).
5. **Trace and read.** Write one *trace test* per write operation: a normal `#[tokio::test]` that runs the
   operation once over a recorder-backed `Db` and records every statement:
   ```rust
   let (db, rec) = connect_with_recorder(&dsn, ConnectOpts { max_conns: Some(1), ..Default::default() }).await?;
   run_migrations_for_testing(&db, Migrator::migrations()).await?;   // the gear's migrator
   rec.clear();                             // migrations were recorded too
   svc.create_cat(&ctx, input).await?;
   snapshot_trace("create_cat", &rec);      // writes <DB_AUDIT_TRACE_DIR>/create_cat.txt when the var is set
   ```
   Dump with `DB_AUDIT_TRACE_DIR=target/db-behavior-traces cargo nextest run -p <gear> --test <trace test file>`
   and read each `.txt` by eye before writing assertions.
6. **Assert the shapes.** Assert `rec.untransacted_read_modify_write().is_empty()` and
   `rec.untransacted_write_runs().is_empty()` (advisory: a hit is a defect only if the write relies on what came
   before it) and, for paths that must not re-read what they wrote, `rec.redundant_reads_after_write().is_empty()`.
   A lone write outside a transaction is fine. For an operation that must be one transaction assert
   `rec.all_in_one_transaction()` with `rec.writes_outside_tx().is_empty()` (the former ignores statements outside
   a transaction). Assert `rec.all_in_serializable_transaction()` only on a path that deliberately runs
   `SERIALIZABLE` and on its sibling writers; it checks the level the code *requested* (SQLite is serializable
   regardless), and both `all_in_*` are `false` when nothing ran in a transaction. Put `rec.dump()` in assertion
   messages.
7. **Scale-invariance.** Run the same operation at N=2 and N=50 (`rec.clear()` between) and compare `rec.stats()`:
   statement counts must be identical unless a list is chunked. `rec.total_params()` may grow with N wherever one
   statement binds a list, by a constant per row. A chunked list adds a statement each time N crosses the chunk
   size, `ceil(N / chunk_size)` in all; run a case above the chunk size (or make it a parameter the test can lower),
   assert that count and that each `param_count` in `rec.events()` is at most `max_bind_params_for`.
8. **Static rules for what SQL cannot show.** Retry wiring, external calls in transactions and `COUNT` use are
   not in a trace: write a `#[test]` over `include_str!`'d source that fails on the pattern, plus a *negative
   control* that must fail on a deliberately bad string.
9. **Barrier tests** (PostgreSQL): for each TOCTOU/CAS/deadlock finding, start two callers at once (template below).
10. **Pin every known defect** as a named assertion for the *correct* behaviour, `#[ignore = "known defect: <what>"]`;
    the fix removes the `#[ignore]`. If it cannot be asserted directly, assert today's behaviour and comment what
    to flip.

### Barrier-test template
`pg_fixture()` (one `testcontainers` PostgreSQL per test file via `OnceCell`, `None` without Docker), `svc1`/`svc2`
(two services over the same `pg.db`) and `assert_invariant_holds` are the gear's own helpers, not toolkit APIs.
```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_leave_the_invariant_intact() {
    let Some(pg) = pg_fixture().await else {   // None when Docker is unavailable
        assert!(std::env::var("<GEAR>_PG_REQUIRE_DOCKER").is_err(), "Docker required in CI"); // gear's own flag
        return;                               // best-effort local skip only
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (b1, b2) = (Arc::clone(&barrier), Arc::clone(&barrier));
    let t1 = tokio::spawn(async move { b1.wait().await; svc1.op(&ctx_a).await });
    let t2 = tokio::spawn(async move { b2.wait().await; svc2.op(&ctx_b).await });
    let (r1, r2) = (t1.await.expect("task 1 panicked"), t2.await.expect("task 2 panicked"));
    let _ = (r1, r2); // both outcomes may be legitimate; the table state is the verdict
    assert_invariant_holds(&pg.db).await;
}
```

The barrier only starts both calls together; it cannot force both to read before either writes, so a passing run
does not rule out the race and only a failed post-state assertion demonstrates it. To force the bad interleaving,
add a test-only hook between the read and the write (an injected callback, or one behind a test-only cargo
feature; `#[cfg(test)]` code is not built for `tests/` integration tests) and run the second writer inside it. Use
the plain barrier for likely races, the hook to reproduce one deterministically.

## Where to keep which test

| Class | Test level | Dialect | Guide |
|---|---|---|---|
| Check-then-act / TOCTOU shape | SQLite unit test, query recorder | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup |
| The race behind a TOCTOU shape, or a CAS under real concurrency | Barrier test (can fail, cannot prove) | PostgreSQL only | this doc |
| CAS / `rows_affected` / lost update (sequential) | SQLite unit test | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup |
| External call / event-in-tx | Static scan + recorder (`all_in_one_transaction`, `writes_outside_tx`) | SQLite suffices | this doc |
| Retry wiring / error-shape preservation | Static scan + unit round-trip test | SQLite suffices | this doc |
| N+1 / scale-invariance | Query recorder, small-N vs large-N | SQLite suffices | this doc |
| Pagination tie-breaker | SQLite unit test (forced duplicate sort key) | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup, [`13_e2e_testing.md`](13_e2e_testing.md) for the HTTP cursor roundtrip |
| Unindexed FK column | Migration review | N/A | this doc |
| `COUNT`-for-existence | Static scan | N/A | this doc |
| Backend divergence (types, locking, errors) | Feature-gated Rust suite via `testcontainers`, in-process, no HTTP | PostgreSQL (and MySQL where supported) | [`13_e2e_testing.md`](13_e2e_testing.md#coverage-goal-one-call-per-api-method) (which PostgreSQL suite to use) |
| Migration correctness (rebuild, `down()`, constraints on populated data) | Migration test on both backends | Both; SQLite rebuild also needs a raw `sqlite3` check | [`11_database_patterns.md`](11_database_patterns.md#database-migrations) |
| Deadlock / lock ordering | Barrier test, looped | PostgreSQL only | this doc |
| Mapper defaulting | SQLite unit test (garbage column value) | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup |

Needs HTTP and a real database: E2E (13). Needs real PostgreSQL/MySQL but no HTTP: a feature-gated Rust suite
inside the gear, outside the 12/13 split; name it in the gear's testing doc. Anything else: SQLite unit test (12).

## Related documents

- [`11_database_patterns.md`](11_database_patterns.md): transaction execution mechanics and the repository pattern.
- [`12_unit_testing.md`](12_unit_testing.md): the sequential, SQLite-backed half of DB tests.
- [`13_e2e_testing.md`](13_e2e_testing.md): HTTP-level cursor roundtrips; why concurrency is not E2E's job.
- [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md): the whole-gear defect-to-control map this document specializes.
- [`docs/arch/database/TRADEOFFS.md`](../arch/database/TRADEOFFS.md): the load- and shape-dependent choices behind the catalog (isolation, locks, indexes, pagination, batching, migrations), as pros and cons, not rules.
