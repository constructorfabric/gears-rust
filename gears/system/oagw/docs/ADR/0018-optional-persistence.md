---
status: accepted
date: 2026-10-05
---

# Optional Persistence — Database-Backed Repositories with In-Memory Fallback

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Backend Selection](#backend-selection)
  - [Scope](#scope)
  - [Schema](#schema)
  - [Domain-to-Table Mapping](#domain-to-table-mapping)
  - [Repository Behavior](#repository-behavior)
  - [Transactions](#transactions)
  - [Tenant Scope in the Repositories](#tenant-scope-in-the-repositories)
  - [Provisioning from the Types Registry](#provisioning-from-the-types-registry)
  - [Endpoint Pool Freshness Across Replicas](#endpoint-pool-freshness-across-replicas)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Opt-in database with in-memory fallback, target schema now](#opt-in-database-with-in-memory-fallback-target-schema-now)
  - [Opt-in database with in-memory fallback, two-table JSON subset first](#opt-in-database-with-in-memory-fallback-two-table-json-subset-first)
  - [Database required](#database-required)
  - [In-memory only](#in-memory-only)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-optional-persistence`

## Context and Problem Statement

OAGW stores upstreams and routes only in memory (`InMemoryUpstreamRepo`, `InMemoryRouteRepo`). Every restart loses all configuration created through the Management API. Some deployments run without a database (local development, `--mock`, DB-less profiles) and must keep working. How should OAGW add persistence without breaking DB-less deployments, and which schema should the first step use so that later growth stays consistent?

## Decision Drivers

* Configuration created through the Management API must survive restarts when a database is available
* Deployments without a database must keep working with no extra configuration
* A misconfigured database must never silently lose data
* One schema and one code path for PostgreSQL, MySQL, and SQLite (`cpt-cf-oagw-constraint-multi-sql`)
* All data access through `toolkit-db` secure ORM with tenant scoping (`cpt-cf-oagw-principle-tenant-scope`)
* Long-term schema consistency: later features should only add tables or columns, never reshape stored data
* Proxy lookup starts with the simplest working approach; optimizations come later

## Considered Options

* Opt-in database with in-memory fallback, target schema now
* Opt-in database with in-memory fallback, two-table JSON subset first
* Database required
* In-memory only

## Decision Outcome

Chosen option: "Opt-in database with in-memory fallback, target schema now", because it adds durability where a database exists, keeps DB-less deployments working unchanged, and adopts the [Storage Schema ADR](./0009-storage-schema.md) before any data exists, so later growth needs no data migration.

### Backend Selection

The gear declares the ToolKit `db` capability and implements `DatabaseCapability::migrations()`. The ToolKit runtime runs migrations in its DB phase, before `init`, and then:

| Gear database configuration (`gears.oagw.database`) | Result |
|---|---|
| Absent | Runtime skips migrations; `ctx.db()` returns `None`; OAGW uses the in-memory repositories and logs a warning that configuration is not persisted |
| Present and usable | Runtime runs OAGW migrations; `ctx.db()` returns a provider; OAGW uses the database repositories |
| Present but unusable (connection failure, backend not compiled in, malformed configuration, migration failure) | Gear startup **fails**. OAGW never falls back to in-memory storage when a database is configured |

No OAGW-specific flag exists. The presence of database configuration is the opt-in. The types-registry gear uses the same pattern.

`toolkit-db` returns errors for connection failures, a backend feature that is not compiled in, and a missing referenced server, and the runtime rejects a `database` section it cannot parse before any gear starts. A gear can still see its `database` key without getting a handle: when the configuration has no global `database:` section, the runtime builds no database manager at all. OAGW therefore adds its own guard in `init`: when `gears.oagw.database` is present in the gear section (`ctx.config_provider().get_gear_config(...)`) but `ctx.db()` returns `None`, startup fails. A `database: null` value is treated as absent: OAGW uses the in-memory repositories and logs the warning. Any other present value of `database` still fails startup when `ctx.db()` returns `None`.

OAGW always compiles `toolkit-db` with the SQLite and PostgreSQL backends. MySQL is supported when OAGW is built with the `mysql` Cargo feature; without it, configuring MySQL fails startup ("backend not compiled in"). MySQL runs the same repositories and passes the same conformance suite, with these backend-specific choices:

- Tables use the `utf8mb4_0900_bin` collation, so strings compare by code point without padding, as on PostgreSQL and SQLite. MySQL's default collation ignores case, accents and width: the tag keys would reject tags that differ only that way, and an alias lookup could return another alias.
- InnoDB creates an index for each child foreign key `(tenant_id, parent_id)`, since no index starts with those columns; the other backends need none beyond the primary keys.
- Timestamps are `timestamp(6)`, in UTC, as in the toolkit-db outbox; a plain `timestamp` keeps whole seconds. MySQL's `TIMESTAMP` ends in 2038; that is MySQL's limit, not OAGW's.
- MySQL must run with `explicit_defaults_for_timestamp=ON`, the MySQL 8.0 default (OAGW needs 8.0 for `utf8mb4_0900_bin`). With it off, MySQL gives a table's first `TIMESTAMP NOT NULL` column `DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP`, which would rewrite `created_at` on every update.
- MySQL has no `UPDATE … RETURNING`, so an upstream or route update there runs the update and then reads the row back in the same transaction; on PostgreSQL and SQLite the update returns the row. The repository picks the path from the connected backend.

MariaDB is not tested.

### Scope

This step persists exactly what the Management API exposes today (`oagw/src/api/rest`):

- Upstreams: create, get, list, update, delete
- Routes: create, get, list (optional upstream filter), update, delete

Custom plugins, plugin garbage collection, and concurrency or circuit-breaker settings are out of scope.

### Schema

The database repositories use the ADR-0009 schema with every table except `oagw_plugin`:

| Table | Holds |
|---|---|
| `oagw_upstream` | Upstream row: alias, protocol, enabled, owner (`managed_by`), auth plugin reference, sharing modes, JSON configuration |
| `oagw_upstream_tag` | Upstream tags |
| `oagw_upstream_plugin` | Ordered upstream plugin bindings |
| `oagw_route` | Route row: upstream, enabled, priority, match type, owner (`managed_by`), sharing modes, JSON configuration |
| `oagw_route_http_match` | HTTP path prefix |
| `oagw_route_method` | HTTP method allowlist |
| `oagw_route_grpc_match` | gRPC service and method |
| `oagw_route_tag` | Route tags |
| `oagw_route_plugin` | Ordered route plugin bindings |

Every configuration table, including each child table, carries `tenant_id` and declares it as the secure ORM tenant column, so all of them use the same `AccessScope::for_tenants` scope. Child foreign keys are composite (`(tenant_id, parent_id)` → parent `(tenant_id, id)`, `ON DELETE CASCADE`), so the database rejects a child row whose tenant differs from its parent's.

`oagw_plugin` is added with custom plugin support. Binding tables have no foreign key to it (ADR-0009), so adding it changes no existing table or row.

One more table gates the registry reconcile (see [Provisioning from the Types Registry](#provisioning-from-the-types-registry)): `oagw_registry_state`, a single installation-wide row (no `tenant_id`, `#[secure(unrestricted)]`, read and written with `AccessScope::allow_all()`) holding the deployment generation, the content hash of the last clean reconcile and the reconcile lease ([ADR-0009](./0009-storage-schema.md#oagw_registry_state)). Its own migration creates it and seeds the row idempotently.

### Domain-to-Table Mapping

| Domain field | Stored as |
|---|---|
| `Upstream.auth` | `auth_plugin_ref` (plugin identifier), `auth_plugin_uuid` (parsed when the identifier's instance part is a UUID), `auth_sharing`, `auth_config` (JSON). `auth_plugin_ref` is NULL when `auth` is absent |
| `Upstream.headers` | `headers` (JSON) |
| `Upstream.cors`, `Route.cors` | `cors` (JSON), `cors_sharing` on upstream |
| `Upstream.rate_limit`, `Route.rate_limit` | `rate_limit` (JSON), `rate_limit_sharing` |
| `Upstream.plugins`, `Route.plugins` | `plugins_sharing` plus one binding row per item (`position` from 0, `plugin_ref`, `plugin_uuid`, `config` JSON). `plugins_sharing` is NULL when `plugins` is absent, which distinguishes an absent configuration from an empty list |
| `Upstream.tags`, `Route.tags` | One tag row per tag; tags are de-duplicated and returned in ascending byte order (sorted in application code: an `ORDER BY tag` follows the database collation) |
| `Route.match_rules.http` | `match_type = http`; `oagw_route_http_match.path_prefix`; one `oagw_route_method` row per method (de-duplicated; returned in enum order `GET, POST, PUT, DELETE, PATCH`); query allowlist and suffix mode in `match_config` (JSON) |
| `Route.match_rules.grpc` | `match_type = grpc`; `oagw_route_grpc_match (service, method)` |
| `Upstream.managed_by`, `Route.managed_by` | `managed_by`: `api` (created through the Management API or the SDK) or `registry` (provisioned from the types registry). Written on create; an update never changes it |

The four tables with a `schema_version` column (`oagw_upstream`, `oagw_upstream_plugin`, `oagw_route`, `oagw_route_plugin`) start it at 1; the tag, method and match tables have none. Sharing columns for absent fields store `private`. `created_at` and `updated_at` are maintained by the database repositories.

Where a sharing column exists (`auth_sharing`, `cors_sharing`, `rate_limit_sharing`, `plugins_sharing`), the column is the source of truth and the JSON column omits `sharing`. `oagw_route` has no CORS sharing column in ADR-0009, so `Route.cors` stores its `sharing` inside the `cors` JSON. `RateLimitConfig.pool_owner_id` is computed at merge time and is never stored.

### Repository Behavior

The domain repository traits (`UpstreamRepository`, `RouteRepository`) keep their other existing methods. `RouteRepository` replaces `find_matching` with `find_matching_in_tenants`; `find_matching` is removed, because its only caller was the per-tenant loop that the new method replaces. The database repositories implement the same contract as the in-memory ones, except for the upstream check on route writes (see **Upstream check** below):

- **Aggregate loading**: a read loads the parent row and its child rows and assembles one domain `Upstream` or `Route`.
- **Batched child loading**: child rows are never loaded one parent at a time. Each child table is read once per repository call with `WHERE tenant_id IN (…) AND <parent>_id IN (…)`, so a call costs one root query plus one query per child table it needs, whatever the number of parents. Only `list_by_alias_for_tenants` and `find_matching_in_tenants` join (proxy resolution, the closest-ancestor lookup and the budget checks): they join children on the composite foreign key `(tenant_id, parent_id)` and scope the parent by tenant with the secure ORM. The join key equates the child's tenant with the scoped parent's, so a joined child is always in scope, and a `LEFT JOIN` keeps parents without children. A tenant predicate on the children would therefore never filter a row; a `WHERE` one would also drop parents without children from a `LEFT JOIN`. On write, every child takes its parent's `tenant_id`, a row's tenant comes only from the security context, and the composite foreign keys reject a mismatch, so a child row cannot carry another tenant. The query-count tests pin the join keys in the generated SQL. At most 32 bindings per parent bound the rows one join returns. Get and list paginate parent rows and do not join; route get and list cost six queries: routes, HTTP match rows, method rows, gRPC match rows, tag rows, and plugin rows.
- **Alias lookup**: alias reads go through `list_by_alias_for_tenants`; `get_by_alias` is removed from `UpstreamRepository`, because no caller needs a single-tenant alias lookup after the change below. The `(tenant_id, alias)` unique index still enforces uniqueness.
- **Proxy alias resolution in one query**: proxy-time resolution calls `list_by_alias_for_tenants(alias, tenant_chain, tags)` once (`WHERE alias = ? AND tenant_id IN (chain)`, unique index `(tenant_id, alias)`, plugin rows `LEFT JOIN`ed) instead of one `get_by_alias` per tenant. The domain service orders the results by chain position and applies the existing shadowing, visibility, and disabled-upstream rules unchanged.
- **Proxy route matching in one query**: `find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)` takes an ordered list of upstream IDs: the selected (closest) upstream first, then the merge-chain ancestor upstreams closest-first. Proxy resolution already falls back to routes attached to ancestor upstreams, so one call covers all of them. The method loads the enabled HTTP candidates in one query (`WHERE tenant_id IN (chain) AND upstream_id IN (upstream_ids) AND enabled = true AND match_type = 'http'`, joined with `oagw_route_http_match` and `oagw_route_method` and filtered on the request method, one row per route; it selects only the columns matching reads, not the JSON). It selects the winner with the same matching logic as the in-memory repository, in this order: upstream preference (list order), tenant chain position, longest path prefix, higher priority number, and lowest route ID. It then loads the winning route in full, with its path, method and plugin rows, in one more query that also requires the route to be on one of `upstream_ids`, and checks the loaded route against the request with the same matching logic (see [Transactions](#transactions)); it loads its tag rows when `tags` is `Tags::Load`. It replaces the per-tenant `find_matching` loop in the domain service.
- **Management hierarchy checks in one query**: on upstream create and update, the closest ancestor upstream with the alias is found once with `list_by_alias_for_tenants(alias, ancestors, Tags::Skip)` and ordered by chain position in Rust. The result is passed to both the bind validation (`bind.rs`) and the budget allocation check (`budget.rs`), which previously each walked the ancestors with one `get_by_alias` per tenant. Descendant budget checks already load the descendant tree's upstreams with one `list_by_alias_for_tenants` call, also without tags.
- **Tags**: the proxy never reads tags, so it passes `Tags::Skip` and both reads return none. The SDK's `resolve_proxy_target` returns the full effective configuration and passes `Tags::Load`. The management hierarchy and budget checks read neither and pass `Tags::Skip`.
- **Query count**: without the proxy configuration cache ([ADR-0020](./0020-proxy-config-cache.md), which serves a hit with no query), an HTTP proxy request costs three queries regardless of tenant hierarchy depth and of how many ancestor upstreams are searched: upstreams for the chain with their plugin rows; the candidate routes that allow the method, with their path; the winning route with its path, method and plugin rows. A request that races a configuration write can cost two more (see [Transactions](#transactions)). With tags, two more: the upstreams' and the winning route's tag rows.
- **Route overlap check inside the write**: route `create` and `update` enforce the Management API's overlap rule themselves, atomically with the write, and return `RepositoryError::RouteOverlap` with the overlap to report (lowest route ID, then first shared method); the Control Plane maps it to the 409 it has always answered. The check is one query that returns the enabled HTTP routes of the upstream with the same path (byte-exact) and priority that allow one of the methods, one row per shared method and only their ID and method ([ADR-0009 Appendix B](./0009-storage-schema.md#appendix-b-queries-and-access-frequency) Q11). It returns no rows in the normal case, whatever the number of routes on the upstream. An update leaves the route itself out and checks the upstream it is written to, the target of a registry move. Only an enabled HTTP route is checked; any other writes as before. The rule is the same for every writer: registry routes are checked against stored registry routes too. See [Transactions](#transactions).
- **Owner**: `update` leaves `managed_by` as stored and returns the stored value.
- **Route upstream**: route `update` writes the `upstream_id` it is given. The Control Plane passes the stored one, except when the registry reconcile moves a registry route to another upstream in place. The in-memory repository moves the route in its upstream index under the route's lock, and `delete_by_upstream` removes only routes still on that upstream.
- **Upstream check**: the database repositories reject a route `create` or `update` onto an upstream the tenant does not have (no upstream with that ID, or another tenant's) as `NotFound` for `upstream`, from the upstream lock of the overlap check or from the composite foreign key, and write nothing. The in-memory repositories do not check the upstream and write the route. The Control Plane reads the upstream before every Management API route write (`UpstreamCheck::Read`), so the difference is not visible through the Management API. It shows only where that read is skipped or raced: a registry route write for an upstream the reconcile applied in the same run (`UpstreamCheck::KnownRegistry`), and a route create racing its upstream's delete, which in memory can leave the route orphaned (see [Transactions](#transactions)). The conformance suite runs this case on the database backends only.
- **Registry rows**: a separate port, `RegistryRowRepository`, holds the registry reconcile's access to registry rows (see [Provisioning from the Types Registry](#provisioning-from-the-types-registry)); only the gear's registry provisioner holds it, so no Management API or SDK path can reach it. `list_upstreams` and `list_routes` return every registry-managed row in full, across all tenants, in key order: 3 queries for upstreams and 6 for routes, as `list` (index `(managed_by)`, [ADR-0009 Appendix B](./0009-storage-schema.md#appendix-b-queries-and-access-frequency) Q10). They are the only reads without a tenant filter.
- **Delete if unused**: `RegistryRowRepository::delete_upstream_if_unused` deletes a registry-managed upstream only if no route uses it and answers `Deleted`, `InUse` or `NotFound`; an `api` upstream reads as `NotFound` and is never deleted. See [Transactions](#transactions).
- **Uniqueness**: the `(tenant_id, alias)` unique index rejects duplicates. The repository maps the violation to `RepositoryError::Conflict` with backend-neutral detection.
- **Existing IDs and tenant match**: on both backends, `create` with an existing ID returns `RepositoryError::Conflict`, and route `update` matches on `(id, tenant_id)`.
- **Not found**: scoped reads and writes that match no row return `RepositoryError::NotFound`.
- **Listing**: offset pagination (`top`, `skip`) ordered by `id` ascending on both backends.
- **Canonical order**: the Control Plane canonicalizes tags and HTTP methods before every create and update. Tags are de-duplicated and sorted ascending in byte order. Methods are de-duplicated and sorted in enum order (`GET, POST, PUT, DELETE, PATCH`). Both backends return them in that order in API responses; the database repositories sort both in application code after loading, because an `ORDER BY` follows the database collation, not byte order.

Later optimizations, none of which change the schema:

- Column projections for the rate-limit budget checks, which need only some columns
- Config caching, which removes the database from the proxy path on a cache hit and needs its own cross-replica freshness decision: [ADR: Proxy Configuration Cache](./0020-proxy-config-cache.md) (ADR-0020, superseding ADR-0007 and the Data Plane L1 cache of ADR-0008)
- Indexed SQL route matching (see [ADR-0009 Appendix B](./0009-storage-schema.md#appendix-b-queries-and-access-frequency) for today's candidate query), only if measurements show the in-Rust matching is a bottleneck; it keeps the same ordering as `select_route` (upstream preference, tenant chain position, longest path prefix, higher priority number, lowest route ID)

### Transactions

`toolkit-db` provides `DBProvider::transaction`, `transaction_with_config` (isolation level and access mode via `TxConfig`), and `Db::transaction_with_retry` (retries on backend-classified contention errors).

- Each repository create and update runs in one transaction that it opens itself. Create inserts the parent row and its child rows. Update replaces the parent row and replaces its child rows. Deletes are single statements, atomic without a transaction. The repository traits take no transaction handle.
- Management reads (`get_by_id` and `list` of both repositories) run in one read-only REPEATABLE READ transaction (`transaction_with_config`), so the parent and child queries see one snapshot. Under the default READ COMMITTED each statement takes a new snapshot: a read racing an update could return the old parent row with the new child rows, and a route read racing a delete could find the route row without its match row and fail with an internal error. `SQLite` runs every transaction serializable and treats read-only as a hint.
- Proxy reads (`list_by_alias_for_tenants` and `find_matching_in_tenants`) run without a transaction, which keeps the hot path at three statements. The statements of one request can therefore see different commits. The route lookup therefore does not trust its candidate query: the winner query re-reads the route's path, methods and upstream, and the loaded route is checked against the request with `select_route`. A winner that a write in between changed so that it no longer matches (for example, its methods no longer include the request's method), deleted, disabled, or re-created on another upstream is selected once more, from a new candidate query; a second miss returns no match (404). Only a request that raced a write pays the two extra statements. A request can still combine an upstream with a route of the configuration that replaced it; it sees only its own tenant chain's committed rows, and the next request sees the new configuration.
- Deleting an upstream writes with one `DELETE` statement; the Control Plane reads the upstream first to check its owner. The foreign-key cascade removes its tags and plugin bindings, its routes, and the routes' child rows, so no state exists in which the routes are gone but the upstream remains. A route created concurrently is either rejected by the composite foreign key or removed with the upstream. The Control Plane deletes the upstream first and then calls `RouteRepository::delete_by_upstream`, which is a no-op on the database path; the in-memory repositories have no cascade and remove the tenant's routes in that call. Only the database closes the window: there the foreign key rejects a route created after the delete, while in memory a route create that passed its upstream check before the delete can still land afterwards and stay orphaned.
- `delete_upstream_if_unused` runs in one transaction with `transaction_with_retry`, READ COMMITTED on PostgreSQL and MySQL (the default on SQLite): a lock on the registry upstream row (`upstream_lock::lock_upstream`: `SELECT … FOR UPDATE` on PostgreSQL and MySQL; on SQLite, which has no row lock, a no-op `UPDATE` of the row, so the transaction takes the database's write lock first and waits for it under the busy timeout; a transaction that reads first fails its first write as busy at once), a one-row check for a route on it, then the `DELETE`. A concurrent route insert's foreign-key check takes a key-share lock on the upstream row, which conflicts with the row lock: the insert either committed first (the check, a fresh READ COMMITTED snapshot, sees it, and the answer is `InUse`) or waits and then fails its foreign key. On MySQL the locking read goes through the unique index the route foreign key references (`FORCE INDEX (uq_oagw_upstream_tenant_id)`): InnoDB's foreign-key check share-locks the parent's record in that index, and a locking read by primary key locks only the clustered record, so without the hint the two never conflict, the delete misses an uncommitted route, and its `DELETE` then cascades that route away. It first locks by primary key, then through that index: an upstream update or delete locks the clustered record before the index record (its own foreign key checks, the delete mark), so every writer takes the two in one order and none deadlocks. Two-connection tests on PostgreSQL and MySQL pin both orders. A single `DELETE … WHERE NOT EXISTS` would not do: under PostgreSQL READ COMMITTED its re-check after the row lock does not see a route committed meanwhile. Contention (PostgreSQL 40001/40P01, MySQL deadlock, SQLite busy) is retried; the body keeps the driver error for that, since `RepositoryError` carries it as text. In memory the route store is checked, then the upstream deleted, with the window `delete_by_upstream` documents.
- The registry-state row is read with one `SELECT` and written with one conditional `UPDATE` per step (claim, acquire, renew, finish), whose affected-row count is the answer; no transaction. Lease deadlines come from the database clock (PostgreSQL `now()`, MySQL `NOW(6)`, SQLite `datetime('now')`), never the pod's, and SQLite compares them through `datetime()`.
- Upstream delete returns no route IDs. Rate-limit buckets of deleted routes are reclaimed by the per-replica sweep that evicts buckets at rest (a full token bucket, or an empty sliding window on the limiter's fixed sub-window grid; both behave exactly like new ones), so cleanup does not depend on which replica or API handled the delete.
- No cross-repository unit of work is introduced in this step. Custom plugin garbage collection will need one, because binding changes and `oagw_plugin.gc_eligible_at` must change together; that is decided with custom plugin support.
- The rate-limit budget check reads sibling upstreams and then writes. This read-then-write race already exists with the in-memory repositories and is a known gap, not a regression.
- A route create or update of an enabled HTTP route runs in one READ COMMITTED transaction with `transaction_with_retry`: it locks its upstream row (as above), runs the overlap query, then writes. Route writes on one upstream therefore serialize, and the second of two overlapping writes sees the first and is rejected; a registry move locks the upstream it moves to. A missing upstream is `NotFound` for `upstream`. On SQLite the lock is the database's write lock, so route writes serialize there too and a second writer waits for it. Proxy reads take no lock and are not delayed. In memory a write lock spans the check and the write.

### Tenant Scope in the Repositories

The database repositories build their own `AccessScope` (`AccessScope::for_tenant` and `for_tenants`). TOOLKIT-SEC-002 forbids manual `AccessScope` construction; this ADR records an exception for OAGW's repositories.

- The scope is a tenant filter, not an authorization grant. Every tenant ID in it comes from the caller's `SecurityContext` (`subject_tenant_id`), from the ancestor chain or, for a budget check, the descendants the tenant resolver returns for that tenant, or, for a write, from the row the Control Plane built for that tenant. No request field names a tenant.
- `RegistryRowRepository`'s reads (one named scope, `registry_reconcile_scope()`) and the registry-state row use an unconstrained scope (`AccessScope::allow_all`): the registry reconcile owns registry rows in every tenant, and the state row has no tenant. Only the gear's registry provisioner holds that port; no request reaches it. The security contexts the reconcile acts under, which no authenticated caller provides, are built in one module (`system_identity`): the platform's default subject (`DEFAULT_SUBJECT_ID`) in the instance's tenant, the same subject as every request when the API gateway runs with auth disabled. The policy engine is asked only when an upstream binds to an ancestor's alias, so the tenant an instance names is trusted as the registry gives it. A dedicated OAGW system actor (its own subject ID and type) is a follow-up.
- Authorization decisions stay with `PolicyEnforcer` where OAGW makes them: proxy invocation and binding to an ancestor's upstream.
- Other gears' repositories build tenant scopes the same way (for example pricing, ledger, mini-chat, products, event-broker, file-storage, and graph-storage). Moving to enforcer-issued scopes is a platform-wide change, not one for OAGW alone.

### Provisioning from the Types Registry

OAGW reads the upstream and route instances that other gears, or the types-registry `entities` configuration, registered in the types registry, and makes the stored rows match them. Each instance carries a fixed UUID from its GTS identifier, which becomes the row ID. The registry is the source of truth for these rows; before persistence, every boot re-created them from it, and both backends keep that behavior.

- **Where it runs**: with the in-memory backend, synchronously in `post_init`, before the pod serves: the store starts empty and no other process shares it. It runs once: there is no re-check, so a skipped instance stays skipped, and the healthcheck Degraded, until the process restarts. With a database, in a background task (`RunnableCapability`; the gear declares `stateful`) that `start` spawns before returning at once, so a held lease, a slow run or a failing dependency never delays a pod's startup or puts it in a crash loop; the pod serves what the database holds from the start, as old pods do during a rollout. A broken or unreachable database still fails startup: migrations run before `start`. The task never ends on its own: after every run, whatever its outcome, it re-checks after `registry_provisioning.recheck_interval` (default 5 minutes, from 1 s to 24 h, ±20% jitter); an unchanged state costs one `SELECT`. A run that fails on an infrastructure error, or panics, is a failed run: recorded, and retried with backoff from 1 s to 5 min (±20% jitter, so pods that failed together do not retry in lockstep). A failed run releases its lease at once, except after a call timeout: the timeout drops only this pod's side of the call, and a statement already sent (an autocommit `DELETE` waiting on a row lock, a `COMMIT`) can still complete on the database, so the lease is kept until its TTL lapses and that statement lands under it, not under the next owner's. The same pod takes its own lease back on its retry. The task runs under a child of the runtime's cancellation token, so it also ends when the runtime shuts down without a stop phase. `stop` cancels the run between writes and the run releases its lease; a run still blocked in a call at the shutdown deadline is aborted, and the lease TTL frees its lease, as after a hard stop. `registry_provisioning.generation` must fit a `BIGINT` (at most `i64::MAX`); both bounds are checked at startup. Readiness is not gated on the reconcile, so a first install serves without registry rows for the seconds the first run takes.
- **Ownership**: every upstream and route records its owner in `managed_by`. Rows created through the Management API or the SDK are `api`; rows provisioned here are `registry`.
- **Read-only through the API**: the Management API and the SDK cannot update or delete a registry-managed row. Both fail with `failed_precondition` (HTTP 400, violation type `REGISTRY_MANAGED`, subject `managed_by`; SDK consumers match `oagw_sdk::precondition::REGISTRY_MANAGED` and `MANAGED_BY`); the row changes only with its registry instance, at the next reconcile. A route created through the API on a registry-managed upstream is allowed and is `api`-managed. Provisioning writes through a separate Control Plane interface that only the gear holds; it runs the same validation as API writes and cannot change an `api` row.
- **Gate (database only)**: the `oagw_registry_state` row holds the deployment generation, the content hash and the lease. The content hash is a SHA-256 over OAGW's version and a canonical encoding of every field of the materialized registry requests (length-prefixed fields, map entries sorted by key, instances sorted by `(tenant_id, id)`; the encoder destructures each struct without `..`, so a new field does not compile until it is hashed), so a new OAGW version re-reconciles unchanged content. A pod whose `registry_provisioning.generation` is lower than the stored one does not reconcile and writes nothing. A pod whose content hashes to the stored hash writes nothing, except to claim its newer generation in one `UPDATE`. Otherwise one pod takes the lease (60 s), claiming its generation in the same statement, and reconciles; the others poll every second (±20% jitter) until the hash matches, a newer generation appears, or the lease frees. The holder renews the lease after every pass and before any write once a third of the TTL (20 s) has passed since the last renewal, and every call of a run (the root tenant, the registry listings, each state statement, each write) is bounded by a call timeout of 20 s. A write therefore starts at most 20 s after a renewal and ends at most 20 s later, inside the TTL. A call that times out fails the run like an outage. A pod that lost its lease mid-run stops writing at the next renewal, and a lease found lost when the hash would be recorded makes the run not clean. The hash is recorded only after a clean run: nothing skipped, no upstream kept, not stopped. Taking the lease clears the recorded hash, so while a run is in progress or after one that was not clean, the rows match no content: a pod whose content matches an earlier clean run (a `helm rollback`, an old pod) reconciles the half-applied rows instead of taking them as up to date. Without a generation the guard is off, with a startup warning.
- **Phases**: (1) load every registry row in full; (2) check the content without reading anything; (3) removals; (4) upstreams, in passes; (5) routes, in passes; (6) record the hash or not, release the lease, log the counts.
- **Content check first**: before any write, each instance passes the checks a create runs before it reads anything (fields, endpoints and the SSRF policy, CORS, budget configuration, the alias, the `secret_ref` syntax), two upstream instances of one tenant must not derive the same alias, a route must name an upstream instance of its tenant, and registry routes must not overlap each other by the Management API's overlap rule (same tenant and upstream, both enabled, same path and priority, a shared method). An instance that fails is skipped: never written, and its stored row, if any, is never removed, because the set of rows to keep comes from every instance, checked or not. An instance the listing cannot read (a `$id` that does not end in a UUID, or a payload that does not parse) is not skipped: it fails the listing, and so the run, like an outage (retried with backoff on a database, failing startup in memory), until it is fixed or removed in the types registry. The types registry accepts any object for the upstream and route types today, so it does not catch these on registration.
- **Failures: reject or fatal**: any error a write returns skips that instance (logged at `error` with its reason and whether a stored row keeps serving, counted in `oagw_registry_provisioning_failures_total{kind,reason}`), unless it is `Internal` (database, tenant resolver unavailable, policy engine failure, credential store unavailable), an outage-like error a write should never return (downstream, link, circuit breaker, timeouts, aborted stream, protocol error), a call timeout, or the tenant resolver's `Unauthorized`; those abort the run, which the background task retries with backoff, and fail startup in memory. The classification lists every `DomainError` variant, so a new one does not compile until it is classified; proxy-only errors a write should never return are rejected with the reason `unexpected` and logged with the error's variant, and plugin errors with `plugin`. Rejected writes include an alias or ID held by an `api` row, an overlap with a stored route, a hierarchy, bind or budget conflict, a policy denial, an inaccessible secret, an unknown tenant, and a route naming an `api` upstream (deleting that upstream through the API would cascade the route away). A stored `api` row with an instance's ID is never overwritten. Dependents fail on their own writes. A rejected write is retried in the next pass as long as the previous pass created or updated something, so alias hand-overs, path hand-overs, route moves and ancestor-before-descendant order resolve in one run whatever the listing order.
- **Removal never cascades into API routes**: registry routes whose instance the registry no longer has are deleted. For a registry upstream it no longer has, the run first lists the routes on it: if a route that this run neither deletes nor writes again uses it (an `api` route, or a registry route the content check skipped), the upstream stays, and nothing on it is deleted except routes the registry no longer has; the registry routes leaving it are moved in place in phase 5, so a rejected move leaves them serving on it. Otherwise the registry routes on it that phase 5 writes again go first: those that passed the content check and name an upstream that passed it too or is stored (they move to another upstream with the release and are re-created there in phase 5), then the upstream is deleted with `delete_upstream_if_unused` only if no route uses it. A route the content check skipped, or one whose new upstream it skipped, is not deleted: it keeps serving on the dropped upstream with its last configuration, and so keeps that upstream, until its content is fixed; nor is a route whose delete is refused. An upstream that routes still use is kept, reported with every registry route still on it and the `api` routes among its first ten routes by ID, and counted (`kept_in_use`); it stays registry-managed, serves those routes with its last registry configuration and keeps its alias, so an instance that wants the alias is skipped. The first run after those routes are gone removes it. A route that passed the check and was deleted here but whose phase-5 write is then rejected (for example on a new upstream whose write is rejected in phase 4, such as an ID an `api` row holds) is absent until the rejection clears, and logged at `error` as not re-created. An `api` route created on the dropped upstream between the listing and the delete has the same effect. A route delete that finds nothing counts as done.
- **Then create or update**: a missing instance is created; a stored row whose content differs is replaced in full and logged with the differing fields; an unchanged row is left alone. A registry route whose instance names another upstream moves in place: the registry writer may change a route's `upstream_id` (the Management API cannot); a rejected move leaves the route serving on its old upstream. A registry route write on an upstream the run applied skips re-reading that upstream; the foreign key still rejects a missing one. An instance that moved to another tenant is removed from the old tenant and created in the new one; a route whose old upstream stays (see above) keeps serving in the old tenant, and its create in the new one is rejected (the row holds its ID), until the upstream can go.
- **One overlap rule**: the overlap check of a registry route write includes stored registry routes, so registry routes never overlap at match time, and API and registry routes conflict both ways. A path hand-over between registry routes resolves through the passes; a path swap never makes progress and both routes are skipped (`path_cycle`), keeping their old paths. `path_cycle` marks a route whose path is held by the stored row of another route whose write this run also rejected, so a hand-over from a route rejected for another reason is `path_cycle` too; a route blocked by the stale row of a route the content check skipped is skipped as `conflict`.
- **Aliases follow the instance**: on every update the registry writer applies the create rules to the instance's endpoints and explicit alias, so new endpoints or a new explicit alias move the alias in place, hostname to IP included. The row keeps its ID, its registry routes and any API routes on it; deleting and re-creating it instead would cascade those API routes away. Two registry upstreams cannot swap aliases in one release: both are skipped (`alias_cycle`), keep their old aliases, and the log asks to split the change across two releases. `alias_cycle` marks an upstream whose alias is held by the stored row of another upstream whose write this run also rejected, so a hand-over from an upstream rejected for another reason is `alias_cycle` too; an upstream blocked by one the content check skipped is skipped as `conflict`. The Management API keeps the alias immutable ([ADR-0010](./0010-resource-identification.md)). Moving an alias has the effects of any alias change: `/proxy/{old alias}` answers 404, upstreams in descendant tenants that share the old alias no longer bind to this one and stop inheriting its sharing, plugins, rate limit and budget, and descendant upstreams that already use the new alias bind to it without their budget allocations being re-checked.
- **Visibility**: `oagw_registry_provisioning_runs_total{outcome}` (`up_to_date`, `stale`, `clean`, `not_clean`, `failed`; a run stopped by shutdown or cancelled while waiting for the lease counts as `not_clean`, so alerts should require it to persist), the failures counter above, the per-instance logs, and the "Type provisioning complete" log of each run. The gear's REST healthcheck reports **Degraded, never Unhealthy**, while a run is pending, failed or not clean: `/readyz` keeps a Degraded pod in traffic, and a liveness probe on `/health` would restart pods on Unhealthy.
- **Concurrent writers**: one pod reconciles at a time under the lease. A Management API write racing a run surfaces as a rejection, retried in the next pass or skipped. Renewal by elapsed time, the call timeout, and keeping the lease after a timeout keep every write inside the lease; only a process frozen for longer than the TTL (a stop-the-world pause, a suspended VM) can still finish one write after a new owner starts, which true fencing (an owner check inside every write's transaction) would close and this design accepts. A create that conflicts falls back to a lookup and continues when the row found is the registry's own.

### Endpoint Pool Freshness Across Replicas

> **Amended by [ADR-0019](./0019-upstream-proxy-engine.md)** (`cpt-cf-oagw-adr-upstream-proxy-engine`): the selector is now `PoolEndpointSelector` over `toolkit-http-proxy`'s `BackendPool`; every rule below applies to it unchanged.

`PingoraEndpointSelector` caches each upstream's endpoint pool by `upstream_id`. Management handlers invalidate that cache only on the replica that handled the write. With a shared database, other replicas would keep the old endpoints. The selector **MUST** rebuild a cached pool whenever the endpoints passed to `select()` differ from the cached list. This needs no messaging between instances.

An upstream deleted through another replica leaves its pool cached here, and each pool runs a background task (DNS every 30 s, TCP health checks every 10 s). The selector therefore drops a pool not selected for 10 minutes, checking every minute, and rebuilds it if it is used again. Dropping a pool, by this sweep, by invalidation, or by a rebuild, **MUST** stop its background task. Updates and deletes through the SDK invalidate the local pool as the Management API handlers do.

### Consequences

* OAGW adds dependencies on `toolkit-db` and SeaORM, declares the `db` and `stateful` capabilities, and ships SeaORM migrations for the nine tables and the registry-state row.
* `resolution.rs` replaces its per-tenant `get_by_alias` and `find_matching` loops with one `list_by_alias_for_tenants` call and one `find_matching_in_tenants` call over the ordered upstream IDs (selected upstream, then ancestor upstreams closest-first). Both repository implementations provide `find_matching_in_tenants`; `find_matching` is removed from `RouteRepository`.
* `bind.rs` and `budget.rs` take the closest ancestor upstream as an input instead of looking it up; upstream create and update look it up once with `list_by_alias_for_tenants`.
* `gear.rs` selects the repository implementation from `ctx.db()`. The in-memory repositories remain a supported runtime mode, not only test doubles, and must gain every capability the database repositories gain.
* ADR-0009 is the single schema source of truth. Later features add tables (`oagw_plugin`) or columns (concurrency limits, circuit breaker) without moving stored data.
* CI runs the repository conformance tests against the in-memory repositories, SQLite, PostgreSQL, and MySQL, and the migration tests against SQLite, PostgreSQL, and MySQL. The default OAGW build compiles the SQLite and PostgreSQL backends; MySQL needs the `mysql` feature (see [Backend Selection](#backend-selection)).
* With the in-memory backend, configuration is lost on restart. Operators who need durability must configure a database.
* Rate-limit counters stay in memory per replica ([ADR-0004](./0004-rate-limiting.md)): replicas that share one database each enforce the configured limit, so N replicas allow up to N times the rate. This predates persistence, but persistence makes several replicas on one configuration the normal deployment.
* Registry content is applied by the reconcile of each pod's version: in memory at every start, on a database by the background task, one pod at a time, re-checked every interval. During a rolling deploy, replicas of two versions hold different registry content. With `registry_provisioning.generation` set from the deployment (Helm `.Release.Revision`, which also increases on `helm rollback`, so a deliberate rollback applies its content), the first new pod claims the newer generation and pods with an older one never reconcile: an old-version pod that restarts during or after the rollout serves the newer rows instead of reverting them, and versions running side by side (canary, blue/green) never flip-flop. The generation is claimed before the writes, so an older pod is refused even when the newer run failed half-way; the next newer-generation run completes it. `kubectl rollout undo` brings back pods with the older generation, which are refused: recover with `helm rollback` (a new revision) or by raising `registry_provisioning.generation`. Without a generation (development, non-Helm deployments) an old-version pod reconciles back to its content when it starts or re-checks, and two versions running together re-apply their content alternately every interval; because removal never cascades into API routes, a revert cannot delete user data, at worst it keeps a newer registry upstream that API routes use.
* Problems in the registry content, and conflicts with `api` rows, no longer stop a pod: they skip the instance, and show up only in the failures and runs metrics, the logs and a Degraded healthcheck, which need alerts. An infrastructure error after the removals leaves them applied until the retried run completes; the pod keeps serving.
* Registry-managed upstreams and routes cannot be changed through the Management API or the SDK; operators change the registry instance instead.
* Stored upstreams outlive the SSRF policy they were validated against: before persistence, a restart re-created and re-validated every upstream. Alias resolution therefore re-checks the selected upstream's endpoint hostnames against the current hostname deny-list and rejects a denied one with the same validation error as create, so a tightened policy also applies to stored and registry-provisioned upstreams. Resolved IPs were already checked on every connection.

### Confirmation

Each behavior below has an ID. The test, or tests, that prove it name the ID in their doc comment, so `grep -rn PERS-GATE-003` finds them. "Every backend" means the in-memory repositories and SQLite, PostgreSQL and MySQL; `make test` runs memory and SQLite, `make test-integration LANE=oagw-db` runs PostgreSQL, MySQL and the SQLite lock races.

**Repositories**

* `PERS-STORE-001`: one repository conformance suite runs on every backend with identical results, plus one database-only case: the in-memory repositories write a route onto an upstream the tenant does not have (see *Repository Behavior*, **Upstream check**).
* `PERS-STORE-002`: absent and empty optional fields round-trip distinctly.
* `PERS-STORE-003`: `managed_by` survives an update that names another owner.
* `PERS-STORE-004`: a route update moves the route to the upstream it names; on the database backends, a route create or move onto a missing or foreign upstream is the upstream not being found.
* `PERS-STORE-005`: `RegistryRowRepository::list_upstreams` and `list_routes` return registry rows in full across tenants and nothing else, on every backend.
* `PERS-STORE-006`: `list_by_alias_for_tenants` and `find_matching_in_tenants` resolve alias shadowing and route selection across a multi-level hierarchy.
* `PERS-STORE-007`: `delete_upstream_if_unused` answers `Deleted`, `InUse` and `NotFound`, and `NotFound` for an `api` upstream, which it leaves alone.

**Schema and migrations**

* `PERS-MIG-001`: the migrations create the schema on SQLite, PostgreSQL and MySQL, including the composite foreign keys.
* `PERS-MIG-002`: values at the stored field limits are stored.
* `PERS-MIG-003`: JSON documents over 64 KiB are stored.
* `PERS-MIG-005`: the database rejects a child row whose `tenant_id` differs from its parent's.

**Startup and restart**

* `PERS-BOOT-001`: without database configuration the gear uses the in-memory repositories.
* `PERS-BOOT-002`: a `database` section without a database handle (no global `database:` section) fails startup. An unreachable database fails startup in `toolkit-db`, before OAGW's `init` runs; OAGW has no test of its own for it.
* `PERS-BOOT-003`: `database: null` uses the in-memory repositories.
* `PERS-BOOT-004`: upstreams and routes created through the Management API survive a restart with a database.
* `PERS-BOOT-005`: provisioning boots twice against the same database without errors and leaves the stored rows unchanged.

**Registry reconcile** (every backend, except the cases that script a failure or a hang, which run in memory)

* `PERS-PROV-001`: a run applies changed registry content, including an alias change in place.
* `PERS-PROV-002`: every kind of bad content is skipped with its reason, and those instances' stored rows stay.
* `PERS-PROV-003`: writes the Control Plane rejects (an alias or ID held by an `api` row, a hierarchy conflict, a route on an `api` upstream) are skipped while the stored rows keep serving.
* `PERS-PROV-004`: alias and path hand-overs resolve in one run in any listing order.
* `PERS-PROV-005`: a chain listed in reverse takes one pass per link.
* `PERS-PROV-006`: alias and path swaps are skipped as cycles.
* `PERS-PROV-007`: a route moves to another upstream in place, and stays on its old upstream when the move is rejected.
* `PERS-PROV-008`: instances the registry dropped are removed.
* `PERS-PROV-009`: a dropped upstream that API routes use is kept until they are deleted.
* `PERS-PROV-010`: a skipped route, or a route moving to a skipped upstream, keeps its dropped upstream until its content is fixed.
* `PERS-PROV-011`: a route moving off a dropped upstream that something else keeps stays on it until its move succeeds, and a route moving tenant off such an upstream stays in the old tenant until it can move.
* `PERS-PROV-012`: a route follows its upstream to a replacement with the same alias.
* `PERS-PROV-013`: an instance moves to its new tenant.
* `PERS-PROV-014`: a run stopped after a moving route's delete, or between writes when cancelled or when the lease is lost in a pass, leaves the rest to the next run, which creates only what is missing.
* `PERS-PROV-015`: a credential-store outage, an unauthorized tenant resolver, or a removal that fails on an outage aborts the run; a refused secret or a refused removal is skipped.
* `PERS-PROV-016`: a removal or a route create that hangs aborts the run after the call timeout.
* `PERS-PROV-017`: a table test classifies every `DomainError` variant, and an `unexpected` rejection logs its variant.
* `PERS-PROV-018`: an instance the listing cannot read (a `$id` without a UUID, a payload that does not parse) fails the listing.

**Lock races** (two connections; PostgreSQL, MySQL, and SQLite on a file)

* `PERS-RACE-001`: a route insert behind `delete_upstream_if_unused`'s lock (an enabled route's upstream lock, or a disabled route's foreign key check) is rejected as the upstream not being found.
* `PERS-RACE-002`: a delete behind an uncommitted route insert keeps the upstream.
* `PERS-RACE-003`: a second overlapping route write waits on the upstream lock and is then rejected.
* `PERS-RACE-004`: a route move waits on its target upstream's lock.
* `PERS-RACE-005`: of concurrent overlapping creates exactly one is written.
* `PERS-RACE-006`: on MySQL a route write behind an upstream update with tags waits without a deadlock.

**Gate** (every backend, except the lost-lease, renewal and timeout cases, which run on in-memory fakes under a paused clock)

* `PERS-GATE-001`: an unchanged registry is not reconciled again (one `SELECT`).
* `PERS-GATE-002`: a waiting pod finishes on the holder's hash.
* `PERS-GATE-003`: an expired lease is taken over.
* `PERS-GATE-004`: a lost lease stops the writes, and a lease lost before the hash is recorded makes the run not clean.
* `PERS-GATE-005`: a write renews the lease once a third of the TTL has passed.
* `PERS-GATE-006`: a failed run releases its lease at once; a run that timed out keeps it until the TTL.
* `PERS-GATE-007`: a hung registry-state statement fails the run after the call timeout.
* `PERS-GATE-008`: a run with skips or a kept upstream is not clean, and a run that is not clean leaves the hash unrecorded.
* `PERS-GATE-009`: taking the lease clears the recorded hash, so a rollback after a run that was not clean reconciles instead of taking the rows as up to date.
* `PERS-GATE-010`: an older generation writes nothing.
* `PERS-GATE-011`: a newer generation with the same content only claims.
* `PERS-GATE-012`: the stored generation never decreases, a refused acquire claims no generation, and an unset generation leaves the stored one alone.
* `PERS-GATE-013`: each hashed field changes the content hash, and map order does not.

**Background task**

* `PERS-LOOP-001`: the task re-checks every interval with one read, and reconciles a reverted state.
* `PERS-LOOP-002`: it refuses an older generation's content.
* `PERS-LOOP-003`: it stays not clean until an invalid instance is fixed.
* `PERS-LOOP-004`: it backs off after a failure, jittered within ±20%.
* `PERS-LOOP-005`: a run whose dependency hangs fails after the call timeout, and a panicking run is survived.
* `PERS-LOOP-006`: a stop ends the run between writes and releases its lease, and ends at once during a hung call.
* `PERS-LOOP-007`: on a database `start` returns at once, and a stopped task starts again.
* `PERS-LOOP-008`: the healthcheck goes from Degraded to Healthy, and reports `registry_provisioning_pending` while another pod holds the lease.
* `PERS-LOOP-009`: `stop` aborts a task that ignores cancel at the shutdown deadline, and logs a panicked one.
* `PERS-LOOP-010`: configuration tests pin the bounds of `recheck_interval` and `generation`.

**Ownership**

* `PERS-OWN-001`: Management API update and delete of registry-managed upstreams and routes fail with `REGISTRY_MANAGED` and change nothing.
* `PERS-OWN-002`: an API route on a registry-managed upstream is accepted.

**Route overlap** (every backend)

* `PERS-OVL-001`: route create and update are rejected exactly when the Management API's overlap rule flags a stored route, report the lowest route ID and first shared method, write nothing when rejected, leave the route itself out, and check a move's target upstream.

**Query counts** (recorded on SQLite)

* `PERS-QC-001`: upstream create and update issue the same number of ancestor lookup queries with 1 and 5 ancestor tenants.
* `PERS-QC-002`: a proxy request issues the same number of queries for tenant chains of 1 and 5 tenants.
* `PERS-QC-003`: list calls issue the same number of queries for page sizes 1 and 50.
* `PERS-QC-004`: a route create is the upstream lock, one overlap query and the inserts, in one transaction.
* `PERS-QC-005`: `delete_upstream_if_unused` is a lock, a select and a delete, in one transaction.

**Proxy path**

* `PERS-PROXY-001`: a route lookup whose winner a concurrent write changed, removed or moved selects once more, and a second miss is no match.
* `PERS-PROXY-002`: a changed endpoint list rebuilds the pool without an explicit invalidation.
* `PERS-PROXY-003`: an upstream stored before its hostname was denied stops resolving once the policy denies it.

## Pros and Cons of the Options

### Opt-in database with in-memory fallback, target schema now

* Good, because DB-less deployments keep working with no configuration change
* Good, because it reuses the ToolKit optional-database mechanism (`ctx.db()` returns `None` when unconfigured)
* Good, because failing startup on an unusable database prevents silent data loss
* Good, because no stored data ever has to be reshaped to reach the target schema
* Bad, because the first step needs nine tables, multi-table mapping, and a transaction per write
* Bad, because two repository implementations must behave identically, which requires a shared conformance suite

### Opt-in database with in-memory fallback, two-table JSON subset first

Store upstreams and routes as two tables with configuration in JSON text, and move to the ADR-0009 tables later.

* Good, because the first step is smaller: single-row writes and no child tables
* Bad, because moving to the target schema later needs a data migration from JSON, which cannot be written in portable SQL
* Bad, because two schema shapes would exist over time, and ADR-0009 would stop being the single source of truth

### Database required

* Good, because there is one storage implementation
* Bad, because DB-less deployments and local development break

### In-memory only

* Good, because it needs no change
* Bad, because configuration is lost on every restart

## More Information

- [ADR: Storage Schema](./0009-storage-schema.md) — schema implemented by this ADR, except `oagw_plugin`
- [ADR: Proxy Configuration Cache](./0020-proxy-config-cache.md) — the config cache on top of this ADR; it supersedes [ADR: Control Plane Caching](./0007-data-plane-caching.md) and the Data Plane L1 cache of [ADR: State Management](./0008-state-management.md)
- [ADR: Rate Limiting](./0004-rate-limiting.md) — rate-limit counters stay in memory per instance; this ADR does not persist them

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-config-persistence` — Opt-in persistence with in-memory fallback and fail-fast on an unusable database
* `cpt-cf-oagw-fr-upstream-mgmt` — Upstream CRUD persisted in `oagw_upstream` and its child tables
* `cpt-cf-oagw-fr-route-mgmt` — Route CRUD persisted in `oagw_route` and its child tables
* `cpt-cf-oagw-nfr-multi-tenancy` — Every configuration table carries `tenant_id` and is accessed through the secure ORM
* `cpt-cf-oagw-db-schema` — Storage schema implementation
