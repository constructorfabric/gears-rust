---
status: accepted
date: 2026-07-24
decision-makers: Constructor Fabric steering committee
---

# ADR-0001: Safe CTE Support in the Secure ORM

**ID**: `cpt-cf-adr-secure-cte-policy`

## Context and Problem Statement

The Secure ORM (`libs/toolkit-db/src/secure/`) enforces multi-tenant isolation
through a single load-bearing invariant: **every access to a table passes through a
scope condition**. This is guaranteed mechanically, not by convention:

- A typestate transition `Unscoped → Scoped` — a query physically cannot be executed
  until `.scope_with()` is called
  ([select.rs:151-188](../../../../libs/toolkit-db/src/secure/select.rs#L151-L188)).
  The `Scoped` marker carries an `Arc<AccessScope>` so related-entity queries inherit
  the same scope
  ([select.rs:22-25](../../../../libs/toolkit-db/src/secure/select.rs#L22-L25)).
- `build_scope_condition::<E>()` attaches a `WHERE` for the concrete entity `E` via
  `E::resolve_property()`
  ([cond.rs:54-83](../../../../libs/toolkit-db/src/secure/cond.rs#L54-L83)).

A Common Table Expression (`WITH x AS (SELECT ... FROM sensitive_table) ...`) breaks
this invariant. The body of a CTE is an **independent `SELECT` over arbitrary tables**
to which the outer query's scope `WHERE` does **not** apply. If a gear could build a
CTE freely, the scope filter would land on the outer entity while the tables read
inside `WITH` stay unfiltered — a direct tenant-isolation hole.

The naive workaround — "expose `into_inner()` and assemble the CTE by hand on
sea_query"
([select.rs:415-418](../../../../libs/toolkit-db/src/secure/select.rs#L415-L418)) —
also violates the platform guardrails. The rule **"No plain SQL in
handlers/services/repos. Raw SQL is allowed only in migration infrastructure"** is
explicit and repeated in
[11_database_patterns.md:9](../../../toolkit_unified_system/11_database_patterns.md#L9)
(and again at lines 161 and 167). Raw SQL escapes both the typestate guarantee and
this rule.

There is real, tracked demand for capabilities that today would reach for a CTE:

- `account-management/.../tr_plugin/queries.rs:26-39` builds a parent map in memory and
  walks it pre-order on the client, explicitly deferring the SQL recursive CTE "as a
  follow-up once `toolkit-db` exposes a safe raw-SQL hook".
- `account-management/.../repo_impl/conversion.rs:1221` recomputes barriers per row and
  notes "Replace with self-join on `tenant_closure` once the recursive-CTE work lands".

**Scope of this ADR.** This policy governs **standard user gears** — the
handlers / services / repos that build queries against their own domain tables. It
does **not** govern `toolkit-db` itself. The one pre-existing CTE in the crate is the
outbox writer's: the raw dialect-specific `WITH` strings live in
[outbox/dialect.rs:130-145](../../../../libs/toolkit-db/src/outbox/dialect.rs#L130-L145)
(`outbox/store.rs` is only the caller). That is raw SQL **inside the system library that
implements the Secure ORM**, where sea_query and hand-written SQL are the implementation
substrate. It is correct, out of scope here, and not a precedent for user-gear code.
(Note: the outbox is no longer behind a preview feature flag, but that changes nothing
about this boundary — it was never user-gear code.)

Today there is **no CTE support in the Secure ORM's user-facing API** and no policy
governing it for user gears. This ADR settles the policy and the API shape **before**
ad-hoc solutions accrete in gears.

## Decision Drivers

- **Preserve the typestate invariant** — a CTE must be as impossible to construct
  unscoped as an `Unscoped` select is impossible to execute.
- **No raw SQL in user-gear code** — honor
  [11_database_patterns.md:9](../../../toolkit_unified_system/11_database_patterns.md#L9);
  in gears everything goes through the Secure ORM / sea_query builder (the `toolkit-db`
  system library that implements it is a separate layer).
- **Reuse what exists** — `build_scope_condition` already emits nested subqueries via
  `sea_query::Query::select()`
  ([cond.rs:104-185](../../../../libs/toolkit-db/src/secure/cond.rs#L104-L185)); the
  "scope as a sea_query expression" mechanic is proven.
- **Do not regress hierarchy handling** — hierarchy traversal already works via
  materialized closure tables (`tenant_closure`, `resource_group_closure`); any recursion
  offered must be covered by the isolation model, not an exception to it.
- **Reviewability** — any escape from the safe path must be visible at the API surface
  so a reviewer sees that isolation was considered.

## Considered Options

- **Option A** — a CTE type constructible only from `SecureSelect<E, Scoped>`; scope is
  embedded inside each CTE body.
- **Option B** — Controlled escape-hatch: a CTE over a non-`Scopable` source with a
  mandatory `.scope_via_exists::<J>()` join predicate to a scoped entity.
- **Option C** — Raw `WITH` from a string in gear code.
- **Option D** — `WITH RECURSIVE` for hierarchy traversal.

## Decision Outcome

Chosen option: **Option A**, because it is the only option that keeps the
tenant-isolation invariant a **compile-time** guarantee while using the sea_query
builder exclusively. The principle: **do not apply scope outside the CTE — embed scope
inside the body of every CTE.** Then any table a CTE touches is already filtered.

### API shape

**As implemented** ([`libs/toolkit-db/src/secure/cte.rs`](../../../../libs/toolkit-db/src/secure/cte.rs)).
This is a builder rather than the free-standing `into_cte(name)` → `with_ctes(ctes)`
pair originally sketched. Reason: a free-standing `SecureCte` value can be built from
query A and passed to query B, which is exactly the scope mismatch the structural design
exists to make unrepresentable — closing that hole would have required the runtime
check enforcement option 1 exists to avoid. Tying construction to the outer query
removes it with no check and no `Result`.

```rust
// 1. A CTE query is reachable ONLY from an already-scoped select, which is what
//    carries the `Arc<AccessScope>` every body inherits.
impl<E: EntityTrait> SecureSelect<E, Scoped> {
    pub fn with_ctes(self) -> SecureCteSelect<E>;
}

// 2. Bodies are registered on the builder and scoped with the outer query's own
//    Arc, so a differently-scoped CTE cannot be constructed -- nothing to compare,
//    no error case. `SecureCteSelect` wraps a `sea_query::SelectStatement` +
//    `WithClause` and does NOT reuse `Select<E>`'s execution path, so a `WITH`
//    clause cannot silently vanish at execution time.
impl<E: EntityTrait> SecureCteSelect<E> {
    /// Non-recursive CTE over `J`. Scope is applied *after* `body` returns, so a
    /// body cannot filter the scope predicate back off.
    pub fn cte<J: ScopableEntity + EntityTrait>(
        self,
        name: &'static str,
        body: impl FnOnce(sea_orm::Select<J>) -> sea_orm::Select<J>,
    ) -> Self;

    /// Recursive CTE. See "Recursive CTE" below.
    pub fn recursive_cte<J: ScopableEntity + EntityTrait>(self, spec: RecursiveCte<J>) -> Self;

    /// Reference a registered CTE from the outer query. Without this the `WITH`
    /// clause is valid SQL that computes nothing.
    pub fn join_cte(self, name: &'static str, on: Condition) -> Self;

    pub fn filter(self, filter: Condition) -> Self;
    pub fn limit(self, limit: u64) -> Self;
    /// `join_cte` is an inner join, so an outer row repeats per matching CTE row.
    /// Needed whenever the CTE is a membership set rather than a 1:1 join —
    /// e.g. expanding a breadth-first frontier where edges converge on a node.
    pub fn distinct(self) -> Self;

    /// Ordering is split by column provenance, because the two cases differ in
    /// validity under `DISTINCT` and the type cannot otherwise tell them apart.
    /// An entity column is always in the outer `SELECT` list; a CTE column is not,
    /// and PostgreSQL/`MySQL` both reject `ORDER BY` on an expression missing from
    /// a `SELECT DISTINCT` list (`SQLite` accepts it, so the mistake would only
    /// surface in production). Combining `distinct()` with `order_by_cte()`
    /// therefore fails with `ScopeError::Invalid` on every backend.
    pub fn order_by(self, col: E::Column, order: Order) -> Self;
    pub fn order_by_cte(self, cte: &'static str, col: &'static str, order: Order) -> Self;

    /// The outer query inherits `Select<E>`'s projection — every column of `E` —
    /// and `all_as::<T>()` narrows only deserialization, not the SQL. On Postgres
    /// that is the difference between an index-only scan and a heap visit per row.
    pub fn select_only(self) -> Self;
    pub fn column(self, col: E::Column) -> Self;
    pub fn column_from_cte(self, cte: &'static str, col: &'static str, alias: &'static str) -> Self;
    pub fn expr_as(self, expr: Expr, alias: &'static str) -> Self;

    // Execution: `FromQueryResult::find_by_statement`, not `Select<E>::all()`.
    pub async fn all(self, runner: &impl DBRunner) -> Result<Vec<E::Model>, ScopeError>;
    pub async fn one(self, runner: &impl DBRunner) -> Result<Option<E::Model>, ScopeError>;
    /// CTEs usually compute something the entity model has no shape for
    /// (aggregates, window functions), so a custom projection is the common case.
    pub async fn all_as<T: FromQueryResult>(self, runner: &impl DBRunner) -> Result<Vec<T>, ScopeError>;

    /// Render without executing. Exists because the crate has no mock database,
    /// so this is the only way a test can assert on the emitted SQL.
    #[doc(hidden)]
    pub fn build_statement(&self, backend: DbBackend) -> Statement;
}
```

The invariant this yields:

- A CTE query **cannot** be started from an `Unscoped` select → the compiler forbids
  unwrapped table access inside `WITH`. Pinned by the compile-fail test
  `tests/ui/fail/cte_from_unscoped.rs`.
- The outer query is itself `Scoped` on its own root entity.
- The `Scopable` requirement is not a separate check: `SecureSelect<E, Scoped>` is only
  reachable through `scope_with`/`scope_with_arc`, which are bounded `E: ScopableEntity`
  ([select.rs:151-155](../../../../libs/toolkit-db/src/secure/select.rs#L151-L155)). So
  `with_ctes` is reachable only from a `Scoped` select, and `cte`/`recursive_cte` carry
  `J: ScopableEntity` for the body entity, so a non-`Scopable` entity can never reach a
  CTE.
- `with_ctes` returns `SecureCteSelect`, a type that does **not** reuse `Select<E>`'s
  execution path, so the API shape and the "Feasibility constraint" section below agree
  — there is no way to build a CTE query that then executes as a plain `Select<E>` and
  drops the `WITH`.
- No raw SQL — everything flows through the sea_query builder, exactly as the scope
  subqueries in [cond.rs](../../../../libs/toolkit-db/src/secure/cond.rs) already do.

### Referencing a CTE

`with_ctes` only *attaches* the CTE definitions; the outer query must still reference
each one by name to use it. Level A's contract: the outer query reads a CTE as a named
source (`FROM`/`JOIN` on the CTE's `&'static str` name), and the predicate tying it to
the outer entity is the caller's responsibility — this is the correctness risk under
Risks, not an isolation one. The precise typed reference helper is an implementation
detail, out of scope for this ADR.

### CTE name

`cte`/`recursive_cte` take `&'static str`, not `&str` — an ergonomic nudge toward string
literals, not a security control (a `&'static str` can still be produced at runtime by
leaking). Injection safety does not depend on it: the name goes through sea_query's
`Alias`, which renders it as a quoted, escaped identifier.

The pinned version is **sea_query 1.0.2** (pulled by sea-orm 2.0.2 since commit
`5446288b`), where the mechanism is a two-branch gate rather than unconditional
escaping — worth stating precisely, because only one branch escapes:

- `QuotedBuilder::prepare_iden` (`sea-query-1.0.2/src/backend/mod.rs:44-61`) doubles the
  quote char **only for `Cow::Owned`**; a `Cow::Borrowed` is written verbatim.
- `Alias::new(name)` always yields `Cow::Owned`, so it is always escaped. A `&'static str`
  used directly as an `Iden` yields `Cow::Borrowed` **only if `is_static_iden()` passes**
  (`src/types/iden/core.rs:228`, i.e. `[A-Za-z_][A-Za-z0-9_]*`), and otherwise falls back
  to `Cow::Owned` — so hostile input is escaped on that path too.

Quote chars: `"` for Postgres and SQLite, `` ` `` for MySQL. So a hostile name becomes an
inert quoted identifier, never executable SQL — pinned by the hostile-name tests (see
Confirmation), which assert the *structural* property that the name occupies exactly the
identifier slot of one CTE definition, not merely that the payload appears somewhere.

The implementation must therefore route names through `Alias::new(...)` and never
`format!` them into SQL. A gear that needs a truly dynamic name is out of scope for
Level A (escape-hatch, Level B); validate against an allowlist there.

### Same-scope constraint

The outer query and **every** supplied `SecureCte` must carry the **same**
`AccessScope`. Combining CTEs built from different scopes (e.g. two tenants) in one query
is disallowed: each CTE body is still individually safe (its own scope is embedded), but
a single query mixing scopes is semantically incoherent and exactly the kind of ad-hoc
pattern this ADR exists to prevent.

**Resolved: option 1, structurally.** `with_ctes()` captures the outer query's
`Arc<AccessScope>` and every body registered on the builder is scoped from it. A
differently-scoped CTE is unrepresentable, so there is nothing to compare, no runtime
check, and no error case — `with_ctes` does not return `Result`. The options considered
and *not* taken, recorded so they are not revisited:

2. **Hard runtime check.** Comparing the outer `AccessScope` against each CTE's by value
   (`AccessScope: PartialEq`,
   [access_scope.rs](../../../../libs/toolkit-security/src/access_scope.rs)) and returning
   `Err` on mismatch — **in all build profiles**. Unnecessary given option 1. Note:
   `debug_assert!` would **not** have been acceptable; it is compiled out in release
   builds and would leave a tenant-isolation invariant unchecked in production.
3. **dylint (complementary, not a substitute).** A custom lint can enforce the *syntactic*
   Level-C prohibitions (no raw `WITH` string, no `into_inner`/`into_query` in
   handlers/services/repos), but it **cannot** verify that two runtime `AccessScope`
   *values* are equal — that is a value-equality/data-flow problem it cannot decide.
   Do not rely on dylint for the same-scope guarantee.

### Feasibility constraint (must be honored by the implementation)

`SecureSelect.inner` is a **`sea_orm::Select<E>`**, not a `sea_query::SelectStatement`
([select.rs:60-65](../../../../libs/toolkit-db/src/secure/select.rs#L60-L65)).
Execution goes through `self.inner.all()/one()/count()`
([select.rs:200-232](../../../../libs/toolkit-db/src/secure/select.rs#L200-L232)), and
the only public unwrap is `into_inner() -> Select<E>`
([select.rs:415-418](../../../../libs/toolkit-db/src/secure/select.rs#L415-L418)) —
there is no `into_query()`. Critically, **`sea_orm::Select<E>` has no `.with()`
method**; `WithClause`/`CommonTableExpression` live on `sea_query`. Therefore
the CTE API cannot be a drop-in over `inner`. This still holds in sea-orm 2.0.2. How the
implementation satisfies it:

- `cte`/`recursive_cte` call `QueryTrait::into_query()` on the `Select<J>` to obtain a
  `sea_query::SelectStatement` (scope `WHERE` already embedded) and wrap it in a
  `sea_query::CommonTableExpression`.
- `SecureCteSelect` holds the outer query as a `sea_query::SelectStatement`, attaches
  `.with(WithClause)` to get a `WithQuery`, and executes through a **separate path** —
  **not** `Select<E>::all()`.
- Two corrections to the original sketch. `find_by_statement` is a method on
  **`FromQueryResult`**, not `EntityTrait` (`sea-orm-2.0.2/src/entity/model.rs:214`), so
  the call is `E::Model::find_by_statement(stmt)`. And `StatementBuilder` *is* implemented
  for `sea_query::WithQuery` (`sea-orm-2.0.2/src/database/statement.rs:134`) via
  `from_string_values_tuple`, so the statement carries **bound parameters** and no manual
  per-backend `match` is needed.
- One gap this exposed: `DBRunner` is sealed and method-free, so there was no way to learn
  the backend in order to render. Added `pub(crate) SeaOrmRunner::backend()`, mirroring
  `outbox/core.rs:408-413`, which keeps the trait method-free and leaks no SeaORM type.

This is a design requirement, not an optional detail: an implementer who assumes
`inner.with(...)` exists will hit a type wall.

### Levels of strictness

- **Level A (safe, this decision)** — `cte`/`recursive_cte`, reachable only from a scoped
  select and only over `Scopable` entities. Covers the great majority of real needs
  (aggregations, dedup, window functions over an intermediate scoped set, and hierarchy
  traversal with a depth cap).
- **Level B (future, controlled escape-hatch)** — CTE over a non-`Scopable` source but
  with a **mandatory** `.scope_via_exists::<J>()` predicate to a scoped entity.
  Out of scope here; recorded so it is not reinvented ad hoc.
- **Level C (forbidden in user gears)** — raw `WITH` from a string, recursive or not. Not
  allowed in standard user-gear code (handlers/services/repos): nothing then guarantees the
  body is scoped, nor — for a recursive `WITH` — that the walk terminates. Migrations and
  the system libraries that implement the Secure ORM (`toolkit-db` internals, e.g. the
  outbox writer) are a different layer and are not governed by this policy.
  This prohibition is *syntactic* and can therefore be enforced mechanically rather
  than by reviewer vigilance: a custom [dylint](https://github.com/trailofbits/dylint)
  lint can flag raw `WITH` strings and any `into_inner()`/`into_query()` unwrap reaching
  a raw-SQL sink in gear crates, and fail CI. (A lint can enforce these syntactic rules;
  it cannot verify the runtime same-scope invariant — see "Same-scope constraint".)

### Consequences

**Positive:**

- Tenant isolation stays a compile-time guarantee; a CTE is as impossible to build
  unscoped as an unscoped select is to execute.
- sea_query-only; no new raw-SQL surface in gear code — the guardrail holds.
- Reuses the proven `build_scope_condition` subquery mechanic.

**Negative:**

- Requires a CTE-aware execution path that bypasses `Select<E>` (via
  `find_by_statement`/`FromQueryResult`), diverging from the existing `.all()` path.
- The CTE body is scoped, but the outer query cannot post-filter the CTE's contents —
  the scope must be correct at body-build time.
- Recursive traversal requires the caller to state a depth bound. There is no safe
  default: `CYCLE` is not portable, so an unbounded walk over a cyclic graph would not
  terminate.
- Recursive results depend on the dedup mode: `Union` (default) discards rows
  duplicating ones already produced at the same depth; `UnionAll` keeps every row
  (see "Recursive CTE"). Neither is a visited set -- a node can still recur at a
  greater depth.

**Risks:**

- The correlation between a CTE and the outer query (e.g. joining the CTE to the outer
  entity on the right tenant key) is **not** compiler-verified. This is a **correctness**
  risk, not an isolation risk: because every CTE body embeds its own scope via
  `build_scope_condition`, a wrong join predicate can only under- or over-select
  *rows* (missing/duplicate results) — it **cannot** leak another tenant's data, since the
  CTE body never contained that data to begin with. The isolation boundary is
  per-CTE-body and independent of outer-join correctness. Mitigate the correctness risk
  with tests and reviewer guidance. (If a future change ever lets CTEs from different
  scopes be combined in one query, that would be a separate, real isolation risk — see the
  "Same-scope constraint" above, which forbids it.)
- The CTE name cannot inject SQL regardless of contents: sea_query renders it as a
  quoted, escaped identifier (`Alias`/`Iden`). `&'static str` is only an ergonomic guard
  toward literals (see "CTE name"), not the injection defense.

### Confirmation

Done ([`cte_tests.rs`](../../../../libs/toolkit-db/src/secure/cte_tests.rs),
[`tests/sqlite/secure_cte.rs`](../../../../libs/toolkit-db/tests/sqlite/secure_cte.rs),
[`tests/ui/fail/cte_from_unscoped.rs`](../../../../libs/toolkit-db/tests/ui/fail/cte_from_unscoped.rs)):

- Scope present in the body of **every** CTE, asserted against the built SQL per backend
  (Postgres / MySQL / SQLite). Unlike the existing scope-condition tests
  ([cond.rs:191+](../../../../libs/toolkit-db/src/secure/cond.rs#L191)), which match on the
  `Debug` form of a `Condition`, these assert on the rendered statement — a `Condition`
  that never reaches the SQL would satisfy a Debug check and still leak.
- Execution-path test: the emitted statement starts with `WITH`, so a regression that fell
  back to the plain `Select<E>` path (silently dropping the CTE) is caught.
- Hostile-name test per backend: `foo"; DROP TABLE x --` must render as a single quoted,
  escaped identifier. Asserted **structurally** — the name occupies exactly the identifier
  slot of one CTE definition — rather than by searching for the payload, because a
  correctly escaped name still *contains* the dangerous substring.
- Compile-fail test: `with_ctes()` on an `Unscoped` select does not compile.
- Recursive: `WITH RECURSIVE` emitted, scope predicate present in **both** members, depth
  cap emitted and reflecting the caller's bound. Live SQLite tests cover tenant isolation,
  subtree traversal, depth truncation, and termination on a cyclic graph.

Two traps worth recording, because both produced tests that passed against deliberately
broken code before being fixed:

1. **Counting the column name is not counting the predicate.** `tenant_id` appears in every
   member's `SELECT` list, so `contains("tenant_id")` passes even on a completely unscoped
   member. Assert on the `tenant_id IN (` form.
2. **The outer query masks a leaking CTE body.** Joining a CTE back on `id` re-filters
   through the outer query's own scope predicate, which drops foreign rows before they are
   observable — so that query shape cannot detect an unscoped body at all. To observe the
   body, pin the outer query to one row and join the CTE on a tautology, making the CTE's
   size visible in the row count.

Remaining:

- Codify the prohibition on raw and recursive CTEs in gear code in
  [11_database_patterns.md](../../../toolkit_unified_system/11_database_patterns.md),
  alongside the existing "No plain SQL" rule. **Done.**
- Back the Level-C prohibition with a custom `dylint` lint in CI (flag raw `WITH`
  strings and `into_inner()`/`into_query()` reaching a raw-SQL sink in gear crates), so
  the guardrail is machine-checked rather than left to review. **Not done** — the lint
  crate lives in the separate `cargo-gears` repository
  (`crates/cargo-gears-lints`, closest analog `de07_security/de0706_no_direct_sqlx.rs`),
  so this is a cross-repo change. Note it already has a target: the only current
  `into_inner()`→`into_query()` chain in gear code is
  `account-management/.../repo_impl/reads.rs:512-518`. The same-scope invariant is *not*
  covered by the lint and is enforced structurally instead.

## Recursive CTE (`WITH RECURSIVE`)

**Accepted, with a mandatory depth cap.** This reverses the original rejection, whose
stated reason does not hold.

The rejection claimed "scope cannot be embedded into the recursive step statically". It
can. A recursive member is not a query over the CTE alone — it reads the **real table**
joined against the CTE self-reference, so `build_scope_condition::<J>()` applies to it
exactly as it does to the seed. Both members can, and do, carry the same
`Arc<AccessScope>`:

```sql
WITH RECURSIVE n AS (
    SELECT j.link_col, j.anchor_col, 0 AS __cte_depth FROM j
     WHERE <seed> AND <scope>
    UNION                                            -- RecursiveDedup, UNION by default
    SELECT j.link_col, j.anchor_col, n.__cte_depth + 1 FROM j
      JOIN n ON j.link_col = n.anchor_col
     WHERE <scope> AND n.__cte_depth < <max_depth>   -- library-emitted, not optional
)
```

The body projects **three columns, not the whole row**. A walk needs only the two
link columns and the depth; the outer query supplies full rows by joining back. That
is not merely narrower — under `UNION` the dedup compares every selected column, and
PostgreSQL's `json` type has no equality operator, so selecting a `ColumnType::Json`
column here would fail with *"could not identify an equality operator for type json"*
before the traversal ran. The same constraint applies to `distinct()` on the outer
query, which is why the projection controls exist.

The real hazard is different, and was not named in the original rejection: **cycles**.
`sea_query` renders PostgreSQL's `CYCLE` clause but its MySQL and SQLite builders
implement `prepare_with_clause_recursive_options` as **empty no-ops**
(`sea-query-1.0.2/src/backend/mysql/query.rs:139-140`,
`src/backend/sqlite/query.rs:65-66`), so `CYCLE` cannot be relied on portably — a cyclic
graph would recurse until the server gave up. Hence `RecursiveCte::max_depth` has no
default and no `Option`: the caller must state a bound, and the library emits it as a
predicate on the recursive member. That is the termination guarantee.

**Path explosion is a choice, not an inherent limit.** An earlier draft of this ADR
stated that the walk necessarily enumerates paths and was therefore unsuited to
general graphs. That was a consequence of emitting `UNION ALL`, not of recursion.
Plain `UNION` in a recursive CTE discards rows duplicating ones already produced —
standard SQL, available on PostgreSQL, MySQL 8+ and SQLite. Since the row carries its
depth, dedup is per (row, depth), so re-expansion is bounded by *(rows × depth)*
rather than by path count; measured on a hub-heavy 200k-node / 660k-edge profile at
p95 4.06 ms by the Graph Storage team, and reproduced here on a converging-path
fixture at 50 rows against 240 for `UNION ALL`.

`RecursiveDedup::Union` is therefore the default, with `UnionAll` available for the
sparse parent-pointer case where comparing rows is wasted work. Neither is a visited
set: a node can still be revisited at a greater depth, only not re-emitted
identically. `distinct()` deduplicates the final rows but does not prune the walk.

**Shape limit: one self-referencing table.** The recursive member joins `J` to the CTE
on `J.link_col = cte.anchor_col`, so both endpoints of a hop must be columns of the
same entity — an edge table (`src`, `dst`) or a parent-pointer tree
(`parent_id`, `id`). A hop *through* a separate table (`node -> edge -> node`) needs a
three-way join in the recursive member and is not expressible; use one scoped query
per hop instead. The column pair is named `link_col`/`anchor_col` rather than
child/parent because the traversal is directed-edge, not necessarily hierarchical.

Two further consequences worth stating plainly:

- **Cost.** If the scope is `InTenantSubtree` (itself a subquery over `tenant_closure`),
  embedding it per hop re-evaluates it at every recursion level. Closure tables remain the
  better choice for *hot*, frequently-traversed hierarchies — see below — but they are no
  longer a *precondition* for traversing one.
- **Dedup is per depth, not per node.** `Union` (default) discards a row duplicating one
  already produced at the same depth, so re-expansion is bounded by *(rows x depth)* rather
  than by path count. It is not a visited set: a node reachable by two paths of different
  length is still expanded at each depth it is reached. `UnionAll` keeps every row and
  therefore enumerates full paths. Either way the depth cap guarantees termination, not
  node-level uniqueness; callers wanting distinct nodes should dedup the final rows with
  `distinct()` (after narrowing the projection with `select_only()` -- see
  "API shape") or `GROUP BY`.

### Closure tables: still preferred for hot hierarchies, no longer required

The platform materializes transitive closure on write rather than computing it on read,
and that remains the right default for hierarchies that are traversed often:

- `tenant_closure` (`ancestor_id, descendant_id, barrier, descendant_status`) is
  maintained incrementally on tree changes
  ([tenant/closure.rs](../../../../gears/system/account-management/account-management/src/domain/tenant/closure.rs)),
  with self-row / barrier invariants enforced by check constraints
  (`ck_tenant_closure_self_row_barrier`) and covered by
  `idx_tenant_closure_ancestor_barrier_status`.
- `resource_group_closure` follows the same incremental pattern.
- `InTenantSubtree` / `InGroupSubtree` compile to a **flat** subquery over the closure
  table (`col IN (SELECT descendant_id FROM ... WHERE ancestor_id = ?)`), not recursion
  ([cond.rs:121-185](../../../../libs/toolkit-db/src/secure/cond.rs#L121-L185)).

The driver for this ADR is a **new domain hierarchy not yet in the closure model.** For
such a hierarchy there are now two legitimate routes, and the choice is a cost question
rather than a safety one:

- **Traversed often → extend the closure model.** Add a closure table maintained
  incrementally on write (self-row + one strict-ancestor hop per edge) with the same
  self-row / status / barrier-style invariants and a covering index, plus a new
  `ScopeFilter` variant and a `build_scope_condition` branch modeled on
  `InTenantSubtree` / `InGroupSubtree`. Traversal then becomes part of the scope
  condition and inherits tenant isolation for free, and reads stay flat.
- **Traversed rarely, or the tree changes constantly → `recursive_cte`.** No migration, no
  write-path maintenance. Pay per-read recursion instead, and accept the per-hop cost of a
  subquery-based scope filter.

Neither route requires the other. What is *not* acceptable is a hand-written recursive
`WITH` in gear code (Level C), because nothing then guarantees the recursive member is
scoped or that the walk terminates.

## Pros and Cons of the Options

### Option A: `SecureCte` from `SecureSelect<E, Scoped>`

- Good, because scope is embedded in the CTE body — isolation is a compile-time
  guarantee.
- Good, because it is sea_query-only; no raw SQL enters gear code.
- Good, because it reuses `build_scope_condition` and the existing subquery mechanic.
- Bad, because it needs a new execution path (`find_by_statement`/`FromQueryResult`)
  distinct from `Select<E>::all()`.
- Bad, because CTE↔outer correlation is not compiler-verified (test/review burden).

### Option B: Escape-hatch with mandatory `scope_via_exists`

- Good, because it covers CTEs over non-`Scopable` sources while keeping an explicit,
  reviewable isolation predicate.
- Bad, because correctness depends on the developer choosing the right join entity;
  weaker than Level A's structural guarantee.
- Deferred: recorded as a future level, not implemented now.

### Option C: Raw `WITH` from a string

- Good, because maximally flexible.
- Bad, because it discards the typestate guarantee entirely and violates the "no plain
  SQL outside migrations" rule
  ([11_database_patterns.md:9](../../../toolkit_unified_system/11_database_patterns.md#L9)).
- Rejected for user-gear code. (The outbox writer's raw CTE is unaffected: it is a
  `toolkit-db` internal — a system library, not user-gear code.)

### Option D: `WITH RECURSIVE` for hierarchies

**Accepted alongside Option A**, as `recursive_cte`. Originally rejected on a premise that
turned out to be false; see "Recursive CTE" above.

- Good, because it materializes nothing — no closure table, no write-path maintenance,
  no migration.
- Good, because scope *is* statically embeddable in the recursive member: that member reads
  the real table joined to the CTE self-reference, so the same `build_scope_condition`
  applies there as to the seed. (This is the corrected premise. The original "scope cannot
  be embedded into the recursive member statically" was wrong.)
- Bad, because `CYCLE` is not portable — `sea_query`'s MySQL and SQLite builders no-op it —
  so termination depends on a library-emitted depth cap rather than the engine.
- Bad, because a subquery-based scope filter (`InTenantSubtree`) is re-evaluated per hop.
- Bad, because dedup is per depth, not per node: a node reachable via paths of
  different lengths is still expanded once per depth even under the default `Union`.
- Not a substitute for closure tables on hot paths; both are supported, chosen on cost.

## More Information

- Secure ORM select builder & execution:
  [libs/toolkit-db/src/secure/select.rs](../../../../libs/toolkit-db/src/secure/select.rs)
- Scope condition builder & closure subqueries:
  [libs/toolkit-db/src/secure/cond.rs](../../../../libs/toolkit-db/src/secure/cond.rs)
- Closure-table maintenance:
  [tenant/closure.rs](../../../../gears/system/account-management/account-management/src/domain/tenant/closure.rs)
- Raw-SQL policy:
  [11_database_patterns.md](../../../toolkit_unified_system/11_database_patterns.md)
- Closure vs recursive CTE rationale:
  [docs/arch/authorization/DESIGN.md](../../authorization/DESIGN.md)
- System-library CTE (out of scope — `toolkit-db` internal, not user-gear code):
  [outbox/store.rs](../../../../libs/toolkit-db/src/outbox/store.rs)
- ADR template & checklist:
  [docs/checklists/ADR.md](../../../checklists/ADR.md)
- sea_query `WithClause` / `CommonTableExpression`, sea_orm `find_by_statement` /
  `FromQueryResult`.
