---
status: accepted
date: 2026-02-24
---
# ADR-0001: Single Table with B-Tree Indexes for Resource Storage

**ID**: `cpt-cf-srr-rdb-adr-storage-optimization`

## Context and Problem Statement

The SRR Relational Database Plugin must store up to 100M resources across potentially thousands of GTS resource types in a single relational database, while sustaining 100 writes/s and 1000 reads-by-ID/s. The plugin must work on PostgreSQL, MariaDB, and SQLite without database-specific configuration. How should the physical storage be organized to meet these performance targets while maintaining cross-database compatibility and operational simplicity?

## Decision Drivers

* Must sustain 100M total resources with 100 writes/s and 1000 reads-by-ID/s (`cpt-cf-srr-rdb-nfr-scalability`)
* Must work on PostgreSQL, MariaDB, and SQLite without database-specific features (`cpt-cf-srr-rdb-fr-db-agnostic`)
* Must not require DDL operations at runtime (no table creation when new resource types are registered)
* Plugin must remain simple and maintainable — complexity belongs in specialized backends, not in the default plugin
* Other storage backends (via main module's storage routing) can handle workloads that exceed this plugin's capacity

## Considered Options

* Single table with B-tree indexes
* Per-resource-type dedicated tables
* Table partitioning by resource type

## Decision Outcome

Chosen option: "Single table with B-tree indexes", because 100M rows is well within the capacity of B-tree indexes on modern relational databases, the approach is fully portable across PostgreSQL/MariaDB/SQLite, it requires no runtime DDL, and it keeps the plugin implementation simple. Workloads that exceed 100M resources or require specialized storage features are served by alternative backends via the main module's per-resource-type storage routing.

### Consequences

* Good, because fully portable — identical behavior on PostgreSQL, MariaDB, and SQLite
* Good, because no runtime DDL — all tables and indexes are created during migration
* Good, because simple implementation — one SeaORM entity, one set of indexes, no type-aware branching
* Good, because well-understood performance model — B-tree index lookups are O(log n), predictable at 100M rows
* Good, because the main module's storage routing provides an escape hatch for types that outgrow this plugin
* Bad, because all resource types share index space — a type with millions of resources affects index size for all types
* Bad, because no index locality by type — queries for a specific type scan a shared B-tree rather than a type-specific structure
* Bad, because 100M row ceiling is a hard constraint — beyond this, alternative backends are required

### Confirmation

* Load test with 100M synthetic resources verifies read-by-ID < 50ms p95 and sustained 1000 reads/s
* Load test verifies 100 writes/s sustained with composite indexes
* Integration tests pass identically on PostgreSQL, MariaDB, and SQLite
* No DDL statements executed after initial migration

## Pros and Cons of the Options

### Single table with B-tree indexes

All resource types stored in one `simple_resources` table. Composite B-tree indexes on `(tenant_id, type)`, `(tenant_id, type, created_at)`, etc. provide query performance.

* Good, because fully portable across PostgreSQL, MariaDB, and SQLite — B-tree indexes are universal
* Good, because no runtime DDL — table and indexes created once during migration
* Good, because simplest implementation — one entity, one table, standard ORM patterns
* Good, because 100M rows is well within B-tree capacity — PostgreSQL routinely handles billions of rows with proper indexes
* Good, because composite indexes `(tenant_id, type)` effectively create a "virtual partition" per tenant+type combination
* Good, because partial indexes (e.g., `WHERE deleted_at IS NOT NULL`) keep index sizes small for specialized queries
* Neutral, because index maintenance adds overhead to writes — acceptable at 100 writes/s
* Bad, because shared index space — a disproportionately large resource type inflates the shared index
* Bad, because no physical data locality by type — related rows may be scattered across disk pages
* Bad, because of limited future scalability — this approach does not scale well beyond 100M resources and would require another plugin to be implemented

### Per-resource-type dedicated tables

A new database table is created for each GTS resource type when the type is first used. Each table has its own indexes.

* Good, because physical data isolation per type — indexes are type-specific and smaller
* Good, because per-type index locality — queries only scan the relevant table
* Good, because per-type vacuuming and maintenance — no cross-type interference
* Bad, because requires DDL at runtime — `CREATE TABLE` when a new resource type is first used
* Bad, because DDL at runtime is risky — schema migrations in production, lock contention, potential failures
* Bad, because not portable — DDL behavior and syntax varies across PostgreSQL, MariaDB, and SQLite
* Bad, because operational complexity — thousands of tables to monitor, migrate, and maintain
* Bad, because SeaORM entity model assumes static table names — dynamic table routing adds significant complexity
* Bad, because connection pool contention during DDL on some databases (especially SQLite)

### Table partitioning by resource type

PostgreSQL-native `PARTITION BY LIST (type)` or `PARTITION BY HASH (type)` to split the single table into type-specific partitions managed by the database engine.

* Good, because physical data isolation per type without application-level routing
* Good, because transparent to queries — the database query planner routes to the correct partition
* Good, because per-partition indexes are smaller and more cache-friendly
* Good, because partition pruning eliminates irrelevant partitions from query plans
* Bad, because not portable — SQLite has no partitioning support; MariaDB partitioning has significant limitations (no foreign keys on partitioned tables, limited index support)
* Bad, because requires runtime DDL to create new partitions when new resource types appear
* Bad, because partition management complexity — need to handle partition creation, merging, and cleanup
* Bad, because the number of partitions can grow unbounded (one per resource type) — some databases degrade with thousands of partitions
* Bad, because SeaORM does not have native partitioning support — would require raw SQL or custom migration logic
* Bad, because violates the plugin's DB-agnostic constraint (`cpt-cf-srr-rdb-constraint-no-db-specific`)

## More Information

### Why 100M rows is "small" for B-tree indexes

A B-tree index with 100M entries has a depth of approximately 4-5 levels (log base ~500 of 100M ≈ 3-4, plus root). Each level requires one disk I/O in the worst case, but the upper levels are almost always cached in the database buffer pool. In practice, a PK lookup on a 100M-row table completes in 1-3 disk I/Os, well within the 50ms p95 latency target.

The composite index `(tenant_id, type)` effectively creates a "virtual partition" — queries that filter on both fields traverse only the relevant portion of the index, making the effective index size proportional to the number of resources of that type within the tenant, not the total table size.

### Escape hatch: storage routing

The Simple Resource Registry's per-resource-type storage routing (`cpt-cf-srr-fr-storage-routing`) provides a clean migration path for resource types that outgrow this plugin:

1. Deploy an alternative backend (e.g., a partitioned PostgreSQL plugin, an ElasticSearch plugin, or a vendor-provided store)
2. Update the routing configuration to direct the high-volume type to the new backend
3. Migrate existing resources (if needed) — the main module's API is unchanged

This means the default plugin does not need to solve every scalability problem — it needs to be good enough for the common case, with a clear path to specialization.

## Traceability

- **Plugin PRD**: [../PRD.md](../PRD.md)
- **Plugin DESIGN**: [../DESIGN.md](../DESIGN.md)
- **Parent Module PRD**: [../../../../simple-resource-registry/docs/PRD.md](../../../../simple-resource-registry/docs/PRD.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-srr-rdb-nfr-scalability` — 100M resources / 100 writes·s⁻¹ / 1000 reads-by-ID·s⁻¹ achieved through B-tree indexes
* `cpt-cf-srr-rdb-fr-db-agnostic` — Single table with standard indexes works on PostgreSQL, MariaDB, and SQLite
* `cpt-cf-srr-rdb-constraint-no-db-specific` — No partitioning or database-specific features used
* `cpt-cf-srr-rdb-constraint-no-runtime-ddl` — No DDL at runtime; all tables created during migration
* `cpt-cf-srr-rdb-principle-single-table` — Establishes and justifies the single-table storage model
* `cpt-cf-srr-rdb-principle-index-performance` — Establishes B-tree indexes as the sole performance mechanism
