<!-- Updated: 2026-04-07 by Constructor Tech -->

# Storage Schema — Portable Relational Baseline with JSON Blobs


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Core Tables](#core-tables)
  - [Non-Negotiable Invariants](#non-negotiable-invariants)
  - [Cascading Delete Semantics (FK Constraints)](#cascading-delete-semantics-fk-constraints)
  - [Secure ORM Scoping Requirements](#secure-orm-scoping-requirements)
  - [Plugin Reference Semantics](#plugin-reference-semantics)
  - [Application-Level Validation Rules](#application-level-validation-rules)
  - [Route Selection Determinism](#route-selection-determinism)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Portable relational baseline + JSON blobs](#portable-relational-baseline--json-blobs)
  - [PostgreSQL-first schema](#postgresql-first-schema)
  - [Fully normalized (no JSON)](#fully-normalized-no-json)
- [Rationale](#rationale)
- [Appendix A: Schema Tables (Illustrative)](#appendix-a-schema-tables-illustrative)
  - [`oagw_upstream`](#oagw_upstream)
  - [`oagw_upstream_tag`](#oagw_upstream_tag)
  - [`oagw_route`](#oagw_route)
  - [`oagw_route_http_match`](#oagw_route_http_match)
  - [`oagw_route_method`](#oagw_route_method)
  - [`oagw_route_grpc_match`](#oagw_route_grpc_match)
  - [`oagw_route_tag`](#oagw_route_tag)
  - [`oagw_plugin` (custom plugins)](#oagw_plugin-custom-plugins)
  - [`oagw_upstream_plugin`](#oagw_upstream_plugin)
  - [`oagw_route_plugin`](#oagw_route_plugin)
  - [`oagw_registry_state`](#oagw_registry_state)
- [Appendix B: Queries and Access Frequency](#appendix-b-queries-and-access-frequency)
  - [Custom plugins (deferred with `oagw_plugin`)](#custom-plugins-deferred-with-oagw_plugin)
- [Appendix C: Plugin Binding Examples (Illustrative)](#appendix-c-plugin-binding-examples-illustrative)
  - [Plugin Reference in API Payloads](#plugin-reference-in-api-payloads)
  - [Plugin Bindings in the Database](#plugin-bindings-in-the-database)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-storage-schema`

> **Implementation note**: The first persistence step implements this schema for upstreams and routes, with every table except `oagw_plugin`, behind an opt-in database with in-memory fallback. `oagw_plugin` is added with custom plugin support. See [ADR: Optional Persistence](./0018-optional-persistence.md).

## Context and Problem Statement

OAGW persists configuration for upstreams, routes, and plugins. This data is read frequently (proxy hot path) and written infrequently (management operations). OAGW must support multiple SQL backends via `toolkit-db` (PostgreSQL, MySQL, SQLite). The schema must be portable and preserve consistent behavior and security guarantees across backends.

## Decision Drivers

* Tenant isolation: all reads/writes must be tenant-scoped via the secure ORM layer
* Tenant hierarchy behavior: alias resolution and effective configuration must support shadowing and inheritance semantics
* Hot-path lookups:
  * Resolve upstream by `(tenant hierarchy, alias)`
  * Match HTTP routes by `(upstream_id, method, longest path prefix, priority)`
  * Match gRPC routes by `(upstream_id, service, method, priority)`
* Deletion semantics: deleting an upstream must delete its routes and dependent match/binding rows
* Plugin requirements: ordered plugin chains with per-binding config; plugin references must support both built-in named IDs and custom UUID-backed IDs; custom plugin lifecycle requires "in use" detection and GC eligibility timestamps
* Portability: avoid correctness depending on backend-specific features (e.g. JSON operators, partial indexes)

## Considered Options

* Portable relational baseline + JSON blobs
* PostgreSQL-first schema (JSONB operators, specialized indexes)
* Fully normalized configuration (no JSON)

## Decision Outcome

Chosen option: "Portable relational baseline + JSON blobs", because it keeps hot-path selectors in indexed relational columns while allowing evolving configuration in JSON fields.

### Core Tables

- `oagw_upstream`: tenant-scoped root config (unique per `(tenant_id, alias)`)
- `oagw_route`: belongs to upstream, with `match_type` (http/grpc), `priority`, `enabled`
- `oagw_route_http_match` / `oagw_route_grpc_match`: typed match keys for deterministic route selection
- `oagw_route_method`: HTTP method allowlists
- `oagw_upstream_tag` / `oagw_route_tag`: discovery tags
- `oagw_plugin`: custom (UUID-backed) plugins only; built-in plugins are not persisted
- `oagw_upstream_plugin` / `oagw_route_plugin`: ordered plugin bindings with per-binding config
- `oagw_registry_state`: one installation-wide row gating the registry reconcile (generation, content hash, lease); not tenant data ([ADR: Optional Persistence](./0018-optional-persistence.md))

### Non-Negotiable Invariants

- All reads and writes are tenant-scoped through the secure data access layer (parameter binding + tenant scoping), except the registry reconcile's listing of registry rows, which reads across tenants, and the installation-wide `oagw_registry_state` row ([ADR: Optional Persistence](./0018-optional-persistence.md))
- Every configuration table carries `tenant_id`, including child tables; a child row's `tenant_id` always equals its parent's
- Multi-table configuration updates are applied atomically (single transaction per logical write)
- Alias resolution and effective configuration merges preserve tenant-hierarchy semantics
- Route matching uses typed match key tables (no inference from opaque JSON)
- Multi-value associations that affect selection/filtering (methods, tags, plugin bindings) are stored in join tables
- Plugin bindings preserve explicit ordering and per-binding config (`position` unique per parent, contiguous from 0)
- Plugin identifiers in bindings support:
  - built-in named IDs (resolved from the built-in registry)
  - custom plugins (resolved by UUID in `oagw_plugin`)
- Named plugins are not persisted as rows:
  - `oagw_plugin` stores custom (UUID-backed) plugins only
  - Named plugins are referenced in bindings via `plugin_ref` with `plugin_uuid = NULL`
  - The binding tables do not have an FK to `oagw_plugin`

### Cascading Delete Semantics (FK Constraints)

- Deleting an upstream deletes its routes.
- Deleting an upstream or route deletes dependent tag, match, method, and plugin-binding rows.
- Child foreign keys are composite: `(tenant_id, upstream_id)` references `oagw_upstream (tenant_id, id)` and `(tenant_id, route_id)` references `oagw_route (tenant_id, id)`, each with `ON DELETE CASCADE`. `oagw_route (tenant_id, upstream_id)` references `oagw_upstream (tenant_id, id)` the same way. The database therefore rejects a child row whose `tenant_id` differs from its parent's.

### Secure ORM Scoping Requirements

- Every configuration table, including tags, methods, match keys, and plugin bindings, carries a `tenant_id` column and declares it as the secure ORM tenant column.
- All configuration tables are accessed with the same tenant scope (`AccessScope::for_tenants`). Every one carries its own tenant column; a joined child row is confined by the composite join key `(tenant_id, parent_id)` to its scoped parent's tenant, not by a separate predicate or `EXISTS`.
- On write, a child row takes its `tenant_id` from its parent. The composite foreign keys above enforce this at the database level.
- Direct, unscoped reads/writes of any table are forbidden, except the registry reconcile's listing above and `oagw_registry_state`, which has no tenant (`#[secure(unrestricted)]`, `AccessScope::allow_all()`).

### Plugin Reference Semantics

- `plugin_ref` is always stored in bindings.
- `plugin_uuid` is stored only for UUID-backed plugins (custom); it is NULL for named plugins.
- Binding tables do not have an FK to `oagw_plugin`.
- `auth_plugin_ref` / `auth_plugin_uuid` are stored as scalar columns on `oagw_upstream` to support efficient "in use" checks.

### Application-Level Validation Rules

- `plugin_ref` must be non-empty and canonicalized (trimmed). (Planned; enforced today: at most 256 bytes, no control characters.)
- For HTTP routes, `path_prefix` must be normalized and must not exceed a fixed maximum number of path segments. (Planned; enforced today: at most 2048 bytes, no control characters.)
- If `plugin_uuid` is set on a binding row:
  - `plugin_uuid` must be a valid UUID.
  - `plugin_ref` must represent the same UUID (exact format is an application concern; comparisons must be done on the parsed UUID).
  - The referenced row must exist in `oagw_plugin` within scope.
- If `plugin_uuid` is NULL on a binding row:
  - `plugin_ref` must resolve to a built-in plugin in the registry. (Planned, with custom plugin support.)
- `auth_plugin_ref/auth_plugin_uuid` (when present) must resolve to an **auth** plugin. (Planned, with custom plugin support.)
- `oagw_upstream_plugin` / `oagw_route_plugin` bindings must not reference auth plugins (auth is configured only via `auth_plugin_*`). (Planned, with custom plugin support.)
- Binding ordering:
  - `position` is unique per `(upstream_id)` / `(route_id)` by PK.
  - Positions start at 0 and are contiguous (no gaps): the repositories assign them from the list order on write.

### Route Selection Determinism

Route matching runs in the application over the candidate routes the database returns (Appendix B), not in SQL. The matching defined in [ADR: Optional Persistence](./0018-optional-persistence.md) uses the lowest route ID as the final tie-break on every backend, including the in-memory one.

To preserve deterministic selection semantics across backends and timestamp precisions, route writes must reject ambiguous route configurations (on create, and on an update that enables or changes a route). The route repositories apply the rule atomically with the write, under a lock on the upstream row (Appendix B, Q11), so two concurrent writes cannot both pass it:

- For HTTP routes, for each method bound to the route, there must not exist another enabled HTTP route under the same upstream with the same `path_prefix` and `priority`.
- For gRPC routes, there must not exist another enabled gRPC route under the same upstream with the same `(service, method)` and `priority`. (Planned with gRPC route matching; gRPC routes are neither checked nor matched today.)

### Consequences

* Good, because portable schema across PostgreSQL/MySQL/SQLite
* Good, because efficient selection for proxy hot-path scenarios
* Good, because supports built-in named IDs, custom UUID plugins, delete-in-use detection, and GC eligibility
* Good, because every table uses the same secure ORM tenant scope, and a joined child is confined by the composite join key, so no access path needs `EXISTS`-based scoping
* Bad, because some referential integrity enforced in application code (built-in plugins not FK-backed)
* Bad, because `tenant_id` is duplicated in child tables; composite foreign keys keep it consistent with the parent
* Bad, because requires application-level validation for `plugin_ref` well-formedness

### Confirmation

Migration tests verify: schema creates successfully on all supported backends (PostgreSQL, MySQL, SQLite). Integration tests verify: cascading deletes, tenant-scoped queries, route matching determinism, plugin binding ordering.

## Pros and Cons of the Options

### Portable relational baseline + JSON blobs

* Good, because hot-path fields in indexed columns enable efficient queries
* Good, because JSON blobs allow evolving configuration without migrations
* Good, because portable across SQL backends
* Bad, because JSON fields not queryable at DB level

### PostgreSQL-first schema

* Good, because JSONB operators enable rich queries on config
* Good, because optimized for Postgres performance
* Bad, because increases divergence risk across backends
* Bad, because correctness depends on backend-specific features

### Fully normalized (no JSON)

* Good, because full DB-level validation and queryability
* Bad, because significant schema complexity and migration overhead
* Bad, because every config field change requires migration

## Rationale

- Keeping hot-path selectors in scalar columns / join tables enables efficient, portable queries.
- Separating route match keys into typed tables enables deterministic route selection without parsing JSON.
- Storing plugin references as canonical strings (`plugin_ref`) supports built-in named IDs while still allowing efficient lookups for custom plugins via `plugin_uuid`.
- Storing `auth_plugin_ref/auth_plugin_uuid` as scalar columns avoids correctness and "in use" checks depending on backend-specific JSON querying.

## Appendix A: Schema Tables (Illustrative)

The tables below are a compact summary of the logical schema above. The exact PostgreSQL DDL, with the queries each index serves, is in [`migration.sql`](../migration.sql).

Notes:

- Types are logical. Physical types may differ across backends.
- No column is `TEXT`. Every string column is a sized `VARCHAR`: a column holding a validated field is sized to that field's limit (alias 253, tag 128, path 2048, gRPC name 256, protocol and plugin references 256), and a column holding a closed set of values is `VARCHAR(16)`. Limits count bytes and sizes count characters, so every accepted value fits.
- `JSON` is the backend's JSON type: `jsonb` on PostgreSQL, `json` on MySQL, text on SQLite. The DB validates the document but never queries into it.
- `oagw_plugin` is not implemented yet: its `VARCHAR(n)` sizes are fixed with the custom plugin API's field limits, and `source_code` still needs a bounded type.
- Child tables have no index beyond their primary key: they are read by parent ID, which the primary key leads with. On MySQL, InnoDB also creates an index for each child foreign key `(tenant_id, parent_id)`, as it does for every foreign key without one.
- Timestamp columns must be stored in an orderable, comparable format (backend timestamp type or epoch milliseconds). All timestamps are UTC.

### `oagw_upstream`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | UUID | No | PK |
| `tenant_id` | UUID | No | Tenant scope |
| `alias` | VARCHAR(253) | No | Unique per tenant |
| `protocol` | VARCHAR(256) | No | GTS protocol identifier |
| `enabled` | BOOL | No | |
| `managed_by` | VARCHAR(16) | No | `api\|registry`; set on create, never changed (ADR-0018) |
| `schema_version` | INT | No | JSON schema version for JSON columns in this table |
| `server` | JSON | No | Endpoints |
| `auth_plugin_ref` | VARCHAR(256) | Yes | Canonical plugin identifier |
| `auth_plugin_uuid` | UUID | Yes | Parsed UUID when custom |
| `auth_config` | JSON | Yes | Config only (no plugin id) |
| `auth_sharing` | VARCHAR(16) | No | `private\|inherit\|enforce` |
| `headers` | JSON | Yes | |
| `cors` | JSON | Yes | |
| `cors_sharing` | VARCHAR(16) | No | `private\|inherit\|enforce` |
| `rate_limit` | JSON | Yes | |
| `rate_limit_sharing` | VARCHAR(16) | No | `private\|inherit\|enforce` |
| `plugins_sharing` | VARCHAR(16) | Yes | `private\|inherit\|enforce`; NULL when no plugins configuration (distinguishes absent from empty list) |
| `created_at` | TIMESTAMP | No | |
| `updated_at` | TIMESTAMP | No | |

Constraints / indexes:

- Unique: `(tenant_id, alias)` (alias resolution per proxied request; ancestor and budget lookups on create/update)
- Unique: `(tenant_id, id)` (target of child composite FKs; upstream listing by tenant ordered by id)
- Index: `(managed_by)` (the registry reconcile lists registry rows across tenants)
- Deferred to `oagw_plugin`: `(auth_plugin_uuid)` for the plugin "in use" check

### `oagw_upstream_tag`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `upstream_id` | UUID | No | PK part, FK `(tenant_id, upstream_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `tag` | VARCHAR(128) | No | PK part |

Indexes: primary key only (rows are read by parent ID).

### `oagw_route`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | UUID | No | PK |
| `tenant_id` | UUID | No | Tenant scope |
| `upstream_id` | UUID | No | FK `(tenant_id, upstream_id)` (cascade) |
| `enabled` | BOOL | No | |
| `priority` | INT | No | Higher wins after specificity |
| `match_type` | VARCHAR(16) | No | `http\|grpc` |
| `managed_by` | VARCHAR(16) | No | `api\|registry`; set on create, never changed (ADR-0018) |
| `schema_version` | INT | No | JSON schema version for JSON columns in this table |
| `match_config` | JSON | Yes | Query allowlist, suffix mode, etc. |
| `cors` | JSON | Yes | |
| `rate_limit` | JSON | Yes | |
| `rate_limit_sharing` | VARCHAR(16) | No | `private\|inherit\|enforce` |
| `plugins_sharing` | VARCHAR(16) | Yes | `private\|inherit\|enforce`; NULL when no plugins configuration (distinguishes absent from empty list) |
| `created_at` | TIMESTAMP | No | |
| `updated_at` | TIMESTAMP | No | |

Constraints / indexes:

- Unique: `(tenant_id, id)` (target of child composite FKs; route listing by tenant ordered by id)
- Index: `(tenant_id, upstream_id)` (proxy route candidates, route listing, the match-conflict check, the cascade from `oagw_upstream`)
- Index: `(managed_by)` (the registry reconcile lists registry rows across tenants)

### `oagw_route_http_match`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK, FK `(tenant_id, route_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `path_prefix` | VARCHAR(2048) | No | |

Indexes: primary key only (rows are read by parent ID).

### `oagw_route_method`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK part, FK `(tenant_id, route_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `method` | VARCHAR(16) | No | PK part |

Indexes: primary key only (rows are read by parent ID).

### `oagw_route_grpc_match`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK, FK `(tenant_id, route_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `service` | VARCHAR(256) | No | |
| `method` | VARCHAR(256) | No | |

Indexes: primary key only (rows are read by parent ID).

### `oagw_route_tag`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK part, FK `(tenant_id, route_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `tag` | VARCHAR(128) | No | PK part |

Indexes: primary key only (rows are read by parent ID).

### `oagw_plugin` (custom plugins)

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | UUID | No | PK |
| `tenant_id` | UUID | No | Tenant scope |
| `plugin_type` | VARCHAR(16) | No | `auth\|guard\|transform` |
| `name` | VARCHAR(n) | No | Unique per tenant |
| `description` | VARCHAR(n) | Yes | |
| `schema_version` | INT | No | JSON schema version for JSON columns in this table |
| `config_schema` | JSON | No | |
| `source_code` | TEXT | No | |
| `last_used_at` | TIMESTAMP | Yes | |
| `gc_eligible_at` | TIMESTAMP | Yes | |
| `created_at` | TIMESTAMP | No | |
| `updated_at` | TIMESTAMP | No | |

Constraints / indexes:

- Unique: `(tenant_id, name)`
- Index: `(gc_eligible_at)`

### `oagw_upstream_plugin`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `upstream_id` | UUID | No | PK part, FK `(tenant_id, upstream_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `position` | INT | No | PK part |
| `plugin_ref` | VARCHAR(256) | No | Canonical plugin identifier |
| `plugin_uuid` | UUID | Yes | Parsed UUID when custom |
| `schema_version` | INT | No | JSON schema version for JSON columns in this table |
| `config` | JSON | Yes | |

Indexes: primary key only (rows are read by parent ID). Deferred to `oagw_plugin`: `(plugin_uuid)` for the plugin "in use" check.

### `oagw_route_plugin`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK part, FK `(tenant_id, route_id)` (cascade) |
| `tenant_id` | UUID | No | Tenant scope; equals parent's (composite FK) |
| `position` | INT | No | PK part |
| `plugin_ref` | VARCHAR(256) | No | Canonical plugin identifier |
| `plugin_uuid` | UUID | Yes | Parsed UUID when custom |
| `schema_version` | INT | No | JSON schema version for JSON columns in this table |
| `config` | JSON | Yes | |

Indexes: primary key only (rows are read by parent ID). Deferred to `oagw_plugin`: `(plugin_uuid)` for the plugin "in use" check.

### `oagw_registry_state`

One row (`id = 1`), seeded by its migration, that gates the registry reconcile on a database ([ADR: Optional Persistence](./0018-optional-persistence.md#provisioning-from-the-types-registry)). No `tenant_id`: it is installation-wide.

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | SMALLINT | No | PK; always 1 |
| `generation` | BIGINT | Yes | Highest deployment generation that claimed the registry (`registry_provisioning.generation`); only increases |
| `content_hash` | VARCHAR(64) | Yes | Hex SHA-256 of OAGW's version and the registry content of the last clean reconcile; cleared when a run takes the lease |
| `applied_at` | TIMESTAMP | Yes | When `content_hash` was recorded (database clock) |
| `lease_owner` | UUID | Yes | Process holding the reconcile lease |
| `lease_expires_at` | TIMESTAMP | Yes | When the lease lapses; written by the database clock only |

Indexes: primary key only.

## Appendix B: Queries and Access Frequency

The queries the database repositories run, as SQL. The secure ORM adds the `tenant_id` predicate. Exact DDL: [`migration.sql`](../migration.sql).

Frequency:

- **per miss**: a proxied request whose lookup misses the proxy configuration cache ([ADR: Proxy Configuration Cache](./0020-proxy-config-cache.md), on by default with a database). Q1 runs on an alias-section miss, Q15 on a route-section miss; a hit runs no query. Loads per replica are bounded by the active keys per TTL plus reloads after writes.
- **per request**: every proxied request with the cache off (`proxy_cache.enabled: false`): Q1–Q3, independent of tenant depth. With the cache on, Q2–Q3 run only for the SDK's `resolve_proxy_target` (per call). The proxy never reads tags and does not load them.
- **per call**: Management API / SDK calls.
- **per check**: the registry reconcile's gate, once per pod per re-check interval (`registry_provisioning.recheck_interval`, default 5 minutes).
- **per run**: a registry reconcile, which runs only when the registry content (or OAGW's version) differs from the last clean run's, or no run since the last one that took the lease was clean.

| # | Query | Frequency | Index |
|---|---|---|---|
| Q1 | Upstreams by alias in the tenant chain, with their plugins | per miss; per request with the cache off; per call (SDK with tags) | unique `(tenant_id, alias)`; child PK |
| Q2 | Enabled HTTP route candidates that allow the request method: selection columns only | per request with the cache off; per call (SDK with tags) | `(tenant_id, upstream_id)`; child PKs |
| Q3 | The winning route in full, with its path, methods and plugins; re-checked against the request, and Q2–Q3 run once more on a mismatch | per request with the cache off; per call (SDK with tags) | PK; child PKs |
| Q4 | Tags of the Q1 upstreams and the Q3 route (SDK `resolve_proxy_target` only) | per call | child PK |
| Q5 | Get / update / delete by ID | per call | PK |
| Q6 | List upstreams | per call | unique `(tenant_id, id)` |
| Q7 | List routes (all, or of one upstream) | per call | unique `(tenant_id, id)` / `(tenant_id, upstream_id)` |
| Q8 | Q1 for ancestors (create/update) or descendants (rate-limit budget); no tags | per call | unique `(tenant_id, alias)` |
| Q9 | Children of read or written rows; replace on update | per call | child PK |
| Q10 | Registry rows across tenants, in full (`RegistryRowRepository`, registry reconcile only): the rows, then their children as Q9 (3 statements for upstreams, 6 for routes) | per run | `(managed_by)`; child PKs |
| Q11 | Route overlap check inside an enabled HTTP route's create or update transaction: lock the upstream the route is written to, then the enabled HTTP routes of that upstream with the same path and priority that share a method (selection columns only), then the write | per call | unique `(tenant_id, id)`; `(tenant_id, upstream_id)`; child PKs |
| Q12 | Registry state: generation and content hash | per check | PK |
| Q13 | Registry state: claim the generation, take, renew or release the lease, record the hash; one conditional `UPDATE` each | per run | PK |
| Q14 | Delete a dropped registry upstream if unused: lock the row, look for one route, delete; one transaction | per run | PK; `(tenant_id, upstream_id)` |
| Q15 | Every enabled HTTP route of several upstreams, in full with its path, methods and plugins (`list_proxy_routes`, the cache's route load): one query for all the missing upstreams (chunked past the backend's bind-parameter limit, which a lookup never reaches) | per miss | `(tenant_id, upstream_id)`; child PKs |

Joins match the composite foreign key `(tenant_id, parent_id)`, and the tenant scope applies to the parent: the join key's tenant keeps every child row in its scoped parent's tenant, so the children need no predicate of their own. Upstream delete is one `DELETE`; routes and child rows cascade through `(tenant_id, upstream_id)` and the child PKs.

```sql
-- Q1, Q8
SELECT u.*, p.*
FROM oagw_upstream u
LEFT JOIN oagw_upstream_plugin p ON p.tenant_id = u.tenant_id AND p.upstream_id = u.id
WHERE u.alias = $1 AND u.tenant_id IN (...);

-- Q2: one row per route that allows the method
SELECT r.id, r.tenant_id, r.upstream_id, r.priority, hm.path_prefix
FROM oagw_route r
JOIN oagw_route_http_match hm ON hm.tenant_id = r.tenant_id AND hm.route_id = r.id
JOIN oagw_route_method m      ON m.tenant_id  = r.tenant_id AND m.route_id  = r.id
WHERE r.upstream_id IN (...) AND r.enabled AND r.match_type = 'http'
  AND m.method = $1 AND r.tenant_id IN (...);

-- Q3: one row per (method, plugin)
SELECT r.*, hm.path_prefix, m.method, p.*
FROM oagw_route r
JOIN oagw_route_http_match hm ON hm.tenant_id = r.tenant_id AND hm.route_id = r.id
JOIN oagw_route_method m      ON m.tenant_id  = r.tenant_id AND m.route_id  = r.id
LEFT JOIN oagw_route_plugin p ON p.tenant_id  = r.tenant_id AND p.route_id  = r.id
WHERE r.id = $1 AND r.upstream_id IN (...) AND r.enabled AND r.match_type = 'http'
  AND r.tenant_id = $2;

-- Q15: Q3 without the winner filter, one row per (route, method, plugin);
-- grouped by (tenant_id, upstream_id) in the application.
SELECT r.*, hm.path_prefix, m.method, p.*
FROM oagw_route r
JOIN oagw_route_http_match hm ON hm.tenant_id = r.tenant_id AND hm.route_id = r.id
JOIN oagw_route_method m      ON m.tenant_id  = r.tenant_id AND m.route_id  = r.id
LEFT JOIN oagw_route_plugin p ON p.tenant_id  = r.tenant_id AND p.route_id  = r.id
WHERE r.upstream_id IN (...) AND r.enabled AND r.match_type = 'http'
  AND r.tenant_id IN (...);

-- Q4
SELECT * FROM oagw_upstream_tag WHERE upstream_id IN (...) AND tenant_id IN (...);
SELECT * FROM oagw_route_tag    WHERE route_id = $1     AND tenant_id IN (...);

-- Q5
SELECT * FROM oagw_upstream WHERE id = $1 AND tenant_id = $2;
DELETE FROM oagw_upstream  WHERE id = $1 AND tenant_id = $2;

-- Q6
SELECT * FROM oagw_upstream WHERE tenant_id = $1 ORDER BY id LIMIT $2 OFFSET $3;

-- Q7
SELECT * FROM oagw_route WHERE tenant_id = $1 [AND upstream_id = $2] ORDER BY id LIMIT $3 OFFSET $4;

-- Q9: one per child table
SELECT * FROM oagw_route_method WHERE route_id IN (...) AND tenant_id IN (...);

-- Q10 (children as Q9)
SELECT * FROM oagw_upstream WHERE managed_by = 'registry';
SELECT * FROM oagw_route    WHERE managed_by = 'registry';

-- Q11: in the route write's transaction, READ COMMITTED on PostgreSQL and
-- MySQL, retried on contention. The lock serializes route writes on one
-- upstream; it is taken as in Q14 (MySQL: two locking reads, SQLite: a
-- no-op UPDATE).
SELECT * FROM oagw_upstream WHERE tenant_id = $4 AND id = $1 FOR UPDATE;
-- One row per (route, shared method); none in the normal case. Path
-- equality is byte-exact on every backend (binary collation on MySQL).
-- The route being updated is left out of the result in the application.
SELECT r.id, m.method
FROM oagw_route r
JOIN oagw_route_http_match hm ON hm.tenant_id = r.tenant_id AND hm.route_id = r.id
JOIN oagw_route_method m      ON m.tenant_id  = r.tenant_id AND m.route_id  = r.id
WHERE r.upstream_id = $1 AND r.enabled AND r.match_type = 'http'
  AND r.priority = $2 AND hm.path_prefix = $3 AND m.method IN (...)
  AND r.tenant_id = $4;
-- then the route insert or update and its children (Q9)

-- Q12
SELECT generation, content_hash FROM oagw_registry_state WHERE id = 1;

-- Q13: rows affected = 1 is the answer. now() is the database clock
-- (MySQL NOW(6), SQLite datetime('now'), compared through datetime()).
UPDATE oagw_registry_state SET generation = $g
 WHERE id = 1 AND (generation IS NULL OR generation < $g);                    -- claim
UPDATE oagw_registry_state
   SET lease_owner = $me, lease_expires_at = now() + 60 * INTERVAL '1 second',
       content_hash = NULL, applied_at = NULL,                                 -- until a clean finish
       generation = $g                                                         -- when the pod has one
 WHERE id = 1 AND (lease_owner IS NULL OR lease_expires_at < now() OR lease_owner = $me)
   AND (generation IS NULL OR generation <= $g);                               -- acquire
UPDATE oagw_registry_state SET lease_expires_at = now() + 60 * INTERVAL '1 second'
 WHERE id = 1 AND lease_owner = $me;                                           -- renew
UPDATE oagw_registry_state
   SET content_hash = $hash, applied_at = now(),                               -- after a clean run only
       lease_owner = NULL, lease_expires_at = NULL
 WHERE id = 1 AND lease_owner = $me;                                           -- finish

-- Q14: READ COMMITTED on PostgreSQL and MySQL; retried on contention.
-- MySQL locks twice: FORCE INDEX (PRIMARY), then FORCE INDEX
-- (uq_oagw_upstream_tenant_id), the index a route insert's foreign key
-- check share-locks (by primary key alone the two would not conflict and
-- an uncommitted route would be cascaded). The primary key comes first, as
-- in an upstream update or delete, so the writers never deadlock.
-- SQLite takes the database's write lock first instead (a read first would
-- fail its write as busy without waiting): UPDATE oagw_upstream
-- SET enabled = enabled WHERE <the same row>; 0 rows is not found.
SELECT * FROM oagw_upstream
 WHERE id = $1 AND tenant_id = $2 AND managed_by = 'registry' FOR UPDATE;
SELECT * FROM oagw_route    WHERE upstream_id = $1 AND tenant_id = $2 LIMIT 1;  -- a row: in use, stop
DELETE FROM oagw_upstream   WHERE id = $1 AND tenant_id = $2;
```

### Custom plugins (deferred with `oagw_plugin`)

Added with `oagw_plugin`, together with the `plugin_uuid` / `auth_plugin_uuid` indexes they use.

#### Check whether a custom plugin UUID is in use

```sql
SELECT
  (SELECT COUNT(*) FROM oagw_upstream u WHERE u.auth_plugin_uuid = :plugin_uuid) AS used_by_upstream_auth,
  (SELECT COUNT(*) FROM oagw_upstream_plugin up WHERE up.plugin_uuid = :plugin_uuid) AS used_by_upstream_bindings,
  (SELECT COUNT(*) FROM oagw_route_plugin rp WHERE rp.plugin_uuid = :plugin_uuid) AS used_by_route_bindings;
```

#### Mark a plugin eligible for GC when unreferenced

```sql
UPDATE oagw_plugin p
SET gc_eligible_at = :gc_eligible_at
WHERE p.id = :plugin_uuid
  AND p.gc_eligible_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream u WHERE u.auth_plugin_uuid = :plugin_uuid)
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream_plugin up WHERE up.plugin_uuid = :plugin_uuid)
  AND NOT EXISTS (SELECT 1 FROM oagw_route_plugin rp WHERE rp.plugin_uuid = :plugin_uuid);
```

#### Delete plugins past GC TTL (still unreferenced)

```sql
DELETE FROM oagw_plugin p
WHERE p.gc_eligible_at IS NOT NULL
  AND p.gc_eligible_at <= :now
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream u WHERE u.auth_plugin_uuid = p.id)
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream_plugin up WHERE up.plugin_uuid = p.id)
  AND NOT EXISTS (SELECT 1 FROM oagw_route_plugin rp WHERE rp.plugin_uuid = p.id);
```

## Appendix C: Plugin Binding Examples (Illustrative)

### Plugin Reference in API Payloads

Mixed named and UUID-backed plugin references in an upstream configuration:

```json
{
  "upstream": {
    "plugins": {
      "items": [
        "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1",
        "gts.cf.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000"
      ]
    }
  }
}
```

### Plugin Bindings in the Database

Named plugins have `plugin_uuid = NULL`; UUID-backed plugins have both `plugin_ref` and `plugin_uuid` populated (enables in-use checks and index lookups):

```sql
-- oagw_upstream_plugin join table
INSERT INTO oagw_upstream_plugin (upstream_id, tenant_id, position, plugin_ref, plugin_uuid, config) VALUES
  ('upstream-uuid', 'tenant-uuid', 0, 'gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1', NULL, '{"log_level":"debug"}'),
  ('upstream-uuid', 'tenant-uuid', 1, 'gts.cf.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000', '550e8400-e29b-41d4-a716-446655440000', '{"max_body_size":1048576}');
```

## More Information

Deferred / future work:
- Concurrency limiting / backpressure queueing: add nullable JSON config fields (upstream and/or route)
- Circuit breaker: add nullable JSON config fields (upstream)
- Backend-specific indexes (e.g. JSON indexes) may be added for performance, but must not change semantics

- [ADR: Optional Persistence](./0018-optional-persistence.md) — opt-in persistence implementing this schema (except `oagw_plugin`)
- [ADR: Plugin System](./0003-plugin-system.md)
- [ADR: Request Routing](./0002-request-routing.md)
- [ADR: State Management](./0008-state-management.md)
- [ADR: Control Plane Caching](./0007-data-plane-caching.md) (superseded by ADR-0020)
- [ADR: Proxy Configuration Cache](./0020-proxy-config-cache.md)
- [ADR: Rate Limiting](./0004-rate-limiting.md)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-upstream-mgmt` — Upstream table schema and tenant-scoped CRUD
* `cpt-cf-oagw-fr-route-mgmt` — Route table schema with typed match keys
* `cpt-cf-oagw-fr-plugin-system` — Plugin table and binding schemas
* `cpt-cf-oagw-nfr-multi-tenancy` — All tables tenant-scoped via secure ORM layer
* `cpt-cf-oagw-nfr-low-latency` — Indexed hot-path columns for fast lookups
