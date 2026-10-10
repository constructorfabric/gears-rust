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

### Non-Negotiable Invariants

- All reads and writes are tenant-scoped through the secure data access layer (parameter binding + tenant scoping), except the startup registry reconcile's key listing, which reads across tenants ([ADR: Optional Persistence](./0018-optional-persistence.md))
- Every table carries `tenant_id`, including child tables; a child row's `tenant_id` always equals its parent's
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

- Every table, including tags, methods, match keys, and plugin bindings, carries a `tenant_id` column and declares it as the secure ORM tenant column.
- All tables are accessed with the same tenant scope (`AccessScope::for_tenants`). Every table carries its own tenant column; a joined child row is confined by the composite join key `(tenant_id, parent_id)` to its scoped parent's tenant, not by a separate predicate or `EXISTS`.
- On write, a child row takes its `tenant_id` from its parent. The composite foreign keys above enforce this at the database level.
- Direct, unscoped reads/writes of any table are forbidden, except the registry reconcile's key listing above.

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

To preserve deterministic selection semantics across backends and timestamp precisions, the control plane must reject ambiguous route configurations on write (before enabling or updating):

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
- Index: `(managed_by)` (the startup registry reconcile lists registry rows across tenants)
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
- Index: `(managed_by)` (the startup registry reconcile lists registry rows across tenants)

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

## Appendix B: Queries and Access Frequency

The queries the database repositories run, as SQL. The secure ORM adds the `tenant_id` predicate. Exact DDL: [`migration.sql`](../migration.sql).

Frequency:

- **per request**: every proxied request. Config is not cached; Q1–Q3 run on each one, independent of tenant depth. The proxy never reads tags and does not load them.
- **per call**: Management API / SDK calls.
- **per boot**: registry reconcile.

| # | Query | Frequency | Index |
|---|---|---|---|
| Q1 | Upstreams by alias in the tenant chain, with their plugins | per request | unique `(tenant_id, alias)`; child PK |
| Q2 | Enabled HTTP route candidates that allow the request method: selection columns only | per request | `(tenant_id, upstream_id)`; child PKs |
| Q3 | The winning route in full, with its methods and plugins | per request | PK; child PKs |
| Q4 | Tags of the Q1 upstreams and the Q3 route (SDK `resolve_proxy_target` only) | per call | child PK |
| Q5 | Get / update / delete by ID | per call | PK |
| Q6 | List upstreams | per call | unique `(tenant_id, id)` |
| Q7 | List routes (all, or of one upstream); route conflict check | per call | unique `(tenant_id, id)` / `(tenant_id, upstream_id)` |
| Q8 | Q1 for ancestors (create/update) or descendants (rate-limit budget); no tags | per call | unique `(tenant_id, alias)` |
| Q9 | Children of read or written rows; replace on update | per call | child PK |
| Q10 | Registry rows across tenants | per boot | `(managed_by)` |

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
SELECT r.*, m.method, p.*
FROM oagw_route r
JOIN oagw_route_method m      ON m.tenant_id = r.tenant_id AND m.route_id = r.id
LEFT JOIN oagw_route_plugin p ON p.tenant_id = r.tenant_id AND p.route_id = r.id
WHERE r.id = $1 AND r.enabled AND r.match_type = 'http' AND r.tenant_id = $2;

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

-- Q10
SELECT * FROM oagw_upstream WHERE managed_by = 'registry';
SELECT * FROM oagw_route    WHERE managed_by = 'registry';
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
- [ADR: Control Plane Caching](./0007-data-plane-caching.md)
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
