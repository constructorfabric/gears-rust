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
  - [Missing indexes](#missing-indexes)
  - [COUNT used to check existence](#count-used-to-check-existence)
  - [SQLite / PostgreSQL / MySQL divergence](#sqlite--postgresql--mysql-divergence)
  - [Migration hazards](#migration-hazards)
  - [Deadlocks and lock ordering](#deadlocks-and-lock-ordering)
  - [Mappers defaulting instead of erroring](#mappers-defaulting-instead-of-erroring)
  - [Tests that prove nothing](#tests-that-prove-nothing)
- [How to run an audit](#how-to-run-an-audit)
  - [Barrier-test template](#barrier-test-template)
- [Where to keep which test](#where-to-keep-which-test)
- [Audit report template](#audit-report-template)
- [Related documents](#related-documents)

<!-- /toc -->

This document describes a third test layer next to unit tests ([`12_unit_testing.md`](12_unit_testing.md))
and E2E tests ([`13_e2e_testing.md`](13_e2e_testing.md)), and a method for auditing a gear's database code
against it. Unit and E2E tests ask whether the result is correct; this layer asks how the code talked to
the database and whether that survives a second concurrent caller. A check-then-insert with no
unique constraint behind it is correct on every sequential call and corrupts data only when two callers overlap. Unit
tests call one thing at a time and E2E tests are stability-first (13), so both are blind to it by design.
[`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md) maps defect classes to controls
for a whole gear; this document specializes its N+1 and concurrency rows for SeaORM / toolkit-db.

## Why and when

Run the audit in full, or as a targeted pass in code review, when:

- **A table or write path is added**, or a "read, decide, write" / CAS sequence is touched.
- **A concurrent or background operation changes** (cleanup job, takeover, retry loop).
- **A migration changes a constraint, column type or table shape** where rows already exist.
- **A batch, hierarchy or list operation sized by data is added**, an isolation level, retry wrapper or error
  mapping is touched, or a symptom appears (lost update, duplicate row after retry, orphaned child, flaky
  concurrency test).

## Defect catalog

Each class: what it is, how to find it, how to catch it with a test, the typical fix. `rec` is the
`QueryRecorder` set up in [step 5](#how-to-run-an-audit).

### Check-then-act races (TOCTOU)
- **What it is.** A read establishes a fact, a decision follows, and a write acts on it; a concurrent commit in
  between invalidates the fact. A bare connection, two transactions and one `READ COMMITTED` transaction all leave
  that window open.
  ```rust
  // BAD: a kitten inserted after the check is cascaded away, or the FK fails the delete unmapped
  let has_kitten = KittenEntity::find().filter(kitten::Column::CatId.eq(id))
      .secure().scope_with(&scope).one(&conn).await?.is_some();
  if !has_kitten {
      CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(&conn).await?;
  }

  // GOOD: the FK (kitten.cat_id REFERENCES cat ON DELETE RESTRICT) decides; no pre-check, no extra isolation
  let res = CatEntity::delete_many().filter(cat::Column::Id.eq(id)).secure().scope_with(&scope).exec(&conn).await;
  match res {
      Ok(r) if r.rows_affected == 0 => Err(DomainError::not_found()),
      Ok(_) => Ok(()),
      Err(e) if e.is_foreign_key_violation() => Err(DomainError::conflict("cat has kittens")),
      Err(e) => Err(e.into()),
  }
  ```
- **How to find it.** A `SELECT`/`find()` whose result decides a later write, with no constraint, parent row lock
  or `SERIALIZABLE` retry path behind the decision; one transaction around both is not enough.
- **How to catch it with a test.** On SQLite (FKs are enforced): deleting a cat with a kitten returns `Conflict`
  and leaves both rows; deleting a missing cat returns `NotFound`. Read the trace for a `SELECT` outside a
  transaction that decides a later write; `rec.untransacted_read_modify_write()` flags it only when read and write
  hit the same table. Reproduce the race with a PostgreSQL [barrier test](#barrier-test-template) asserting the
  post-state invariant, not the callers' return values. On a fix-(c) path assert
  `rec.all_in_serializable_transaction()` on it and on every writer that shares its predicate.
- **Typical fix.** **(a) By default a constraint decides:** a unique index for check-then-insert, an FK with
  `ON DELETE RESTRICT`/`NO ACTION` for delete-if-no-children, a `CHECK` for a per-row bound; map the violation
  with `ScopeError::is_unique_violation()`/`is_foreign_key_violation()` (GOOD above). **(b) When the invariant
  spans rows and no constraint expresses it** ("at most N kittens per cat"), serialize its writers on the parent
  row: `SELECT .. FOR UPDATE` (`.lock(LockType::Update)` on the `Select` before `.secure()`) as the first
  statement of a `READ COMMITTED` transaction (the PostgreSQL default), then check, then write, with no external
  call in between. Every writer of the invariant takes the same lock in the same order; a child insert that does
  not lock is held back only by its FK check (`FOR KEY SHARE`, which conflicts with `Update` but not
  `NoKeyUpdate`): that makes it wait, it does not make it check, and without an FK not even that. SQLite renders
  no lock and relies on its single writer. Nothing enforces these conditions, so state them at the call site.
  **(c) `SERIALIZABLE` inside `transaction_with_retry` only when no row can carry the lock:** the check looks for
  a row that does not exist yet (a pedigree cycle check: may this cat become its own ancestor?) and locking one
  row above every writer, such as the owning tenant, would serialize too much. Then every writer that can
  invalidate the predicate must be `SERIALIZABLE` too, since SSI tracks only serializable transactions, and the
  path pays with `40001` aborts. Not where a constraint or the target row already decides. **Why a plain
  transaction is not enough:** at `READ COMMITTED` each statement takes a new snapshot, so the check is stale by
  the time the write runs; even one `DELETE .. WHERE NOT EXISTS (..)` evaluates its subquery against its own
  snapshot, and a wait on the parent row re-checks the `WHERE` only against a newer version of that row, which a
  child insert does not create.

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
  domain error. For lost update load two copies first, change a different field in each, write both, and assert
  both changes by a direct entity query ("Direct DB assertions" in 12).
- **Typical fix.** Map zero rows to `Conflict`/`NotFound`/`StaleVersion`; update only changed columns or add an
  optimistic-concurrency column checked in the `WHERE`.

### External effects inside a transaction; events outside it
- **What it is.** Two opposite mistakes. (1) Network I/O, a cross-gear call or other slow work inside an open
  transaction holds locks longer and repeats on every retry. (2) An event sent after `COMMIT` is lost if the
  process crashes in between.
  ```rust
  // BAD: external call inside the open transaction (holds locks for the whole call; repeated if the
  // closure is ever retried); event enqueued after COMMIT
  // (in_transaction_mapped returns (SecureConn, Result); see 11)
  let (_conn, res) = conn.in_transaction_mapped(DomainError::database_infra, move |tx| Box::pin(async move {
      vet.approve(&cat).await?;
      repo.insert(tx, &scope, &cat).await
  })).await;
  res?;
  events.enqueue(CatCreated { id }).await?;           // a crash before this line loses the event

  // GOOD: call out first; state change and outbox row in one transaction
  vet.approve(&cat).await?;
  let (_conn, res) = conn.in_transaction_mapped(DomainError::database_infra, move |tx| Box::pin(async move {
      repo.insert(tx, &scope, &cat).await?; outbox.insert(tx, &scope, &CatCreated { id }).await
  })).await;
  res?;
  ```
- **How to find it.** Search the closure and its callees for an injected client (`Arc<dyn Client>`); check the event
  write is in the same transaction.
- **How to catch it with a test.** Static: a source-text rule matching the client type inside the closure, with
  a negative control. Dynamic: on the write-plus-event trace assert `rec.all_in_one_transaction()` and
  `rec.writes_outside_tx().is_empty()` (the former ignores statements outside a transaction), and that
  `rec.stats()` contains the outbox `INSERT`.
- **Typical fix.** Call out before `BEGIN` (or after `COMMIT` if tolerant); write events to an outbox table in
  the same transaction and deliver them separately, at least once and idempotently.

### Retry wiring and non-idempotent retries
- **What it is.** `transaction_with_retry` re-runs the whole closure on PostgreSQL `40001`/`40P01`, MySQL
  deadlocks and SQLite `SQLITE_BUSY`; that is correct only if the closure is safe to re-run. Failures: a durable
  side effect outside the transaction repeats; a transaction that can abort (a deadlock, or `40001` under
  `SERIALIZABLE`) has no retry wrapper; the `DbErr` is unreachable when the classifier runs.
  ```rust
  // BAD: this path locks two cats, so it can deadlock (40P01); the abort reaches the caller as a failed request
  db.transaction_ref_mapped_with_config(TxConfig::default(), |tx| Box::pin(async move {
      repo.swap_kittens(tx, &scope, cat_a, cat_b).await
  })).await

  // GOOD: retry wrapper. 2nd arg is your fn(&DomainError) -> Option<&DbErr>;
  // it must still find the DbErr after every map_err in the chain
  db.transaction_with_retry(TxConfig::default(), |e: &DomainError| e.db_err(), |tx| Box::pin(async move {
      repo.swap_kittens(tx, &scope, cat_a, cat_b).await   // idempotent: safe to run again
  })).await
  ```
- **How to find it.** Grep both calls per file; a bare `transaction_ref_mapped_with_config` on a path that locks
  rows or runs `SERIALIZABLE`, beside retried siblings, is the tell.
- **How to catch it with a test.** Static scan of retried vs unretried calls, with a negative control; a round-trip
  test that passes the real contention error through the actual `map_err` chain into the classifier. The `DbErr`
  must stay reachable by `extract_db_err` through every `map_err`; a `.to_string()` anywhere in the chain turns
  the retry loop into dead code.
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
- **How to find it.** Classify each repository call in a loop as request- or data-bounded; check collection `IN`s
  use `toolkit_db::secure::max_bind_params_for` with headroom and reads have a `LIMIT` or keyset bound.
- **How to catch it with a test.** Scale-invariance at small and large N via `rec.stats()` (audit step 7);
  `rec.redundant_reads_after_write()` finds "insert, discard, re-read".
- **Typical fix.** One batched `INSERT .. VALUES` or `IN (..)`, chunked when the list follows the data; add
  `LIMIT`/keyset bounds; return what the write already gave back.

### Non-deterministic ORDER BY under offset pagination
- **What it is.** A list sorts by a non-unique column and paginates with `LIMIT`/`OFFSET` without a unique
  tie-breaker, so equal rows can swap order between pages: duplicates or skips, no error.
  The OData layer refuses `$skip` ([07](07_odata_pagination_select_filter.md)); this class is about hand-written
  `.order_by` with `.offset()` in repositories and sweeps.
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
  serves the sort.

### Missing indexes
- **What it is.** A hot query or FK column has no serving index: sequential scans, and cascading `DELETE`s that
  scan the whole child table. PostgreSQL uses a partial index only if the query's `WHERE` provably implies its
  predicate. Looks like: an unindexed FK; a partial index `WHERE status = 'x'` beside `status IN ('x', 'y')`.
- **How to find it.** Build the "query, serving index, hot or background path" table (audit step 3); check every FK.
- **How to catch it with a test.** A statement-count recorder cannot see it. Where a PostgreSQL harness can run
  `EXPLAIN`, assert an index scan; otherwise review `EXPLAIN ANALYZE` on production-scale data.
- **Typical fix.** A new migration, column order matching the predicate (equality first, sort last).

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
- **How to find it.** Grep `.count(` in repository code; a total should not exist
  (see [`07_odata_pagination_select_filter.md`](07_odata_pagination_select_filter.md)).
- **How to catch it with a test.** A static scan for `.count(` that allow-lists real aggregates, or assert the
  existence check's normalized SQL has `LIMIT` and no bare `COUNT(*)`.
- **Typical fix.** `.filter(predicate).one(runner)`, branch on the `Option`.

### SQLite / PostgreSQL / MySQL divergence
- **What it is.** One code path behaves differently per backend, and SQLite's permissiveness hides it:
  - **Locking.** MySQL has only `FOR UPDATE` and `FOR SHARE`/`LOCK IN SHARE MODE`, so a gear that also runs on
    MySQL uses only `LockType::Update`/`Share`; `NoKeyUpdate`/`KeyShare` are not translated and fail there with a
    syntax error. SQLite renders no lock: writers are serialized per database and a read-then-write transaction
    facing another writer fails with `SQLITE_BUSY` (`SQLITE_BUSY_SNAPSHOT` in WAL); both are retryable. A SQLite
    race test proves nothing about a lock.
  - **Errors.** Map unique and FK violations with `ScopeError::is_unique_violation()`/`is_foreign_key_violation()`
    (SQLSTATE first), not a hand-written message match; leave contention classification to `transaction_with_retry`.
  - **Types.** SQLite compares a UUID with `TEXT` silently (often matching nothing); PostgreSQL raises
    `operator does not exist: uuid = text`: bind a `Uuid`, not a `String`. Plain `json` has no equality operator
    on PostgreSQL (breaks `SELECT DISTINCT`): use `jsonb`.
- **How to find it.** Confirm each `.lock(..)` renders valid SQL on every backend the gear supports, each error
  `match` uses a code or variant, and each UUID/JSON predicate is exercised on a real engine.
- **How to catch it with a test.** Execute the predicate on the real engine, as in
  [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md#wire--storage-type-mismatch); only a
  real engine tells "no rows matched" from "the query would not run".
- **Typical fix.** Structured error matching; explicit casts; route lock- and type-sensitive cases to
  PostgreSQL.

### Migration hazards
- **What it is.** Schema-evolution mistakes:
  - editing a shipped migration;
  - a SQLite table rebuild (create new, copy, drop old, rename) with `foreign_keys = ON`, where `DROP TABLE`
    runs an implicit `DELETE` that fires `CASCADE`/`SET NULL` on children;
  - an undocumented no-op `down()`;
  - a new `NOT NULL`/`CHECK`/FK that existing rows violate, which passes on an empty test database.
  ```rust
  // BAD: no-op down() that reads as a working rollback
  async fn down(&self, _m: &SchemaManager) -> Result<(), DbErr> { Ok(()) }

  // GOOD: the no-op is documented, with what a real rollback would need
  /// No-op: `nickname` is `UNIQUE`, so SQLite cannot `DROP COLUMN` it; a real rollback rebuilds `cat`
  /// (create, copy, drop, rename) with child FKs in mind.
  async fn down(&self, _m: &SchemaManager) -> Result<(), DbErr> { Ok(()) }
  ```
- **How to find it.** The diff adds a migration, not edits one; trace child tables when a rebuilt table drops;
  each `down()` undoes `up()` or documents why not; ask what violating rows do.
- **How to catch it with a test.** Run `up()`, `down()`, `up()` on both backends, calling `down()` directly (the
  toolkit runner applies only `up()`), asserting data survives with correct types; a documented no-op `down()`
  is exempt. Check SQLite rebuilds in a real `sqlite3` session with the pragma set explicitly. Seed a violating
  row and confirm the staged path (add nullable, backfill, validate, tighten; *expand/contract*) succeeds or
  fails loudly.
- **Typical fix.** A new migration; a documented reason for any no-op `down()`; expand/contract on populated
  tables. For a SQLite rebuild, copy child rows out and back or rebuild the children too: `up()` runs inside a
  transaction, where `PRAGMA foreign_keys = OFF` is a no-op.

### Deadlocks and lock ordering
- **What it is.** Two operations lock the same two rows (or row sets) in opposite order and wait on each other;
  PostgreSQL aborts one with `40P01`. `transaction_with_retry` treats it like `40001`, so every transaction that
  can hit either must run inside it.
- **How to find it.** For operations that lock the same rows, by `UPDATE`/`DELETE`, `FOR UPDATE` or an FK check
  (`FOR KEY SHARE`), confirm one acquisition order (write rows one by one in id order, or lock them first with
  `SELECT .. ORDER BY id FOR UPDATE`), or that retry wraps both.
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
- **How to find it.** Does an unparseable column give `Err` or a logged default? A sibling mapper returning
  `Err` marks the defaulting one as an oversight.
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
  assert_eq!(CatEntity::find().filter(cat::Column::Name.eq("tom")).all(&db).await?.len(), 1);
  ```
- **How to find it.** Confirm CI sets a flag (your gear's choice, e.g. `<GEAR>_PG_REQUIRE_DOCKER=1`) that makes the
  PostgreSQL suite fail instead of silently skip without Docker; the barrier template shows the check. For each
  assertion ask whether it fails when the targeted bug is present; revert a fix to confirm.
- **How to catch it with a test.** Not mechanizable; the backstop is
  [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md#coverage-that-proves-nothing):
  every known defect has a named assertion; `#[ignore]`d ones are listed in the report's open findings.
- **Typical fix.** Fail closed in CI; assert table state and specific error variants; use barriers.

## How to run an audit

Tooling: `toolkit_db::test_support` behind the `test-support` cargo feature (enable it in the gear's
`[dev-dependencies]`). `rec` below is its `QueryRecorder`.

1. **Inventory.** List every table, every repository method issuing a statement, every place a transaction
   opens. Grep is enough.
2. **Transaction map.** Per state-changing operation: what runs before and inside the transaction (order,
   isolation), where external effects sit relative to `BEGIN`/`COMMIT`, and what a crash or a concurrent
   caller does between steps. This is the highest-yield step.
3. **Query-vs-index check.** For each `WHERE`/`ORDER BY`, find a serving index ([Missing indexes](#missing-indexes)).
4. **Loop check.** Find repository calls inside loops; classify as request- or data-bounded ([N+1](#n1-unchunked-in-missing-limit)).
5. **Trace and read.** Write one *trace test* per write operation: a normal `#[tokio::test]` that runs the
   operation once over a recorder-backed `Db` and records every statement:

   ```rust
   let (db, rec) = connect_with_recorder(&dsn, ConnectOpts { max_conns: Some(1), ..Default::default() }).await?;
   run_migrations(&db).await?;              // your gear's migrator
   rec.clear();                             // migrations were recorded too
   svc.create_cat(&ctx, input).await?;
   snapshot_trace("create_cat", &rec);      // writes <DB_AUDIT_TRACE_DIR>/create_cat.txt when the var is set
   ```
   Dump with `DB_AUDIT_TRACE_DIR=target/db-behavior-traces cargo nextest run -p <gear> --test <trace test file>`
   and read each `.txt` by eye before writing assertions.
6. **Assert the shapes.** Assert `rec.untransacted_read_modify_write().is_empty()` and
   `rec.untransacted_write_runs().is_empty()` (advisory: a hit is a defect only if the write relies on what came
   before it) and, for write paths that must not re-read what they wrote,
   `rec.redundant_reads_after_write().is_empty()`. A lone write outside a transaction is fine. For an operation that
   must be one transaction assert `rec.all_in_one_transaction()` together with `rec.writes_outside_tx().is_empty()`,
   since the former ignores statements outside a transaction. Add `rec.all_in_serializable_transaction()` only on a
   fix-(c) path and its sibling writers. Both `all_in_*` are `false` when nothing ran in a transaction. The recorder
   cannot see transaction-boundary SQL; `all_in_serializable_transaction()` checks the level the code *requested*,
   and SQLite runs serializable regardless. Put `rec.dump()` in the assertion message.
7. **Scale-invariance.** Run the same operation at N=2 and N=50 (`rec.clear()` between) and compare `rec.stats()`:
   statement counts must be identical unless a list is chunked. `rec.total_params()` may grow with N wherever one
   statement binds a list (a multi-row `INSERT .. VALUES`, an `IN (..)`), by a constant per row. A chunked list adds
   a statement each time N crosses the chunk size, `ceil(N / chunk_size)` in all; to exercise it, run a case above
   the chunk size (or make the chunk size a parameter the test can lower) and assert that count and that each
   `param_count` in `rec.events()` is at most `max_bind_params_for`.
8. **Static rules for what SQL cannot show.** Retry wiring, external calls in transactions and `COUNT` use are
   not in a trace: write a `#[test]` over `include_str!`'d source that fails on the forbidden pattern, plus a
   *negative control* running the same check on a deliberately bad string and expecting it to fail.
9. **Barrier tests** (PostgreSQL): for each TOCTOU/CAS/deadlock finding start two callers at once with the
   template below and assert the table state afterwards.
10. **Pin every known defect** as a named assertion for the *correct* behaviour, `#[ignore = "known defect: <ID>"]`;
    the fix removes the `#[ignore]`. If it cannot be asserted directly, assert today's behaviour and say in a
    comment which assertion to flip.

### Barrier-test template

`shared_pg()` (one `testcontainers` PostgreSQL per test file via `OnceCell`, `None` without Docker), `svc1`/`svc2`
(two services over the same `pg.db`) and `assert_invariant_holds` are the gear's own helpers, not toolkit APIs.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_leave_the_invariant_intact() {
    let Some(pg) = shared_pg().await else {   // None when Docker is unavailable
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

The barrier only starts both calls together; it cannot force both to read before either writes, so a passing
run proves nothing and a failing run proves the bug. To force the bad interleaving, add a test-only hook between
the read and the write (an injected callback, or one behind a test-only cargo feature; `#[cfg(test)]` code is not
built for `tests/` integration tests) and run the second writer inside it. Use the plain barrier for likely races,
the hook to reproduce one deterministically.

## Where to keep which test

| Class | Test level | Dialect | Guide |
|---|---|---|---|
| Check-then-act / TOCTOU shape | SQLite unit test, query recorder | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup |
| The race behind a TOCTOU shape | Barrier test (can fail, cannot prove) | PostgreSQL only | this doc |
| CAS / `rows_affected` / lost update (sequential) | SQLite unit test | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup |
| CAS under real concurrency | Barrier test | PostgreSQL only | this doc |
| External call / event-in-tx | Static scan + recorder (`all_in_one_transaction`, `writes_outside_tx`) | SQLite suffices | this doc |
| Retry wiring / error-shape preservation | Static scan + unit round-trip test | SQLite suffices | this doc |
| N+1 / scale-invariance | Query recorder, small-N vs large-N | SQLite suffices | this doc |
| Pagination tie-breaker | SQLite unit test (forced duplicate sort key) | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup, [`13_e2e_testing.md`](13_e2e_testing.md) for the HTTP cursor roundtrip |
| Missing index / plan shape | `EXPLAIN`-backed integration test, or manual review | PostgreSQL only | this doc |
| `COUNT`-for-existence | Static scan | N/A | this doc |
| Backend divergence (types, locking, errors) | Feature-gated Rust suite via `testcontainers`, in-process, no HTTP | PostgreSQL (and MySQL where supported) | [`13_e2e_testing.md`](13_e2e_testing.md#coverage-goal-one-call-per-api-method) (which PostgreSQL suite to use) |
| Migration correctness (rebuild, `down()`, expand/contract) | Migration test on both backends | Both; SQLite rebuild also needs a raw `sqlite3` check | [`11_database_patterns.md`](11_database_patterns.md#database-migrations) |
| Deadlock / lock ordering | Barrier test, looped | PostgreSQL only | this doc |
| Mapper defaulting | SQLite unit test (garbage column value) | SQLite suffices | this doc; [`12_unit_testing.md`](12_unit_testing.md) for test setup |

Needs HTTP and a real database: E2E (13). Needs real PostgreSQL/MySQL but no HTTP: a feature-gated Rust suite
inside the gear, outside the 12/13 split; name it in the gear's own testing doc. Anything else: SQLite unit
test (12).

## Audit report template

Keep the catalog and method here and record only gear-specific findings in the gear's report. Mirror this
skeleton:

```markdown
# DB behavior audit — <gear name>
## Scope
Code, branch or PR covered, and what is deliberately excluded.
## What was found
Table: ID | class | severity | where (repository/service, no line numbers) | status and reason.
## Open findings
Each finding not fixed in this change: ID, why, and the condition for revisiting.
## Transaction-behaviour findings
Narrative on isolation and retry choices only; the rows stay in the table above.
## How it was found
Mechanisms used (recorder, scale-invariance, static scans, barrier suite) and how they were validated.
## Deviation from the unit/E2E testing guide
Suites outside the 12/13 split, and why.
## What this does not cover
Statement cost, transaction duration, `EXPLAIN` plans, whatever was not tested: named, not implied.
```

## Related documents

- [`11_database_patterns.md`](11_database_patterns.md): transaction execution mechanics and the repository pattern.
- [`12_unit_testing.md`](12_unit_testing.md): the sequential, SQLite-backed half of DB tests.
- [`13_e2e_testing.md`](13_e2e_testing.md): HTTP-level cursor roundtrips; why concurrency under the `Service ↔ PostgreSQL` seam is not E2E's job.
- [`16_defect_class_to_control_map.md`](16_defect_class_to_control_map.md): the whole-gear defect-to-control map this document specializes.
