---
status: accepted
date: 2026-02-24
---
# ADR-0002: Store Payload as TEXT (Not JSONB)

**ID**: `cpt-cf-srr-rdb-adr-payload-storage-text`

## Context and Problem Statement

The SRR Relational Database Plugin stores an opaque JSON payload for each resource. Relational databases differ in how they represent JSON:

- PostgreSQL provides JSONB with rich operators and indexing.
- MariaDB and SQLite typically store JSON as TEXT and have different (or limited) JSON query capabilities.

The plugin must be portable across PostgreSQL, MariaDB, and SQLite and does not require payload-level querying or indexing (schema fields are queryable; payload is opaque). Should the plugin store payload as PostgreSQL JSONB where available, or store payload uniformly as TEXT?

## Decision Drivers

* Portability across PostgreSQL, MariaDB, and SQLite (`cpt-cf-srr-rdb-fr-db-agnostic`)
* Payload is opaque by contract — no payload-level query language or indexing is required
* Avoid backend-specific SQL/operators that would leak into the plugin interface or behavior
* Keep migration and operational footprint simple

## Considered Options

* Use PostgreSQL JSONB and TEXT elsewhere (mixed behavior)
* Store payload uniformly as TEXT for all supported databases
* Introduce a dedicated search/index backend for payload queries (out of scope for this plugin)

## Decision Outcome

Chosen option: "Store payload uniformly as TEXT", because it maximizes portability and simplicity, and the SRR contract does not require payload querying or indexing.

This plugin treats the payload as an opaque blob. If payload-level querying or full-text search is required for a resource type, that type should be routed to a search-capable backend.

### Consequences

* Good, because fully portable across PostgreSQL, MariaDB, and SQLite
* Good, because avoids relying on JSONB-specific operators and indexes
* Good, because schema fields remain the primary query surface (OData on schema fields)
* Bad, because PostgreSQL JSONB indexing advantages are intentionally not used
* Bad, because any future payload-query feature would require a different backend or a new plugin version

### Confirmation

* Plugin can run unchanged on PostgreSQL, MariaDB, and SQLite
* Integration tests confirm payload round-trips without modification
* No payload-level filtering, ordering, or indexing is implemented in this plugin

## Pros and Cons of the Options

### Use PostgreSQL JSONB and TEXT elsewhere (mixed behavior)

* Good, because can leverage JSONB operators and indexes on PostgreSQL
* Bad, because behavior diverges by database backend
* Bad, because encourages introducing payload-query features that are not portable

### Store payload uniformly as TEXT for all supported databases

* Good, because consistent behavior across all supported databases
* Good, because simplest schema and ORM mapping
* Bad, because does not leverage JSONB indexing on PostgreSQL

### Introduce a dedicated search/index backend for payload queries

* Good, because supports payload-level query/search without coupling to relational DB specifics
* Bad, because out of scope for this plugin; requires a separate backend and routing

## Traceability

- **Plugin PRD**: [../PRD.md](../PRD.md)
- **Plugin DESIGN**: [../DESIGN.md](../DESIGN.md)
- **Parent PRD**: [../../../docs/PRD.md](../../../docs/PRD.md)

This decision directly relates to:

* `cpt-cf-srr-rdb-fr-db-agnostic` — DB-agnostic behavior across PostgreSQL/MariaDB/SQLite
* `cpt-cf-srr-constraint-no-payload-query` — payload remains opaque; no payload query language required
* `cpt-cf-srr-fr-search-api` — payload search is handled by search-capable backends
