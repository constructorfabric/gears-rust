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
  - [Startup Provisioning from the Types Registry](#startup-provisioning-from-the-types-registry)
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
- Timestamps are `datetime(6)`, in UTC. MySQL's `timestamp` keeps whole seconds and ends in 2038.
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

Every table, including each child table, carries `tenant_id` and declares it as the secure ORM tenant column, so all tables use the same `AccessScope::for_tenants` scope. Child foreign keys are composite (`(tenant_id, parent_id)` → parent `(tenant_id, id)`, `ON DELETE CASCADE`), so the database rejects a child row whose tenant differs from its parent's.

`oagw_plugin` is added with custom plugin support. Binding tables have no foreign key to it (ADR-0009), so adding it changes no existing table or row.

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

The domain repository traits (`UpstreamRepository`, `RouteRepository`) keep their other existing methods. `RouteRepository` replaces `find_matching` with `find_matching_in_tenants`; `find_matching` is removed, because its only caller was the per-tenant loop that the new method replaces. The database repositories implement the same contract as the in-memory ones:

- **Aggregate loading**: a read loads the parent row and its child rows and assembles one domain `Upstream` or `Route`.
- **Batched child loading**: child rows are never loaded one parent at a time. Each child table is read once per repository call with `WHERE tenant_id IN (…) AND <parent>_id IN (…)`, so a call costs one root query plus one query per child table it needs, whatever the number of parents. Only `list_by_alias_for_tenants` and `find_matching_in_tenants` join (proxy resolution, the closest-ancestor lookup and the budget checks): they join children on the composite foreign key `(tenant_id, parent_id)` and scope the parent by tenant with the secure ORM. The join key equates the child's tenant with the scoped parent's, so a joined child is always in scope, and a `LEFT JOIN` keeps parents without children. A tenant predicate on the children would therefore never filter a row; a `WHERE` one would also drop parents without children from a `LEFT JOIN`. On write, every child takes its parent's `tenant_id`, a row's tenant comes only from the security context, and the composite foreign keys reject a mismatch, so a child row cannot carry another tenant. The query-count tests pin the join keys in the generated SQL. At most 32 bindings per parent bound the rows one join returns. Get and list paginate parent rows and do not join; route get and list cost six queries: routes, HTTP match rows, method rows, gRPC match rows, tag rows, and plugin rows.
- **Alias lookup**: alias reads go through `list_by_alias_for_tenants`; `get_by_alias` is removed from `UpstreamRepository`, because no caller needs a single-tenant alias lookup after the change below. The `(tenant_id, alias)` unique index still enforces uniqueness.
- **Proxy alias resolution in one query**: proxy-time resolution calls `list_by_alias_for_tenants(alias, tenant_chain, tags)` once (`WHERE alias = ? AND tenant_id IN (chain)`, unique index `(tenant_id, alias)`, plugin rows `LEFT JOIN`ed) instead of one `get_by_alias` per tenant. The domain service orders the results by chain position and applies the existing shadowing, visibility, and disabled-upstream rules unchanged.
- **Proxy route matching in one query**: `find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)` takes an ordered list of upstream IDs: the selected (closest) upstream first, then the merge-chain ancestor upstreams closest-first. Proxy resolution already falls back to routes attached to ancestor upstreams, so one call covers all of them. The method loads the enabled HTTP candidates in one query (`WHERE tenant_id IN (chain) AND upstream_id IN (upstream_ids) AND enabled = true AND match_type = 'http'`, joined with `oagw_route_http_match` and `oagw_route_method` and filtered on the request method, one row per route; it selects only the columns matching reads, not the JSON). It selects the winner with the same matching logic as the in-memory repository, in this order: upstream preference (list order), tenant chain position, longest path prefix, higher priority number, and lowest route ID. It then loads the winning route in full, with its method and plugin rows, in one more query, and its tag rows when `tags` is `Tags::Load`. It replaces the per-tenant `find_matching` loop in the domain service.
- **Management hierarchy checks in one query**: on upstream create and update, the closest ancestor upstream with the alias is found once with `list_by_alias_for_tenants(alias, ancestors, Tags::Skip)` and ordered by chain position in Rust. The result is passed to both the bind validation (`bind.rs`) and the budget allocation check (`budget.rs`), which previously each walked the ancestors with one `get_by_alias` per tenant. Descendant budget checks already load the descendant tree's upstreams with one `list_by_alias_for_tenants` call, also without tags.
- **Tags**: the proxy never reads tags, so it passes `Tags::Skip` and both reads return none. The SDK's `resolve_proxy_target` returns the full effective configuration and passes `Tags::Load`. The management hierarchy and budget checks read neither and pass `Tags::Skip`.
- **Query count**: an HTTP proxy request costs three queries regardless of tenant hierarchy depth and of how many ancestor upstreams are searched: upstreams for the chain with their plugin rows; the candidate routes that allow the method, with their path; the winning route with its method and plugin rows. With tags, two more: the upstreams' and the winning route's tag rows.
- **Owner**: `update` leaves `managed_by` as stored and returns the stored value, as it does for a route's `upstream_id`.
- **Registry keys**: `list_registry_keys` returns the `(tenant_id, id)` of every registry-managed row, across all tenants, for the startup reconcile (see [Startup Provisioning](#startup-provisioning-from-the-types-registry)). It is one query (index `(managed_by)`) and the only read without a tenant filter.
- **Uniqueness**: the `(tenant_id, alias)` unique index rejects duplicates. The repository maps the violation to `RepositoryError::Conflict` with backend-neutral detection.
- **Existing IDs and tenant match**: on both backends, `create` with an existing ID returns `RepositoryError::Conflict`, and route `update` matches on `(id, tenant_id)`.
- **Not found**: scoped reads and writes that match no row return `RepositoryError::NotFound`.
- **Listing**: offset pagination (`top`, `skip`) ordered by `id` ascending on both backends.
- **Canonical order**: the Control Plane canonicalizes tags and HTTP methods before every create and update. Tags are de-duplicated and sorted ascending in byte order. Methods are de-duplicated and sorted in enum order (`GET, POST, PUT, DELETE, PATCH`). Both backends return them in that order in API responses; the database repositories sort both in application code after loading, because an `ORDER BY` follows the database collation, not byte order.

Later optimizations, none of which change the schema:

- Column projections for checks that need only some columns (rate-limit budget checks, the route match-conflict check)
- Config caching (ADR-0007, ADR-0008), which removes the database from the proxy path on a cache hit and needs its own cross-replica freshness decision
- Indexed SQL route matching (see [ADR-0009 Appendix B](./0009-storage-schema.md#appendix-b-queries-and-access-frequency) for today's candidate query), only if measurements show the in-Rust matching is a bottleneck; it keeps the same ordering as `select_route` (upstream preference, tenant chain position, longest path prefix, higher priority number, lowest route ID)

### Transactions

`toolkit-db` provides `DBProvider::transaction`, `transaction_with_config` (isolation level and access mode via `TxConfig`), and `Db::transaction_with_retry` (retries on backend-classified contention errors).

- Each repository create and update runs in one transaction that it opens itself. Create inserts the parent row and its child rows. Update replaces the parent row and replaces its child rows. Deletes are single statements, atomic without a transaction. The repository traits take no transaction handle.
- Management reads (`get_by_id` and `list` of both repositories) run in one read-only REPEATABLE READ transaction (`transaction_with_config`), so the parent and child queries see one snapshot. Under the default READ COMMITTED each statement takes a new snapshot: a read racing an update could return the old parent row with the new child rows, and a route read racing a delete could find the route row without its match row and fail with an internal error. The route overlap check on create and update lists routes the same way, so it is covered too. `SQLite` runs every transaction serializable and treats read-only as a hint.
- Proxy reads (`list_by_alias_for_tenants` and `find_matching_in_tenants`) run without a transaction, which keeps the hot path at three statements. The statements of one request can therefore see different commits. A request racing a configuration write can select a route on its path from before the write and load it as written after it, or combine an upstream with a route of the configuration that replaced it; a winner deleted or disabled in between does not match. Such a request sees only its own tenant chain's committed rows and is not retried; the next request sees the new configuration.
- Deleting an upstream writes with one `DELETE` statement; the Control Plane reads the upstream first to check its owner. The foreign-key cascade removes its tags and plugin bindings, its routes, and the routes' child rows, so no state exists in which the routes are gone but the upstream remains. A route created concurrently is either rejected by the composite foreign key or removed with the upstream. The Control Plane deletes the upstream first and then calls `RouteRepository::delete_by_upstream`, which is a no-op on the database path; the in-memory repositories have no cascade and remove the tenant's routes in that call. Only the database closes the window: there the foreign key rejects a route created after the delete, while in memory a route create that passed its upstream check before the delete can still land afterwards and stay orphaned.
- Upstream delete returns no route IDs. Rate-limit buckets of deleted routes are reclaimed by the per-replica sweep that evicts buckets at rest (a full token bucket, or an empty sliding window on the limiter's fixed sub-window grid; both behave exactly like new ones), so cleanup does not depend on which replica or API handled the delete.
- No cross-repository unit of work is introduced in this step. Custom plugin garbage collection will need one, because binding changes and `oagw_plugin.gc_eligible_at` must change together; that is decided with custom plugin support.
- The rate-limit budget check reads sibling upstreams and then writes. This read-then-write race already exists with the in-memory repositories and is a known gap, not a regression.
- The route overlap check lists the upstream's routes and then inserts. Two concurrent creates can both pass it and store overlapping routes, with either backend. This is a known gap and harmless: route selection breaks the tie by the lowest route ID, so matching stays deterministic.

### Tenant Scope in the Repositories

The database repositories build their own `AccessScope` (`AccessScope::for_tenant` and `for_tenants`). TOOLKIT-SEC-002 forbids manual `AccessScope` construction; this ADR records an exception for OAGW's repositories.

- The scope is a tenant filter, not an authorization grant. Every tenant ID in it comes from the caller's `SecurityContext` (`subject_tenant_id`), from the ancestor chain or, for a budget check, the descendants the tenant resolver returns for that tenant, or, for a write, from the row the Control Plane built for that tenant. No request field names a tenant.
- `list_registry_keys` uses an unconstrained scope (`AccessScope::allow_all`): the startup reconcile owns registry rows in every tenant. Only `post_init` reaches it; no request does.
- Authorization decisions stay with `PolicyEnforcer` where OAGW makes them: proxy invocation and binding to an ancestor's upstream.
- Other gears' repositories build tenant scopes the same way (for example pricing, ledger, mini-chat, products, event-broker, file-storage, and graph-storage). Moving to enforcer-issued scopes is a platform-wide change, not one for OAGW alone.

### Startup Provisioning from the Types Registry

During `post_init`, OAGW reads the upstream and route instances that other gears, or the types-registry `entities` configuration, registered in the types registry, and makes the stored rows match them. Each instance carries a fixed UUID from its GTS identifier, which becomes the row ID. The registry is the source of truth for these rows; before persistence, every boot re-created them from it, and both backends keep that behavior.

- **Ownership**: every upstream and route records its owner in `managed_by`. Rows created through the Management API or the SDK are `api`; rows provisioned here are `registry`.
- **Read-only through the API**: the Management API and the SDK cannot update or delete a registry-managed row. Both fail with `failed_precondition` (HTTP 400, violation type `REGISTRY_MANAGED`, subject `managed_by`; SDK consumers match `oagw_sdk::precondition::REGISTRY_MANAGED` and `MANAGED_BY`); the row changes only with its registry instance, at the next boot. A route created through the API on a registry-managed upstream is allowed and is `api`-managed. Provisioning writes through a separate Control Plane interface that only `post_init` holds; it runs the same validation as API writes and cannot change an `api` row.
- **Overlaps checked first**: before anything is written, the registry's routes are checked against each other by the Management API's overlap rule (same tenant and upstream, both enabled, same path and priority, a shared method). Registry routes that overlap fail startup.
- **Removal next**: registry-managed rows whose instance the registry no longer has are deleted, routes before upstreams. A registry route whose instance now names another upstream is deleted in this step too, and re-created under the same ID in the next, because a route never moves between upstreams; that frees its old upstream, and the upstream's alias, in the same boot. An upstream that routes still use (API routes, or registry routes whose instance still names it) is kept with a warning, because deleting it would cascade those routes away; once they are gone, the next boot removes it. The in-use check and the delete are two steps, so an API route that another replica, already serving, creates on the upstream between them is removed by the cascade. Closing that gap needs a repository method that locks the upstream row, counts its routes and deletes it in one transaction; until then it is a known gap. A kept upstream keeps its alias, so a registry upstream that takes the alias over fails startup with an error naming the routes that keep the old one.
- **Then create or update**: a missing instance is created; a stored row whose content differs is replaced in full and logged with the differing fields; an unchanged row is left alone. The overlap check of a registry route write skips the other registry routes, which either end in the state checked first or were removed, so registry routes can swap or hand over paths in one release; API and registry routes still conflict both ways. An instance that moved to another tenant is removed from the old tenant by the removal step and created in the new one.
- **Aliases follow the instance**: on every update the registry writer applies the create rules to the instance's endpoints and explicit alias, so new endpoints or a new explicit alias move the alias in place, hostname to IP included. The row keeps its ID, its registry routes and any API routes on it; deleting and re-creating it instead would cascade those API routes away. Stored instances are applied before new ones, so a new instance can take an alias another one gives up in the same boot; two registry upstreams cannot swap aliases in one release, because the first update conflicts. The Management API keeps the alias immutable ([ADR-0010](./0010-resource-identification.md)). Moving an alias has the effects of any alias change: `/proxy/{old alias}` answers 404, upstreams in descendant tenants that share the old alias no longer bind to this one and stop inheriting its sharing, plugins, rate limit and budget, and descendant upstreams that already use the new alias bind to it without their budget allocations being re-checked.
- **Failures fail startup**: a create or update the Control Plane rejects (for example an alias clash with an `api` upstream, or a route instance that names an `api` upstream: deleting that upstream through the API would cascade the route away, and every later boot would fail to re-create it), a lookup or listing error, or a stored `api` row that holds an instance's ID. Such a row is never overwritten.
- **Replicas booting together** race on the same rows: a create that conflicts falls back to a lookup and continues when the row now exists, a delete that finds nothing counts as done, and concurrent updates write the same content. A route re-created on its new upstream that conflicts continues only when the row found is the registry's route with the wanted content.

### Endpoint Pool Freshness Across Replicas

`PingoraEndpointSelector` caches each upstream's endpoint pool by `upstream_id`. Management handlers invalidate that cache only on the replica that handled the write. With a shared database, other replicas would keep the old endpoints. The selector **MUST** rebuild a cached pool whenever the endpoints passed to `select()` differ from the cached list. This needs no messaging between instances.

An upstream deleted through another replica leaves its pool cached here, and each pool runs a background task (DNS every 30 s, TCP health checks every 10 s). The selector therefore drops a pool not selected for 10 minutes, checking every minute, and rebuilds it if it is used again. Dropping a pool, by this sweep, by invalidation, or by a rebuild, **MUST** stop its background task. Updates and deletes through the SDK invalidate the local pool as the Management API handlers do.

### Consequences

* OAGW adds dependencies on `toolkit-db` and SeaORM, declares the `db` capability, and ships SeaORM migrations for the nine tables.
* `resolution.rs` replaces its per-tenant `get_by_alias` and `find_matching` loops with one `list_by_alias_for_tenants` call and one `find_matching_in_tenants` call over the ordered upstream IDs (selected upstream, then ancestor upstreams closest-first). Both repository implementations provide `find_matching_in_tenants`; `find_matching` is removed from `RouteRepository`.
* `bind.rs` and `budget.rs` take the closest ancestor upstream as an input instead of looking it up; upstream create and update look it up once with `list_by_alias_for_tenants`.
* `gear.rs` selects the repository implementation from `ctx.db()`. The in-memory repositories remain a supported runtime mode, not only test doubles, and must gain every capability the database repositories gain.
* ADR-0009 is the single schema source of truth. Later features add tables (`oagw_plugin`) or columns (concurrency limits, circuit breaker) without moving stored data.
* CI runs the repository conformance tests against the in-memory repositories, SQLite, PostgreSQL, and MySQL, and the migration tests against SQLite, PostgreSQL, and MySQL. The default OAGW build compiles the SQLite and PostgreSQL backends; MySQL needs the `mysql` feature (see [Backend Selection](#backend-selection)).
* With the in-memory backend, configuration is lost on restart. Operators who need durability must configure a database.
* Registry content applies on every boot with either backend, so a registry change takes effect on the next restart. During a rolling deploy, replicas of two versions can hold different registry content. Each boot applies its own version's content, and proxy requests read the database, so all replicas serve what the last boot wrote: an old-version replica that boots after new ones writes the old content back, and removes instances only the new version has, until a new-version replica boots again.
* Registry-managed upstreams and routes cannot be changed through the Management API or the SDK; operators change the registry instance instead.
* Stored upstreams outlive the SSRF policy they were validated against: before persistence, a restart re-created and re-validated every upstream. Alias resolution therefore re-checks the selected upstream's endpoint hostnames against the current hostname deny-list and rejects a denied one with the same validation error as create, so a tightened policy also applies to stored and registry-provisioned upstreams. Resolved IPs were already checked on every connection.

### Confirmation

* One repository conformance test suite runs against the in-memory repositories and the database repositories on SQLite, PostgreSQL, and MySQL, and all pass with identical results, including round-trips of absent versus empty optional fields.
* Migration tests create the schema on SQLite, PostgreSQL, and MySQL, including the composite foreign keys, store values at the stored field limits and JSON documents over 64 KiB, and keep timestamps to the microsecond past 2038.
* A test shows the database rejects a child row whose `tenant_id` differs from its parent's.
* A gear startup test without database configuration uses the in-memory repositories; one with a `database` section but no database handle (no global `database:` section) fails startup; one with `database: null` uses the in-memory repositories. An unreachable database fails startup in `toolkit-db`, before OAGW's `init` runs; OAGW has no test of its own for it.
* A restart test shows upstreams and routes created through the Management API survive restart with a database configured.
* A provisioning test boots twice against the same database without errors and leaves the stored rows unchanged.
* Provisioning tests show that a boot applies changed registry content, replaces a route moved to another upstream (also onto a new upstream with the old one's alias, on SQLite too), lets registry routes swap and hand over paths, rejects registry routes that overlap each other, removes instances the registry dropped, keeps an upstream that API routes still use and names those routes when its alias is wanted, moves an instance to its new tenant, and refuses to overwrite an `api` row with an instance's ID. Race tests cover a route another replica created, removed, or moved concurrently.
* A test shows Management API update and delete of registry-managed upstreams and routes fail with `REGISTRY_MANAGED` and change nothing, while an API route on a registry-managed upstream is accepted.
* Conformance tests show `managed_by` survives an update that names another owner, and `list_registry_keys` returns registry rows across tenants and nothing else, on every backend.
* A query-count test shows upstream create and update issue the same number of ancestor lookup queries with 1 and with 5 ancestor tenants.
* A query-count test shows a proxy request issues the same number of queries for tenant chains of 1 and 5 tenants, and that list calls issue the same number of queries for page sizes 1 and 50.
* Conformance tests cover alias shadowing and route selection across a multi-level hierarchy for both `list_by_alias_for_tenants` resolution and `find_matching_in_tenants`.
* An endpoint selector test shows a changed endpoint list rebuilds the pool without an explicit invalidation.
* A resolution test shows an upstream stored before its hostname was denied stops resolving once the policy denies it.

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
- [ADR: Control Plane Caching](./0007-data-plane-caching.md) and [ADR: State Management](./0008-state-management.md) — config caching to add on top of this ADR
- [ADR: Rate Limiting](./0004-rate-limiting.md) — rate-limit counters stay in memory per instance; this ADR does not persist them

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-config-persistence` — Opt-in persistence with in-memory fallback and fail-fast on an unusable database
* `cpt-cf-oagw-fr-upstream-mgmt` — Upstream CRUD persisted in `oagw_upstream` and its child tables
* `cpt-cf-oagw-fr-route-mgmt` — Route CRUD persisted in `oagw_route` and its child tables
* `cpt-cf-oagw-nfr-multi-tenancy` — Every table carries `tenant_id` and is accessed through the secure ORM
* `cpt-cf-oagw-db-schema` — Storage schema implementation
