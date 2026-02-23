# PRD — Simple Resource Registry Relational Database Plugin

## 1. Overview

### 1.1 Purpose

The SRR Relational Database Plugin (`srr-rdb-plugin`) is the default storage backend for the Simple Resource Registry module.

This PRD specifies **only plugin-specific requirements** for the relational database backend. All system-level requirements (API semantics, security model, tenant/owner/GTS access control behavior, payload validation behavior, batch semantics, error model, etc.) are defined in the main module PRD and are **inherited** by this plugin:

- **Parent PRD (authoritative)**: [../../../docs/PRD.md](../../../docs/PRD.md)

The plugin implements the `ResourceStoragePluginClient` trait using a relational database (PostgreSQL, MariaDB, or SQLite) via SecureORM (SeaORM with SecurityContext-based tenant scoping). The plugin stores resources in a single shared table with fixed schema columns and a TEXT payload column containing serialized JSON, providing ACID guarantees, OData query support on schema fields, and idempotent resource creation.

### 1.2 Background / Problem Statement

The Simple Resource Registry requires at least one default storage backend. This plugin provides a DB-agnostic relational implementation that can run on edge devices (SQLite) and production deployments (PostgreSQL/MariaDB) without database-specific configuration.

The plugin is intentionally kept simple: it uses a single shared table for all resource types and relies on B-tree indexes for query performance (see ADR `cpt-cf-srr-rdb-adr-storage-optimization`). More specialized storage optimizations (partitioning, per-type tables) are intentionally left to other backends.

### 1.3 Goals (Business Outcomes)

- Provide a zero-configuration default storage backend that works on PostgreSQL, MariaDB, and SQLite
- Meet the parent module's scalability targets through proper indexing (`cpt-cf-srr-nfr-scalability`)
- Keep the implementation simple and maintainable — no database-specific features (partitioning, materialized views, etc.)

All other business and product goals are defined by the parent Simple Resource Registry PRD.

### 1.4 Glossary

This PRD uses the parent module glossary as the primary source of truth. The terms below are plugin-specific.

| Term | Definition |
|------|------------|
| Schema Fields | The fixed envelope columns (id, tenant_id, owner_id, type, timestamps) stored as dedicated database columns and queryable via OData |
| Payload Column | A TEXT column storing serialized JSON payload as an opaque blob |
| Idempotency Record | A `(tenant_id, idempotency_key)` entry that maps a deduplication key to the resource ID created on the first successful request |
| SecureORM | SeaORM wrapper that injects `SecurityContext`-based tenant scoping into every database query |

## 2. Actors

### 2.1 Human Actors

This plugin has no direct human actors. It is consumed exclusively by the Simple Resource Registry main module.

### 2.2 System Actors

#### Simple Resource Registry Main Module

**ID**: `cpt-cf-srr-rdb-actor-srr-main`

**Role**: The main module invokes this plugin via the `ResourceStoragePluginClient` trait for all resource persistence and query operations. The main module handles authentication, authorization, GTS type resolution, event/audit emission, and request routing — the plugin only handles storage.

## 3. Operational Concept & Environment

The plugin operates within the standard CyberFabric modkit lifecycle. It registers itself as a scoped client in ClientHub with a GTS plugin instance ID, enabling the main module's Storage Router to discover and invoke it. The plugin uses the platform's shared database connection pool managed by modkit-db.

## 4. Scope

### 4.1 In Scope

- Implementation of the full `ResourceStoragePluginClient` trait
- Single shared table (`simple_resources`) for all resource types with fixed schema columns + TEXT payload column (serialized JSON)
- B-tree indexes on schema fields for query performance
- OData $filter/$orderby translation to SeaORM query filters on schema fields
- GTS wildcard type matching via SQL `LIKE` with prefix matching
- Atomic idempotency: resource creation + idempotency key recording in a single database transaction
- Soft-delete via `deleted_at` timestamp
- Immediate hard-delete for resources with `retention_days == 0`
- Batch purge of soft-deleted resources past their retention period
- Resource group memberships via a junction table
- Cursor-based pagination (limit, cursor) on schema fields
- Support for PostgreSQL, MariaDB, and SQLite without database-specific branching

### 4.2 Out of Scope

- Table partitioning by resource type (not DB-agnostic; see ADR)
- Per-resource-type dedicated tables (requires DDL at runtime; see ADR)
- Full-text search within payload content (use a search-capable backend)
- Payload-level indexing or querying
- Caching layers (Redis, in-memory) — caching is a main module concern
- Database migration management beyond initial schema creation

## 5. Functional Requirements

### 5.1 Storage Operations

#### Relational Storage Implementation

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-fr-relational-storage`

The plugin **MUST** implement the `ResourceStoragePluginClient` trait using SecureORM (SeaORM with SecurityContext-based tenant scoping). Schema fields **MUST** be stored as dedicated database columns. The payload **MUST** be stored as a TEXT column containing serialized JSON. The plugin **MUST** declare `odata_support: true` and `search_support: false` in its `PluginCapabilities`.

**Rationale**: Core plugin functionality — implements the storage contract defined by the main module.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

#### OData Query Translation

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-fr-odata-translation`

The plugin **MUST** translate OData $filter and $orderby parameters to SeaORM query conditions on schema fields (id, tenant_id, owner_id, type, created_at, updated_at, deleted_at). The plugin **MUST** support cursor-based pagination (limit, cursor). Invalid OData expressions **MUST** result in an error returned to the main module.

**Rationale**: Enables the main module's OData query support on schema fields.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

#### GTS Wildcard Type Matching

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-fr-gts-wildcard`

The plugin **MUST** support GTS wildcard filtering on the `type` column. A trailing wildcard (`*`) in the type filter **MUST** be translated to a SQL `LIKE` prefix match (e.g., `type LIKE 'gts.x.core.srr.resource.v1~acme.%'`). The wildcard is greedy and matches through the `~` chain separator.

**Rationale**: Enables the main module's GTS wildcard filtering requirement.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

#### Atomic Idempotent Creation

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-fr-idempotency`

The plugin **MUST** atomically check for an existing `(tenant_id, idempotency_key)` record and persist the resource + idempotency record in a single database transaction. If a matching record exists within the retention window (default 24 h), the plugin **MUST** return `CreateOutcome::Duplicate(existing_resource_id)`. If no match exists, the plugin **MUST** insert both the resource row and the idempotency record atomically and return `CreateOutcome::Created(resource)`.

**Rationale**: Ensures safe retry semantics for resource creation without risk of double-creation.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

#### Soft-Delete and Hard-Delete

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-fr-soft-delete`

The plugin **MUST** implement soft-delete by setting `deleted_at = now()` on the resource row. The plugin **MUST** implement immediate hard-delete via `hard_delete()` by permanently removing the resource row. Soft-deleted resources **MUST** be excluded from list queries by default (WHERE `deleted_at IS NULL`).

**Rationale**: Supports the main module's soft-delete + configurable retention model.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

#### Retention Purge

- [ ] `p3` - **ID**: `cpt-cf-srr-rdb-fr-retention-purge`

The plugin **MUST** implement `purge_deleted_before(type, cutoff, batch_size)` to permanently delete resources where `type` matches and `deleted_at < cutoff`, limited to `batch_size` rows per invocation. This operation is called by the main module's retention purge job.

**Rationale**: Enables automatic cleanup of soft-deleted resources past their retention period.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

#### Resource Group Memberships

- [ ] `p3` - **ID**: `cpt-cf-srr-rdb-fr-group-memberships`

The plugin **MUST** store resource group membership data in a dedicated junction table. The plugin **MUST** support listing all resources in a group within a tenant, and listing all groups a resource belongs to.

**Rationale**: Supports the main module's resource groups feature.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

### 5.2 Database Compatibility

#### Multi-Database Support

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-fr-db-agnostic`

The plugin **MUST** work on PostgreSQL, MariaDB, and SQLite without requiring database-specific configuration or conditional SQL.

**Rationale**: Enables the Simple Resource Registry to run on any CyberFabric deployment target.
**Actors**: `cpt-cf-srr-rdb-actor-srr-main`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Scalability

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-nfr-scalability`

The plugin **MUST** meet the parent module's scalability targets: 100 million total resources, 100 write operations per second, and 1000 single-resource fetch-by-ID operations per second. These targets **MUST** be achieved through proper B-tree indexing on schema fields, without requiring table partitioning or database-specific optimizations.

**Threshold**: 100M total resources; 100 writes/s; 1000 reads-by-ID/s
**Rationale**: The default backend must handle the full scalability envelope defined by the parent module.
**Architecture Allocation**: See DESIGN.md section "Indexing Strategy"

#### Single-Resource Read Latency

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-nfr-read-latency`

Single-resource fetch-by-ID **MUST** complete within 50ms at p95 under normal load. This is achieved via primary key lookup with no joins.

**Threshold**: 50ms p95 for single-resource reads
**Rationale**: Inherited from parent module NFR `cpt-cf-srr-nfr-read-latency`.

### 6.2 NFR Exclusions

- **High-volume partitioning**: Not applicable — the plugin intentionally avoids DB-specific partitioning to maintain cross-database compatibility. See ADR for rationale.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### ResourceStoragePluginClient Implementation

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-interface-plugin-impl`

**Type**: Rust trait implementation (async)
**Stability**: stable
**Description**: Implements `ResourceStoragePluginClient` from the `simple-resource-registry-sdk` crate. Registered as a scoped client in ClientHub with a GTS plugin instance ID.
**Breaking Change Policy**: Follows the parent module's plugin interface versioning — trait changes are coordinated via the SDK crate.

## 8. Use Cases

#### Standard CRUD via Main Module

- [ ] `p1` - **ID**: `cpt-cf-srr-rdb-usecase-crud`

**Actor**: `cpt-cf-srr-rdb-actor-srr-main`

**Preconditions**:
- Plugin is registered in ClientHub with its GTS instance ID
- Database schema (tables + indexes) is deployed

**Main Flow**:
1. Main module's Storage Router resolves this plugin for the target resource type
2. Main module calls plugin's `create`/`get`/`list`/`update`/`delete` method with SecurityContext
3. Plugin executes the operation against the relational database via SecureORM
4. Plugin returns the result to the main module

**Postconditions**:
- Resource state is persisted in the database
- All operations are tenant-scoped via SecureORM

## 9. Acceptance Criteria

- [ ] Plugin implements all `ResourceStoragePluginClient` trait methods
- [ ] Plugin declares `odata_support: true`, `search_support: false`
- [ ] CRUD operations work correctly on PostgreSQL, MariaDB, and SQLite
- [ ] OData $filter, $orderby translate correctly to SQL WHERE/ORDER BY on schema fields
- [ ] GTS wildcard type filter translates to SQL LIKE prefix match
- [ ] Idempotent creation: duplicate `idempotency_key` within same tenant returns `CreateOutcome::Duplicate`
- [ ] Idempotency check + resource insert are atomic (single transaction)
- [ ] Soft-delete sets `deleted_at`; soft-deleted resources excluded from list queries
- [ ] `hard_delete()` permanently removes the resource row
- [ ] `purge_deleted_before()` deletes matching rows up to `batch_size`
- [ ] System sustains 100M total resources, 100 writes/s, 1000 reads-by-ID/s with B-tree indexes
- [ ] Single-resource fetch-by-ID completes within 50ms at p95

## 10. Dependencies

| Dependency | Description | Needed By |
|------------|-------------|-----------|
| simple-resource-registry-sdk | Plugin trait definition (`ResourceStoragePluginClient`, `CreateOutcome`, `PluginCapabilities`) | `cpt-cf-srr-rdb-fr-relational-storage` |
| modkit-db (SecureORM) | Tenant-scoped database access via SeaORM + SecurityContext | `cpt-cf-srr-rdb-fr-relational-storage` |
| SeaORM | ORM for database operations, query building, and migrations | `cpt-cf-srr-rdb-fr-odata-translation` |
| modkit (ClientHub) | Scoped client registration for plugin discovery | `cpt-cf-srr-rdb-fr-relational-storage` |

## 11. Assumptions

- The database connection pool is managed by modkit-db and shared with other modules
- Database migrations for this plugin's tables are deployed before the plugin starts
- The main module handles all authentication, authorization, and GTS type resolution before calling the plugin

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Single-table design hits performance ceiling before 100M resources on underpowered hardware | Query latency exceeds NFR thresholds | Proper composite indexes; main module's storage routing can redirect high-volume types to alternative backends |
| SQLite lacks concurrent write support under load | Write throughput may not reach 100 writes/s on SQLite | SQLite is intended for edge/dev environments with lower concurrency; production deployments use PostgreSQL |
| TEXT payload storage limits payload-specific DB optimization | Payload extraction/indexing is not available in this plugin | Payload is opaque by design — no payload-level queries are supported; use a search-capable backend when payload querying is required |

## 13. Open Questions

None — this plugin's requirements are fully derived from the parent module's storage backend contract.

## 14. Traceability

- **Parent Module PRD**: [../../../docs/PRD.md](../../../docs/PRD.md)
- **Parent Module DESIGN**: [../../../docs/DESIGN.md](../../../docs/DESIGN.md)
- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
