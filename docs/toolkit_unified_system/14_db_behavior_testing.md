# DB Behavior Testing & Audit Guide

<!-- Created: 2026-09-24 by Constructor Tech -->

<!-- toc -->

- [Why and when](#why-and-when)
- [Rules](#rules)
  - [Transactions and retries](#transactions-and-retries)
    - [R1. A retried closure is safe to run again](#r1-a-retried-closure-is-safe-to-run-again)
    - [R2. The retry can fire](#r2-the-retry-can-fire)
    - [R3. Rows locked one at a time are locked in key order](#r3-rows-locked-one-at-a-time-are-locked-in-key-order)
  - [Queries](#queries)
    - [R4. No statement binds a list that can outgrow the bind limit](#r4-no-statement-binds-a-list-that-can-outgrow-the-bind-limit)
    - [R5. A paginated query has a total order](#r5-a-paginated-query-has-a-total-order)
    - [R6. Existence is checked with LIMIT 1](#r6-existence-is-checked-with-limit-1)
  - [Backends](#backends)
    - [R7. Constraint violations are recognized by code](#r7-constraint-violations-are-recognized-by-code)
    - [R8. UUIDs are bound as Uuid](#r8-uuids-are-bound-as-uuid)
    - [R9. On MySQL only Update and Share locks](#r9-on-mysql-only-update-and-share-locks)
  - [Migrations](#migrations)
    - [R10. A shipped migration's up() is never edited](#r10-a-shipped-migrations-up-is-never-edited)
    - [R11. A SQLite table rebuild keeps its child rows](#r11-a-sqlite-table-rebuild-keeps-its-child-rows)
    - [R12. down() says what it does](#r12-down-says-what-it-does)
  - [Tests](#tests)
    - [T1. A suite CI relies on fails instead of skipping](#t1-a-suite-ci-relies-on-fails-instead-of-skipping)
    - [T2. Assertions check the outcome](#t2-assertions-check-the-outcome)
    - [T3. Concurrent callers start on a barrier](#t3-concurrent-callers-start-on-a-barrier)
    - [T4. Lock- and type-dependent behaviour is tested on a real server engine](#t4-lock--and-type-dependent-behaviour-is-tested-on-a-real-server-engine)
    - [T5. A migration that adds a constraint meets a violating row](#t5-a-migration-that-adds-a-constraint-meets-a-violating-row)
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
stability-first ([`13_e2e_testing.md`](13_e2e_testing.md)), so both are blind to it by design.

The document has two parts: (1) **Rules**: only what holds in every gear with no exception, each with its cost; (2) the
testing and audit method. Everything that depends on load, data shape or the guarantees a gear chooses is in
[`docs/arch/database/TRADEOFFS.md`](../arch/database/TRADEOFFS.md) and is not repeated here; this document only
provides the tools to pin such a choice with a test.
[`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md) maps defect classes to controls for a whole
gear; this document specializes its database rows for SeaORM / toolkit-db.

## Why and when
Run the audit in full, or as a targeted pass in code review, when:

- **A table or write path is added**, or a "read, decide, write" / CAS sequence is touched.
- **A concurrent or background operation changes** (cleanup job, takeover, retry loop).
- **A migration changes a constraint, column type or table shape** where rows already exist.
- **A batch, hierarchy or list operation sized by data is added**, an isolation level, retry wrapper or error
  mapping is touched, or a symptom appears (lost update, duplicate row after retry, orphaned child, flaky
  concurrency test).

## Rules
Every rule has five fields: **Rule.** **Why.** **Cost.** **How to find it.** **How to test it.** Cost is always
present; "None." is written only where a rule really costs nothing. `rec` is the `QueryRecorder` of
[step 5](#how-to-run-an-audit).

### Transactions and retries

#### R1. A retried closure is safe to run again
- **Rule.** Everything inside a closure passed to `transaction_with_retry` either rolls back with the transaction or is
  idempotent: no external call, no direct publish, no change to state outside the closure.
- **Why.** The closure re-runs on PostgreSQL `40001`/`40P01`, MySQL deadlocks and SQLite `SQLITE_BUSY`; anything
  outside the transaction repeats.
- **Cost.** That work moves out of the closure, so it runs outside the transaction; where it goes is a choice, see
  [Transactions and external work](../arch/database/TRADEOFFS.md#transactions-and-external-work).
- **How to find it.** An injected client (`Arc<dyn Client>`), a `producer.publish(..)`, or a write to captured shared
  state inside the closure.
- **How to test it.** A static `#[test]` over `include_str!`'d source matching the client type inside the closure,
  with a negative control ([How to run an audit](#how-to-run-an-audit), step 8).

#### R2. The retry can fire
- **Rule.** For a contention failure, the extractor passed to `transaction_with_retry` (the gear's own
  `Fn(&E) -> Option<&DbErr>`, called `as_db_err` in the examples) returns a `DbErr` the classifier recognizes after
  every `map_err`.
- **Why.** The backend-dispatched classifier matches the SQLSTATE or error code, or the message text, including the
  text of a `DbErr::Custom`. If the error was flattened into a `String` that `as_db_err` cannot return as a `DbErr`, or
  a conversion drops that text, the loop never retries and the wrapper is dead code. A `.to_string()` used only for a
  log line does not change the returned error.
- **Cost.** The domain error type keeps the source `DbErr` (or a `DbErr::Custom` carrying the original message)
  reachable.
- **How to find it.** Follow every `map_err` between the repository call and the closure's return value.
- **How to test it.** A unit round-trip passing a real contention error through the actual `map_err` chain into
  `as_db_err` and the classifier.

#### R3. Rows locked one at a time are locked in key order
- **Rule.** A transaction that updates, deletes or locks several rows of one table one statement at a time visits them
  in sorted key order.
- **Why.** Two transactions visiting the same rows in opposite order wait on each other; PostgreSQL aborts one with
  `40P01`. Locks taken by a single multi-row statement, an FK check or a cascade cannot be ordered this way; handling
  those deadlocks is a choice, see [Row locks and dialects](../arch/database/TRADEOFFS.md#row-locks-and-dialects).
- **Cost.** Sorting the keys before the loop.
- **How to find it.** A loop of `update`/`delete`/locking `select` inside a transaction over an unsorted collection.
- **How to test it.** A looped PostgreSQL barrier test running two opposite-order inputs without retry
  (`transaction_with_retry_max` with `max_attempts` 1, or a plain transaction, so a `40P01` surfaces) and asserting
  both complete.

### Queries

#### R4. No statement binds a list that can outgrow the bind limit
- **Rule.** A list whose length follows the data, not a validated request bound, is never bound into one statement
  unbounded.
- **Why.** Every backend caps bind parameters per statement; above the cap the statement fails at run time, usually
  only on production-sized data.
- **Cost.** The rule itself costs nothing; each way of staying under the cap has its own cost, see
  [Batching and bind budgets](../arch/database/TRADEOFFS.md#batching-and-bind-budgets).
- **How to find it.** `is_in(..)` or `.insert_many(..).secure()…exec(..)` (the `SecureInsertMany` builder, which does
  not split) fed by a collection loaded from the database or from an unbounded request field; the toolkit's
  `secure_insert_many` function splits its rows by itself and is exempt.
- **How to test it.** Run the operation with a list above the chunk size (or make the chunk size a parameter the test
  can lower) and assert each `param_count` in `rec.events()` is at most `max_bind_params_for`.

#### R5. A paginated query has a total order
- **Rule.** The `ORDER BY` of every hand-written paginated query, offset or keyset, ends with a unique column. The
  OData layer refuses `$skip`
  ([`07_odata_pagination_select_filter.md`, Unsupported system query options](07_odata_pagination_select_filter.md#unsupported-system-query-options));
  this is about hand-written `.offset()` and cursors. Offset versus keyset is a choice, see
  [Pagination](../arch/database/TRADEOFFS.md#pagination).
  ```rust
  // BAD: equal created_at values may swap order between pages
  CatEntity::find().order_by_desc(cat::Column::CreatedAt).limit(n).offset(m)
      .secure().scope_with(&scope).all(runner).await?;

  // GOOD: unique tie-breaker, same direction
  CatEntity::find().order_by_desc(cat::Column::CreatedAt).order_by_desc(cat::Column::Id).limit(n).offset(m)
      .secure().scope_with(&scope).all(runner).await?;
  ```
- **Why.** Rows equal on the sort key can swap order between page queries: duplicates or skips, no error.
- **Cost.** The index that serves the sort carries the unique column as its trailing key, each key in the direction the
  `ORDER BY` uses (or all reversed, which a backward scan serves), or the database adds a sort step; MySQL before 8.0
  sorts for mixed directions.
- **How to find it.** Every `.offset(` or hand-written cursor: the last `order_by` is a unique column.
- **How to test it.** In the operation's trace, assert that the statement's `ORDER BY` (the `sql` of its event in
  `rec.events()`) ends with the unique column. Paging through tied rows on SQLite passes with the defect present,
  since an index-served sort on SQLite returns ties in rowid order, deterministically.

#### R6. Existence is checked with LIMIT 1
- **Rule.** A yes/no question uses `.one(..)` (a `LIMIT 1` query), never `COUNT(*) > 0`. Whether a list page carries a
  total is decided in
  [`07_odata_pagination_select_filter.md`, Unsupported system query options](07_odata_pagination_select_filter.md#unsupported-system-query-options)
  (it does not); what to do when a number is really needed is in
  [Counting and existence](../arch/database/TRADEOFFS.md#counting-and-existence).
  ```rust
  // BAD: counts every match to answer a yes/no question
  let exists = CatEntity::find().filter(cat::Column::Name.eq(name))
      .secure().scope_with(&scope).count(runner).await? > 0;

  // GOOD: one() limits to a single row and stops there
  let exists = CatEntity::find().filter(cat::Column::Name.eq(name))
      .secure().scope_with(&scope).one(runner).await?.is_some();
  ```
- **Why.** `COUNT` reads every match; `LIMIT 1` stops at the first.
- **Cost.** `.one()` reads one whole row where `COUNT` returns a number; for a single row this is negligible.
- **How to find it.** `.count(` compared with zero.
- **How to test it.** A static scan for `.count(` compared with zero, or assert the operation's trace has `LIMIT` and
  no `COUNT(*)`.

### Backends

#### R7. Constraint violations are recognized by code
- **Rule.** Unique and FK violations are recognized with `ScopeError::is_unique_violation()` /
  `is_foreign_key_violation()`, never by matching message text; contention is left to `transaction_with_retry`.
- **Why.** Message text differs per backend and version.
- **Cost.** None.
- **How to find it.** `.contains(..)` or string comparison on a database error.
- **How to test it.** Provoke the violation in a SQLite unit test and assert the mapped domain error; the same on a
  real PostgreSQL (and MySQL where supported).

#### R8. UUIDs are bound as Uuid
- **Rule.** A UUID column is compared with a `Uuid` value, never a string.
- **Why.** SQLite compares a UUID with `TEXT` silently, often matching nothing; PostgreSQL raises
  `operator does not exist: uuid = text`.
- **Cost.** None.
- **How to find it.** A UUID column filtered with a `String`/`&str`.
- **How to test it.** Run the predicate on a real PostgreSQL
  ([T4](#t4-lock--and-type-dependent-behaviour-is-tested-on-a-real-server-engine)).

#### R9. On MySQL only Update and Share locks
- **Rule.** A gear that runs on MySQL uses only `LockType::Update` and `LockType::Share`.
- **Why.** MySQL has only `FOR UPDATE` and `FOR SHARE` / `LOCK IN SHARE MODE`; sea-query does not translate
  `NoKeyUpdate` and `KeyShare` for it, they come out as PostgreSQL phrases and fail with a syntax error.
- **Cost.** The weaker PostgreSQL lock strengths are unavailable on that path.
- **How to find it.** `.lock(LockType::NoKeyUpdate)` / `KeyShare` in a gear that supports MySQL.
- **How to test it.** Run the locking query on every supported backend.

### Migrations

#### R10. A shipped migration's up() is never edited
- **Rule.** A change to the schema is a new migration; the `up()` of a migration that has run anywhere beyond
  the author's own machine is never edited.
- **Why.** Databases that already ran it keep the old schema while new ones get the edited one, and nothing records
  the difference.
- **Cost.** Every change, however small, is a new migration.
- **How to find it.** The diff modifies the `up()` of an existing migration instead of adding a migration.
- **How to test it.** Review only.

#### R11. A SQLite table rebuild keeps its child rows
- **Rule.** A migration that rebuilds a SQLite table (create new, copy, drop old, rename) preserves every row of the
  tables that reference it.
- **Why.** The toolkit runner executes `up()` inside a transaction, where `PRAGMA foreign_keys = OFF` is a no-op; with
  foreign keys on, `DROP TABLE` runs an implicit `DELETE` that fires `CASCADE` or `SET NULL` on child rows.
- **Cost.** The migration copies child rows out and back, or rebuilds the children as well: more code, a longer
  migration.
- **How to find it.** A `DROP TABLE` of a referenced table on the SQLite path.
- **How to test it.** Seed a parent with children, run `up()`, assert the children are intact; reproduce the rebuild
  in a `sqlite3` session: `PRAGMA foreign_keys = ON;` before `BEGIN` (the CLI default is off and the pragma is ignored
  inside a transaction), then the rebuild inside `BEGIN`/`COMMIT` as the runner runs it.

#### R12. down() says what it does
- **Rule.** `down()` undoes `up()`, or returns `Err(DbErr::Migration(..))` stating why it cannot; a documented
  `Ok(())` only when `up()` left nothing to undo.
  ```rust
  // BAD: no-op down() that reads as a working rollback
  async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> { Ok(()) }

  // GOOD: an irreversible migration says so; a documented Ok(()) is only for a down() with nothing to undo
  async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
      Err(DbErr::Migration("irreversible: `nickname` is UNIQUE, SQLite cannot DROP COLUMN it; \
                            rebuild `cat` (create, copy, drop, rename) by hand".into()))
  }
  ```
- **Why.** The toolkit runner applies only `up()`, so nothing exercises `down()`; a no-op `Ok(())` reads as a working
  rollback.
- **Cost.** None beyond writing the reason.
- **How to find it.** Each `down()` against its `up()`.
- **How to test it.** Run `up()`, `down()`, `up()` on both backends, calling `down()` directly; an irreversible
  `down()` returning `Err(DbErr::Migration(..))` is exempt.

### Tests

#### T1. A suite CI relies on fails instead of skipping
- **Rule.** A PostgreSQL suite that CI relies on fails when Docker is missing; a local run may skip only when the CI
  flag (e.g. `<GEAR>_PG_REQUIRE_DOCKER=1`) is unset.
- **Why.** A PostgreSQL suite that returns early without Docker is green in CI having run nothing.
- **Cost.** CI runners need Docker.
- **How to find it.** An early `return` when the fixture is `None` without checking the flag.
- **How to test it.** The [template](#barrier-test-template) below does it.

#### T2. Assertions check the outcome
- **Rule.** No `is_ok()` alone, no `is_err()` without the error variant, no "both callers succeeded" without the table
  state.
- **Why.** Such assertions pass with the bug present.
- **Cost.** None.
- **How to find it.** Ask of each assertion whether it fails when the bug is present; revert a fix to confirm.
- **How to test it.** Not mechanizable; the backstop is
  [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md#coverage-that-proves-nothing): every known
  defect has a named assertion; an `#[ignore]`d one names the defect in its reason string.

#### T3. Concurrent callers start on a barrier
- **Rule.** A concurrency test releases its callers with a barrier, never a `sleep`, and asserts the table state. What a
  passing run proves is explained under the [template](#barrier-test-template).
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
- **Why.** A `sleep` is a timing guess.
- **Cost.** None.
- **How to find it.** `tokio::time::sleep` between the spawns of a concurrency test.
- **How to test it.** The test itself; revert the fix and confirm the post-state assertion fails.

#### T4. Lock- and type-dependent behaviour is tested on a real server engine
- **Rule.** Behaviour that depends on a lock or a column type is tested on a real server engine the gear runs on
  (PostgreSQL, and MySQL where supported), not only on SQLite.
- **Why.** SQLite renders no lock at all and serializes writers per database, and it compares a UUID with `TEXT`
  silently, so a SQLite run proves nothing about a lock or a typed predicate; only a real engine tells "no rows
  matched" from "the query would not run"
  ([`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md#wire--storage-type-mismatch)).
- **Cost.** Docker in CI and a slower suite.
- **How to find it.** A `.lock(..)` or a UUID predicate covered only by SQLite tests.
- **How to test it.** A feature-gated Rust suite via `testcontainers`, in-process, no HTTP.

#### T5. A migration that adds a constraint meets a violating row
- **Rule.** A migration adding `NOT NULL`, `CHECK` or an FK is tested on a table that holds a violating row.
- **Why.** On an empty test database it passes; on populated data it fails or blocks.
- **Cost.** A seeded fixture per such migration.
- **How to find it.** A migration that adds a constraint with no test that seeds a violating row.
- **How to test it.** Seed the violating row and confirm the migration fails loudly or its backfill handles the row.
  How to roll it out on a populated table is a choice, see
  [Migrations on populated tables](../arch/database/TRADEOFFS.md#migrations-on-populated-tables).

## How to run an audit
Run the audit as an LLM pass over the gear, with the rules above as the checklist; the output is fixes and
pinned tests, not a stored report. Tooling: `toolkit_db::test_support` behind the `test-support` cargo feature
(enable it in the gear's `[dev-dependencies]`); `rec` below is its `QueryRecorder`.

1. **Inventory.** List every table, every repository method issuing a statement, every place a transaction opens.
2. **Transaction map.** Per state-changing operation: what runs before and inside the transaction (order,
   isolation), where external effects sit relative to `BEGIN`/`COMMIT`, and what a crash or a concurrent caller
   does between steps. This is the highest-yield step.
3. **Rules.** Walk the [Rules](#rules) above, using each rule's "How to find it".
4. **Decision points.** For each [TRADEOFFS](../arch/database/TRADEOFFS.md) topic that occurs in the gear, confirm
   the chosen option is deliberate, recorded (a comment at the call site for a local choice, the gear's design doc,
   an ADR when callers or consumers depend on the guarantee) and pinned by a test.
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
6. **Assert the shapes.** Pin the transaction shape the gear chose: assert
   `rec.untransacted_read_modify_write().is_empty()` and `rec.untransacted_write_runs().is_empty()` (advisory: a hit
   is a defect only if the write relies on what came before it) and, for paths that must not re-read what they wrote,
   `rec.redundant_reads_after_write().is_empty()`. A lone write outside a transaction is fine. For an operation that
   must be one transaction assert `rec.all_in_one_transaction()` with `rec.writes_outside_tx().is_empty()` (the former
   ignores statements outside a transaction). Assert `rec.all_in_serializable_transaction()` only on a path that
   deliberately runs `SERIALIZABLE` and on its sibling writers; it checks the level the code *requested* (SQLite is
   serializable regardless), and both `all_in_*` are `false` when nothing ran in a transaction. Put `rec.dump()` in
   assertion messages.
7. **Pin the statement count.** Run the same operation at N=2 and N=50 (`rec.clear()` between) and assert the
   statement counts the gear chose: constant, or `ceil(N / chunk_size)` for a chunked list (run a case above the
   chunk size or lower it through a parameter); a change in either fails the test and turns into a deliberate
   decision. `rec.total_params()` may grow with N wherever one statement binds a list; each `param_count` stays
   within [R4](#r4-no-statement-binds-a-list-that-can-outgrow-the-bind-limit).
8. **Static rules for what SQL cannot show.** The contents of a retried closure
   ([R1](#r1-a-retried-closure-is-safe-to-run-again)), `COUNT` used for existence
   ([R6](#r6-existence-is-checked-with-limit-1)) and message matching on errors
   ([R7](#r7-constraint-violations-are-recognized-by-code)) are not in a trace: write a `#[test]` over
   `include_str!`'d source that fails on the pattern, plus a *negative control* that must fail on a deliberately bad
   string.
9. **Barrier tests** (PostgreSQL): for each race-prone decision point (check-then-act, conditional writes) and for
   [R3](#r3-rows-locked-one-at-a-time-are-locked-in-key-order), start two callers at once (template below).
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

| Test kind | What it can show | Dialect | Used for |
|---|---|---|---|
| SQLite unit test (setup from [`12_unit_testing.md`](12_unit_testing.md)) | Sequential behaviour, error mapping | SQLite | [R6](#r6-existence-is-checked-with-limit-1), [R7](#r7-constraint-violations-are-recognized-by-code), [T2](#t2-assertions-check-the-outcome), and pinned choices that do not depend on concurrency |
| Query-recorder trace test | Statement shape, `ORDER BY`, transaction boundaries, statement and bind counts | SQLite suffices | [R4](#r4-no-statement-binds-a-list-that-can-outgrow-the-bind-limit), [R5](#r5-a-paginated-query-has-a-total-order), [audit steps 6-7](#how-to-run-an-audit) |
| Static source rule with a negative control | What SQL cannot show | N/A | [R1](#r1-a-retried-closure-is-safe-to-run-again), [R6](#r6-existence-is-checked-with-limit-1), [R7](#r7-constraint-violations-are-recognized-by-code) |
| Barrier test, looped where needed (can fail, cannot prove) | Races and deadlocks | PostgreSQL only | [R3](#r3-rows-locked-one-at-a-time-are-locked-in-key-order), [T3](#t3-concurrent-callers-start-on-a-barrier), check-then-act and conditional-write choices |
| Feature-gated real-engine suite (`testcontainers`, in-process, no HTTP) | Locks, types, backend errors | PostgreSQL (and MySQL where supported) | [R8](#r8-uuids-are-bound-as-uuid), [R9](#r9-on-mysql-only-update-and-share-locks), [T4](#t4-lock--and-type-dependent-behaviour-is-tested-on-a-real-server-engine); [`13_e2e_testing.md`](13_e2e_testing.md#coverage-goal-one-call-per-api-method) says which PostgreSQL suite to use |
| Migration test on both backends | `up`/`down`/`up`, rebuilds, constraints on populated data | Both; a SQLite rebuild also in a raw `sqlite3` session | [R10](#r10-a-shipped-migrations-up-is-never-edited), [R11](#r11-a-sqlite-table-rebuild-keeps-its-child-rows), [R12](#r12-down-says-what-it-does), [T5](#t5-a-migration-that-adds-a-constraint-meets-a-violating-row); [`11_database_patterns.md`](11_database_patterns.md#database-migrations) |

Needs HTTP and a real database: E2E ([`13_e2e_testing.md`](13_e2e_testing.md)). Needs real PostgreSQL/MySQL but no
HTTP: a feature-gated Rust suite inside the gear, outside the unit/E2E split; name it in the gear's testing doc.
Anything else: SQLite unit test ([`12_unit_testing.md`](12_unit_testing.md)).

## Related documents

- [`11_database_patterns.md`](11_database_patterns.md): transaction execution mechanics and the repository pattern.
- [`12_unit_testing.md`](12_unit_testing.md): the sequential, SQLite-backed half of DB tests.
- [`13_e2e_testing.md`](13_e2e_testing.md): HTTP-level cursor roundtrips; why concurrency is not E2E's job.
- [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md): the whole-gear defect-to-control map this document specializes.
- [`docs/arch/database/TRADEOFFS.md`](../arch/database/TRADEOFFS.md): the choices with a price (isolation, locks, indexes, pagination, batching, events, migrations): options with their guarantees and costs, not rules.
