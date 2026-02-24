# ADR: Storage Schema

- **Status**: Proposed
- **Date**: 2026-02-23
- **Deciders**: OAGW Team

## Context and Problem Statement

OAGW persists configuration for upstreams, routes, and plugins. This data is read frequently (proxy hot path) and written infrequently (management operations).

OAGW must support multiple SQL backends via `modkit-db` (PostgreSQL, MySQL, SQLite). The schema must be portable and preserve consistent behavior and security guarantees across backends.

## Decision Drivers

- **Tenant isolation**: all reads/writes must be tenant-scoped via the secure ORM layer.
- **Tenant hierarchy behavior**: alias resolution and effective configuration must support shadowing and inheritance semantics.
- **Hot-path lookups**:
  - Resolve upstream by `(tenant hierarchy, alias)`.
  - Match HTTP routes by `(upstream_id, method, longest path prefix, priority)`.
  - Match gRPC routes by `(upstream_id, service, method, priority)`.
- **Deletion semantics**:
  - Deleting an upstream must delete its routes and dependent match/binding rows.
- **Plugin requirements**:
  - Ordered plugin chains with per-binding config.
  - Plugin references must support both builtin named IDs and custom UUID-backed IDs.
  - Custom plugin lifecycle requires “in use” detection and GC eligibility timestamps.
- **Portability**: avoid correctness depending on backend-specific features (e.g. JSON operators, partial indexes).

## Considered Options

### Option 1: Portable relational baseline + JSON blobs (Chosen)

Use a small set of relational tables for hot-path fields and relationships (indexes, join tables) and store larger, evolving configuration as JSON text.

### Option 2: PostgreSQL-first schema

Rely heavily on PostgreSQL-native features (UUID defaults, JSONB operators, specialized indexes). This optimizes early Postgres performance but increases divergence risk across backends.

### Option 3: Fully normalized configuration (no JSON)

Model all configuration in relational tables. This increases DB-level validation/queryability but adds significant schema complexity and migration overhead.

## Decision

Adopt **Option 1**: a **portable relational baseline** with JSON blobs for evolving configuration.

### Non-negotiable invariants

- All reads and writes are tenant-scoped through the secure data access layer (parameter binding + tenant scoping).
- Multi-table configuration updates are applied atomically (single transaction per logical write).
- Alias resolution and effective configuration merges preserve tenant-hierarchy semantics.
- Route matching uses typed match key tables (no inference from opaque JSON).
- Multi-value associations that affect selection/filtering (methods, tags, plugin bindings) are stored in join tables.
- Plugin bindings preserve explicit ordering and per-binding config.
- Plugin identifiers in bindings support:
  - builtin named IDs (resolved from the built-in registry)
  - custom plugins (resolved by UUID in `oagw_plugin`)

Named plugins are not persisted as rows:

- `oagw_plugin` stores custom (UUID-backed) plugins only.
- Named plugins are referenced in bindings via `plugin_ref` with `plugin_uuid = NULL`.
- The binding tables do not have an FK to `oagw_plugin`.

## Schema (Logical)

This section describes table responsibilities, relationships, and key constraints.

See **Appendix A** for the full column-by-column schema summary (including indexes).

### Entities and relationships

- `oagw_upstream` is the tenant-scoped root configuration object (unique per `(tenant_id, alias)`).
- `oagw_route` belongs to `oagw_upstream`.
- `oagw_route_http_match` / `oagw_route_grpc_match` store typed match keys for deterministic route selection.
- `oagw_route_method` stores HTTP method allowlists.
- `oagw_upstream_tag` / `oagw_route_tag` store tags.
- `oagw_plugin` stores **custom (UUID-backed) plugins only**.
- `oagw_upstream_plugin` / `oagw_route_plugin` store ordered plugin bindings.

### Cascading delete semantics (FK constraints)

- Deleting an upstream deletes its routes.
- Deleting an upstream or route deletes dependent tag, match, method, and plugin-binding rows.

### Secure ORM scoping requirements

- Some dependent tables in this schema do not carry a `tenant_id` column (e.g. tags, methods, match keys, plugin bindings).
- When accessing these tables, the implementation must apply tenant scoping via the secure ORM layer using scoped joins and/or `EXISTS`-based scoping against `oagw_upstream` / `oagw_route`.
- Direct, unscoped reads/writes of dependent tables are forbidden.

### Plugin reference semantics

- `plugin_ref` is always stored in bindings.
- `plugin_uuid` is stored only for UUID-backed plugins (custom); it is NULL for named plugins.
- Binding tables do not have an FK to `oagw_plugin`.
- `auth_plugin_ref` / `auth_plugin_uuid` are stored as scalar columns on `oagw_upstream` to support efficient “in use” checks.

### Application-level validation rules

- `plugin_ref` must be non-empty and canonicalized (trimmed).
- For HTTP routes, `path_prefix` must be normalized and must not exceed a fixed maximum number of path segments.
- If `plugin_uuid` is set on a binding row:
  - `plugin_uuid` must be a valid UUID.
  - `plugin_ref` must represent the same UUID (exact format is an application concern; comparisons must be done on the parsed UUID).
  - The referenced row must exist in `oagw_plugin` within scope.
- If `plugin_uuid` is NULL on a binding row:
  - `plugin_ref` must resolve to a builtin plugin in the registry.
- `auth_plugin_ref/auth_plugin_uuid` (when present) must resolve to an **auth** plugin.
- `oagw_upstream_plugin` / `oagw_route_plugin` bindings must not reference auth plugins (auth is configured only via `auth_plugin_*`).
- Binding ordering:
  - `position` is unique per `(upstream_id)` / `(route_id)` by PK.
  - Positions must start at 0 and be contiguous (no gaps). This is validated on write.

### Route selection determinism

The route matching queries in Appendix B use `created_at` as the final ordering term.

To preserve deterministic selection semantics across backends and timestamp precisions, the control plane must reject ambiguous route configurations on write (before enabling or updating):

- For HTTP routes, for each method bound to the route, there must not exist another enabled HTTP route under the same upstream with the same `path_prefix` and `priority`.
- For gRPC routes, there must not exist another enabled gRPC route under the same upstream with the same `(service, method)` and `priority`.

## Appendix A: Schema Tables (Illustrative)

The tables below are a compact summary of the logical schema above.

Notes:

- Types are logical. Physical types may differ across backends.
- “JSON text” is stored as a backend-appropriate text type and treated as opaque by the DB.
- Timestamp columns must be stored in an orderable, comparable format (backend timestamp type or epoch milliseconds). All timestamps are UTC.

### `oagw_upstream`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | UUID | No | PK |
| `tenant_id` | UUID | No | Tenant scope |
| `alias` | TEXT | No | Unique per tenant |
| `protocol` | TEXT | No | GTS protocol identifier |
| `enabled` | BOOL | No | |
| `auth_sharing` | TEXT | No | `private|inherit|enforce` |
| `rate_limit_sharing` | TEXT | No | `private|inherit|enforce` |
| `plugins_sharing` | TEXT | No | `private|inherit|enforce` |
| `schema_version` | INT | No | JSON schema version for JSON text columns in this table |
| `server` | JSON text | No | Endpoints + protocol config |
| `auth_plugin_ref` | TEXT | Yes | Canonical plugin identifier |
| `auth_plugin_uuid` | UUID | Yes | Parsed UUID when custom |
| `auth_config` | JSON text | Yes | Config only (no plugin id) |
| `headers` | JSON text | Yes | |
| `cors` | JSON text | Yes | |
| `rate_limit` | JSON text | Yes | |
| `created_at` | TIMESTAMP | No | |
| `updated_at` | TIMESTAMP | No | |

Constraints / indexes:

- Unique: `(tenant_id, alias)`
- Index: `(alias, tenant_id)`
- Index: `(auth_plugin_uuid)`

### `oagw_upstream_tag`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `upstream_id` | UUID | No | PK part, FK (cascade) |
| `tag` | TEXT | No | PK part |

Indexes:

- `(tag, upstream_id)`

### `oagw_route`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | UUID | No | PK |
| `tenant_id` | UUID | No | Tenant scope |
| `upstream_id` | UUID | No | FK (cascade) |
| `enabled` | BOOL | No | |
| `priority` | INT | No | Higher wins after specificity |
| `match_type` | TEXT | No | `http|grpc` |
| `schema_version` | INT | No | JSON schema version for JSON text columns in this table |
| `match_config` | JSON text | Yes | Query allowlist, suffix mode, etc. |
| `cors` | JSON text | Yes | |
| `rate_limit` | JSON text | Yes | |
| `rate_limit_sharing` | TEXT | No | `private|inherit|enforce` |
| `plugins_sharing` | TEXT | No | `private|inherit|enforce` |
| `created_at` | TIMESTAMP | No | |
| `updated_at` | TIMESTAMP | No | |

Indexes:

- `(upstream_id, enabled, match_type, priority)`

### `oagw_route_http_match`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK, FK (cascade) |
| `path_prefix` | TEXT | No | |

Indexes:

- `(path_prefix, route_id)`

### `oagw_route_method`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK part, FK (cascade) |
| `method` | TEXT | No | PK part |

Indexes:

- `(method, route_id)`

### `oagw_route_grpc_match`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK, FK (cascade) |
| `service` | TEXT | No | |
| `method` | TEXT | No | |

Indexes:

- `(service, method, route_id)`

### `oagw_route_tag`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK part, FK (cascade) |
| `tag` | TEXT | No | PK part |

Indexes:

- `(tag, route_id)`

### `oagw_plugin` (custom plugins)

| Column | Type | Null | Notes |
|---|---|---:|---|
| `id` | UUID | No | PK |
| `tenant_id` | UUID | No | Tenant scope |
| `plugin_type` | TEXT | No | `auth|guard|transform` |
| `name` | TEXT | No | Unique per tenant |
| `description` | TEXT | Yes | |
| `schema_version` | INT | No | JSON schema version for JSON text columns in this table |
| `config_schema` | JSON text | No | |
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
| `upstream_id` | UUID | No | PK part, FK (cascade) |
| `position` | INT | No | PK part |
| `plugin_ref` | TEXT | No | Canonical plugin identifier |
| `plugin_uuid` | UUID | Yes | Parsed UUID when custom |
| `schema_version` | INT | No | JSON schema version for JSON text columns in this table |
| `config` | JSON text | Yes | |

Indexes:

- `(plugin_uuid, upstream_id)`

### `oagw_route_plugin`

| Column | Type | Null | Notes |
|---|---|---:|---|
| `route_id` | UUID | No | PK part, FK (cascade) |
| `position` | INT | No | PK part |
| `plugin_ref` | TEXT | No | Canonical plugin identifier |
| `plugin_uuid` | UUID | Yes | Parsed UUID when custom |
| `schema_version` | INT | No | JSON schema version for JSON text columns in this table |
| `config` | JSON text | Yes | |

Indexes:

- `(plugin_uuid, route_id)`

## Appendix B: Example Queries (Illustrative)

Notes:

- These queries are illustrative for reasoning about indexing and constraints.
- OAGW implementation must use the secure ORM layer (no raw SQL in module code).
- `:param` denotes a bound parameter. Lists must be expanded safely by the query builder.

### Resolve upstream by alias across a tenant hierarchy

Assumes the application provides the visible tenant IDs in precedence order (child first): `:t0, :t1, ...`.

```sql
SELECT u.*
FROM oagw_upstream u
WHERE u.alias = :alias
  AND u.tenant_id IN (:t0, :t1, :t2)
ORDER BY CASE u.tenant_id
  WHEN :t0 THEN 0
  WHEN :t1 THEN 1
  WHEN :t2 THEN 2
  ELSE 999
END
LIMIT 1;
```

### Match HTTP route by (method, longest path prefix, priority)

This query assumes the application precomputes a bounded list of candidate prefixes for the request path (longest first), e.g.
`/a/b/c` -> [`/a/b/c`, `/a/b`, `/a`, `/`]. This allows `hm.path_prefix` to use an index and avoids relying on portable-but-hard-to-index substring predicates.

```sql
SELECT r.*
FROM oagw_route r
JOIN oagw_route_http_match hm ON hm.route_id = r.id
JOIN oagw_route_method rm ON rm.route_id = r.id
WHERE r.upstream_id = :upstream_id
  AND r.enabled = :enabled
  AND r.match_type = 'http'
  AND rm.method = :method
  AND hm.path_prefix IN (:p0, :p1, :p2, :p3)
ORDER BY LENGTH(hm.path_prefix) DESC,
         r.priority DESC,
         r.created_at ASC
LIMIT 1;
```

### Match gRPC route by (service, method, priority)

```sql
SELECT r.*
FROM oagw_route r
JOIN oagw_route_grpc_match gm ON gm.route_id = r.id
WHERE r.upstream_id = :upstream_id
  AND r.enabled = :enabled
  AND r.match_type = 'grpc'
  AND gm.service = :service
  AND gm.method = :method
ORDER BY r.priority DESC
       , r.created_at ASC
LIMIT 1;
```

### Check whether a custom plugin UUID is in use

```sql
SELECT
  (SELECT COUNT(*) FROM oagw_upstream u WHERE u.auth_plugin_uuid = :plugin_uuid) AS used_by_upstream_auth,
  (SELECT COUNT(*) FROM oagw_upstream_plugin up WHERE up.plugin_uuid = :plugin_uuid) AS used_by_upstream_bindings,
  (SELECT COUNT(*) FROM oagw_route_plugin rp WHERE rp.plugin_uuid = :plugin_uuid) AS used_by_route_bindings;
```

### Mark a plugin eligible for GC when unreferenced

```sql
UPDATE oagw_plugin p
SET gc_eligible_at = :gc_eligible_at
WHERE p.id = :plugin_uuid
  AND p.gc_eligible_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream u WHERE u.auth_plugin_uuid = :plugin_uuid)
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream_plugin up WHERE up.plugin_uuid = :plugin_uuid)
  AND NOT EXISTS (SELECT 1 FROM oagw_route_plugin rp WHERE rp.plugin_uuid = :plugin_uuid);
```

### Delete plugins past GC TTL (still unreferenced)

```sql
DELETE FROM oagw_plugin p
WHERE p.gc_eligible_at IS NOT NULL
  AND p.gc_eligible_at <= :now
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream u WHERE u.auth_plugin_uuid = p.id)
  AND NOT EXISTS (SELECT 1 FROM oagw_upstream_plugin up WHERE up.plugin_uuid = p.id)
  AND NOT EXISTS (SELECT 1 FROM oagw_route_plugin rp WHERE rp.plugin_uuid = p.id);
```

## Rationale

- Keeping hot-path selectors in scalar columns / join tables enables efficient, portable queries.
- Separating route match keys into typed tables enables deterministic route selection without parsing JSON.
- Storing plugin references as canonical strings (`plugin_ref`) supports builtin named IDs while still allowing efficient lookups for custom plugins via `plugin_uuid`.
- Storing `auth_plugin_ref/auth_plugin_uuid` as scalar columns avoids correctness and “in use” checks depending on backend-specific JSON querying.

## Consequences

### Positive

- Portable schema across PostgreSQL/MySQL/SQLite.
- Efficient selection for the scenarios that are on the proxy hot path.
- Supports plugin scenarios:
  - builtin named IDs
  - custom UUID plugins
  - delete-in-use detection
  - GC eligibility timestamps

### Negative

- Some referential integrity is enforced in application code (e.g., builtin plugins are not FK-backed).
- Requires application-level validation to ensure `plugin_ref` is well-formed and that `plugin_uuid` matches it when present.

## Deferred / Future Work

- Concurrency limiting / backpressure queueing: add nullable JSON config fields (upstream and/or route).
- Circuit breaker: add nullable JSON config fields (upstream).
- Backend-specific indexes (e.g. JSON indexes) may be added for performance, but must not change semantics.

## Related ADRs

- [ADR: Plugin System](./adr-plugin-system.md)
- [ADR: Request Routing](./adr-request-routing.md)
- [ADR: State Management](./adr-state-management.md)
- [ADR: Control Plane Caching](./adr-data-plane-caching.md)
- [ADR: Rate Limiting](./adr-rate-limiting.md)
