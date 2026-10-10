# Feature: Core Domain & Storage Foundation


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Gear Bootstrap Flow](#gear-bootstrap-flow)
  - [GTS Type Provisioning Flow](#gts-type-provisioning-flow)
  - [Registry Upstream and Route Provisioning Flow](#registry-upstream-and-route-provisioning-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Storage Selection](#storage-selection)
  - [Database Migration Execution](#database-migration-execution)
  - [Domain Model Validation](#domain-model-validation)
  - [SDK Trait Definition](#sdk-trait-definition)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Domain Entities](#implement-domain-entities)
  - [Implement Database Schema & Migrations](#implement-database-schema--migrations)
  - [Implement SeaORM Entities](#implement-seaorm-entities)
  - [Implement Storage Selection](#implement-storage-selection)
  - [Implement Repository Conformance Tests](#implement-repository-conformance-tests)
  - [Implement Idempotent Registry Provisioning](#implement-idempotent-registry-provisioning)
  - [Implement SDK Crate](#implement-sdk-crate)
  - [Implement ToolKit Gear Wiring](#implement-toolkit-gear-wiring)
  - [Implement GTS Type Provisioning](#implement-gts-type-provisioning)
  - [Implement DDD-Light Layering](#implement-ddd-light-layering)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-oagw-featstatus-domain-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-oagw-feature-domain-foundation`

## 1. Feature Context

### 1.1 Overview

Establish domain model entities, database schema, ToolKit gear wiring, SDK crate, and GTS type provisioning for the OAGW gear.

### 1.2 Purpose

Foundation layer that all other OAGW features depend on. Provides the shared domain entities (Upstream, Route, Plugin, ServerConfig, Endpoint), opt-in upstream and route storage (database tables with migrations when a database is configured, in-memory otherwise), the `oagw-sdk` public crate (`ServiceGatewayClientV1` trait, SDK models, errors), ToolKit gear wiring, and GTS type registration.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Operates the gear; configuration managed via this foundation |
| `cpt-cf-oagw-actor-types-registry` | Receives GTS schema/instance registrations during type provisioning |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Requirements**: `cpt-cf-oagw-nfr-multi-tenancy`, `cpt-cf-oagw-fr-config-persistence`
- **Design elements**: `cpt-cf-oagw-design-domain-model`, `cpt-cf-oagw-db-schema`, `cpt-cf-oagw-design-layers`, `cpt-cf-oagw-design-dependencies`, `cpt-cf-oagw-design-drivers`, `cpt-cf-oagw-design-overview`
- **Principles**: `cpt-cf-oagw-principle-tenant-scope`
- **Constraints**: `cpt-cf-oagw-constraint-toolkit-deploy`, `cpt-cf-oagw-constraint-multi-sql`
- **ADRs**: `cpt-cf-oagw-adr-storage-schema`, `cpt-cf-oagw-adr-optional-persistence`
- **Dependencies**: None

### 1.5 Out of Scope

- CRUD handler implementations for upstreams, routes, and plugins (Feature 2: Management API)
- Plugin trait implementations and execution engine (Feature 3: Plugin System)
- Proxy request routing and execution logic (Feature 4: Proxy Engine)
- SSE event streaming (Feature 5: Real-Time Events)

## 2. Actor Flows (CDSL)

### Gear Bootstrap Flow

- [x] `p1` - **ID**: `cpt-cf-oagw-flow-domain-gear-bootstrap`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Success Scenarios**:
- Gear starts with a configured database: migrations run, database repositories selected, GTS types provisioned, local client registered, gear ready to serve
- Gear starts without a configured database: in-memory repositories selected with a warning, GTS types provisioned, local client registered, gear ready to serve

**Error Scenarios**:
- Database configured but unusable (connection error, backend not compiled in, malformed configuration, migration failure)
- GTS registration failure (types_registry unavailable)

**Steps**:
1. [x] - `p1` - Platform operator starts cf-gears-server with OAGW gear enabled - `inst-boot-1`
2. [x] - `p1` - Runtime DB phase (before `init`): **IF** `gears.oagw.database` is configured, RUN OAGW migrations from `DatabaseCapability::migrations()` - `inst-boot-4`
3. [x] - `p1` - **IF** database connection or migration fails - `inst-boot-5`
   1. [x] - `p1` - Runtime logs error and **RETURN** startup failure - `inst-boot-5a`
4. [x] - `p1` - ToolKit invokes OAGW `Gear::init()` with application context - `inst-boot-2`
5. [x] - `p1` - Load `OagwConfig` from configuration file (fields defined in `cpt-cf-oagw-design-overview`) - `inst-boot-3`
6. [x] - `p1` - Select repositories via `cpt-cf-oagw-algo-domain-storage-selection` - `inst-boot-storage`
7. [x] - `p1` - **IF** storage selection fails - `inst-boot-storage-fail`
   1. [x] - `p1` - Log error and **RETURN** gear initialization failure - `inst-boot-storage-fail-a`
8. [x] - `p1` - Call GTS type provisioning: register all OAGW schemas and built-in instances - `inst-boot-6`
9. [x] - `p1` - **IF** GTS registration fails - `inst-boot-7`
   1. [x] - `p1` - Log error and **RETURN** gear initialization failure - `inst-boot-7a`
10. [x] - `p1` - Register `ServiceGatewayClientV1` local client in ClientHub - `inst-boot-8`
11. [x] - `p1` - Register REST API routes via OperationBuilder - `inst-boot-9`
12. [x] - `p1` - **RETURN** gear initialized successfully - `inst-boot-10`

### GTS Type Provisioning Flow

- [x] `p1` - **ID**: `cpt-cf-oagw-flow-domain-gts-provisioning`

**Actor**: `cpt-cf-oagw-actor-types-registry`

**Success Scenarios**:
- All OAGW GTS schemas registered
- Built-in plugin instances registered

**Error Scenarios**:
- types_registry unavailable
- Schema conflicts with existing registrations

**Steps**:
1. [x] - `p1` - OAGW gear calls types_registry client to register schemas - `inst-gts-1`
2. [x] - `p1` - Register upstream schema: `gts.cf.core.oagw.upstream.v1~` - `inst-gts-2`
3. [x] - `p1` - Register route schema: `gts.cf.core.oagw.route.v1~` - `inst-gts-3`
4. [x] - `p1` - Register auth plugin schema: `gts.cf.core.oagw.auth_plugin.v1~` - `inst-gts-4`
5. [x] - `p1` - Register guard plugin schema: `gts.cf.core.oagw.guard_plugin.v1~` - `inst-gts-5`
6. [x] - `p1` - Register transform plugin schema: `gts.cf.core.oagw.transform_plugin.v1~` - `inst-gts-6`
7. [x] - `p1` - Register error type schema: `gts.cf.core.errors.err.v1~` (OAGW error instances) - `inst-gts-7`
8. [x] - `p1` - Register proxy permission: `gts.cf.core.oagw.proxy.v1~` - `inst-gts-8`
9. [x] - `p1` - **FOR EACH** built-in plugin in auth/guard/transform registries - `inst-gts-9`
   1. [x] - `p1` - Register instance with full GTS identifier - `inst-gts-9a`
10. [x] - `p1` - **IF** any registration fails - `inst-gts-10`
    1. [x] - `p1` - **RETURN** provisioning error with failed type identifier - `inst-gts-10a`
11. [x] - `p1` - **RETURN** all types provisioned successfully - `inst-gts-11`

> **Integration note**: types_registry calls use ToolKit default client timeout. No retry on failure — fail-fast during gear startup. Operator must ensure types_registry is available before starting OAGW.

### Registry Upstream and Route Provisioning Flow

- [x] `p1` - **ID**: `cpt-cf-oagw-flow-domain-registry-provisioning`

**Actor**: `cpt-cf-oagw-actor-types-registry`

**Success Scenarios**:
- Upstream and route instances registered in types_registry by other gears are created in OAGW storage during `post_init`, owned by the registry (`managed_by = registry`)
- On a restart with a configured database, unchanged instances are left as stored, changed instances are updated, and instances the registry no longer has are removed
- When replicas boot together, an instance another replica created after the lookup is accepted and startup continues
- An upstream instance whose endpoints or explicit alias now give another alias keeps its ID and routes and takes the new alias in place

**Error Scenarios**:
- Instance identifier has a non-UUID instance segment
- Instance content fails to deserialize or fails domain validation
- Listing the stored registry rows or looking up an instance fails for a reason other than not-found (for example, storage is unavailable)
- A create or update conflicts with a row the Management API created (an alias clash or a route overlap), or a Management API row holds the instance's ID
- Two route instances overlap each other (same tenant, upstream, path, priority, and a shared method, both enabled)
- An upstream instance wants the alias of a registry upstream that was kept because routes still use it
- A route instance names an upstream created through the Management API

**Steps**:
1. [x] - `p1` - List `gts.cf.core.oagw.upstream.v1~*` and `gts.cf.core.oagw.route.v1~*` instances from types_registry; extract the UUID from each GTS instance identifier; **IF** not a UUID, **RETURN** startup failure - `inst-prov-1`
2. [x] - `p1` - **IF** two enabled route instances share tenant, upstream, path, priority, and a method, **RETURN** startup failure naming both, before any write - `inst-prov-1a`
3. [x] - `p1` - **FOR EACH** stored registry-managed route, in any tenant, whose `(tenant, ID)` the registry no longer has: delete it; a route already gone counts as deleted - `inst-prov-2`
   1. [x] - `p1` - **IF** the registry still has it but its instance names another upstream, delete it too; it is re-created under the same ID below, which frees the old upstream and its alias in this boot - `inst-prov-2a`
4. [x] - `p1` - **FOR EACH** stored registry-managed upstream, in any tenant, whose `(tenant, ID)` the registry no longer has - `inst-prov-3`
   1. [x] - `p1` - **IF** routes still use it, keep it and log a warning; otherwise delete it (the cascade removes its child rows); an upstream already gone counts as deleted - `inst-prov-3a`
5. [x] - `p1` - **FOR EACH** upstream instance, stored ones before new ones, so that one whose alias changed frees it for a new instance in this boot - `inst-prov-4`
   1. [x] - `p1` - Look the ID up in the instance tenant; **IF** the lookup fails for any reason other than not-found, **RETURN** startup failure naming the instance ID and tenant - `inst-prov-4a`
   2. [x] - `p1` - **IF** not found, create it through the registry write path of the Control Plane with the instance UUID as its ID; **IF** the create conflicts, look the ID up again and continue with the row only when it now exists; **IF** it does not and a kept upstream holds the alias, the failure names that upstream and the routes that keep it - `inst-prov-4b`
   3. [x] - `p1` - **IF** the stored row was created through the Management API, **RETURN** startup failure; never overwrite it - `inst-prov-4c`
   4. [x] - `p1` - **IF** the stored row differs from the instance, replace it in full through the registry write path and log the differing fields; the write takes the alias by the create rules, so the alias follows the instance, and an alias a kept upstream holds fails as in the create above; otherwise leave it - `inst-prov-4d`
6. [x] - `p1` - **FOR EACH** route instance, as for upstreams; the route must name a registry-managed upstream, and one that names a Management API upstream fails; the overlap check of these writes skips other registry-managed routes, which already passed step 2; a route whose stored row names another upstream (moved by a replica on other registry content since step 3) is deleted and re-created under the same ID, and a re-create that conflicts continues only when the row found is this instance as wanted - `inst-prov-5`
7. [x] - `p1` - **IF** any create, update, or delete fails, **RETURN** startup failure naming the instance ID and tenant - `inst-prov-5a`
8. [x] - `p1` - **RETURN** provisioning complete with counts of created, updated, unchanged, removed, and kept rows - `inst-prov-6`

> **Registry rule**: the types registry owns the rows provisioned from it. Each boot makes them match the registry, and the Management API and the SDK cannot update or delete them (400 `failed_precondition`, `REGISTRY_MANAGED`). See [ADR-0018](../ADR/0018-optional-persistence.md#startup-provisioning-from-the-types-registry), including how replicas of two versions interact during a rolling deploy.

## 3. Processes / Business Logic (CDSL)

### Storage Selection

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-domain-storage-selection`

**Input**: Gear context (`ctx.db()`, gear configuration section)

**Output**: Upstream and route repository implementations, or startup error

**Steps**:
1. [x] - `p1` - **IF** `ctx.db()` returns a database provider - `inst-store-1`
   1. [x] - `p1` - Construct SeaORM `UpstreamRepository` and `RouteRepository` over the provider - `inst-store-1a`
   2. [x] - `p1` - **RETURN** database repositories - `inst-store-1b`
2. [x] - `p1` - **IF** the gear configuration section contains a non-null `database` value (a `null` value counts as absent and falls through to the next step) - `inst-store-2`
   1. [x] - `p1` - **RETURN** startup error: database configured but not available (for example, the global `database:` section is missing, so the runtime provides no database) - `inst-store-2a`
3. [x] - `p1` - Log warning: no database configured, upstream and route configuration is not persisted - `inst-store-3`
4. [x] - `p1` - **RETURN** in-memory repositories - `inst-store-4`

### Database Migration Execution

- [x] `p1` - **ID**: `cpt-cf-oagw-algo-domain-migration`

**Input**: OAGW migrations from `DatabaseCapability::migrations()`, privileged connection held by the ToolKit runtime

**Output**: Migration result (success with applied count, or error with details)

**Steps**:
1. [x] - `p1` - Runtime collects OAGW migrations from `DatabaseCapability::migrations()` - `inst-mig-1`
2. [x] - `p1` - Runtime determines pending migrations for OAGW - `inst-mig-2`
3. [x] - `p1` - **FOR EACH** pending migration in order - `inst-mig-3`
   1. [x] - `p1` - DB: EXECUTE migration DDL - `inst-mig-3a`
   2. [x] - `p1` - **IF** DDL fails - `inst-mig-3b`
      1. [x] - `p1` - **RETURN** migration error with migration name and details; gear startup fails - `inst-mig-3b2`
4. [x] - `p1` - Migration creates `oagw_upstream`, `oagw_upstream_tag`, `oagw_upstream_plugin`, `oagw_route`, `oagw_route_http_match`, `oagw_route_method`, `oagw_route_grpc_match`, `oagw_route_tag`, `oagw_route_plugin` per `cpt-cf-oagw-adr-storage-schema`; every table has a `tenant_id` column - `inst-mig-4`
5. [x] - `p1` - Migration creates unique `(tenant_id, alias)` and `(tenant_id, id)` on `oagw_upstream`, unique `(tenant_id, id)` on `oagw_route`, composite FKs `(tenant_id, parent_id)` → parent `(tenant_id, id)` with `ON DELETE CASCADE`, and the ADR-0009 indexes - `inst-mig-5`
6. [x] - `p1` - Migrations use SeaORM schema builders and portable column types (JSON in the backend's JSON type: `jsonb` on PostgreSQL, `json` on MySQL, text on SQLite; UTC timestamps; every string a `VARCHAR` sized for its stored field limit) so they run on PostgreSQL, MySQL, and SQLite; on MySQL only, tables compare strings byte-wise (`utf8mb4_0900_bin`) and timestamps are `datetime(6)` - `inst-mig-6`
7. [x] - `p1` - **RETURN** migration success with count of applied migrations - `inst-mig-7`

### Domain Model Validation

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-domain-entity-validation`

**Input**: Domain entity instance (Upstream, Route, Plugin, ServerConfig, Endpoint)

**Output**: Validation result with errors list

**Steps**:
1. [x] - `p1` - Parse and normalize input fields - `inst-val-1`
2. [x] - `p1` - **IF** entity is `Upstream` - `inst-val-2`
   1. [x] - `p1` - Validate `alias` is non-empty, lowercase, and matches allowed characters - `inst-val-2a`
   2. [ ] - `p1` - Validate `protocol` is a recognized GTS protocol identifier (not enforced yet: only the 256-byte, no-control-character limit applies) - `inst-val-2b`
   3. [x] - `p1` - Validate `server.endpoints` has at least one endpoint - `inst-val-2c`
   4. [x] - `p1` - Validate all endpoints share the same `scheme` and `port` - `inst-val-2d`
   5. [x] - `p1` - Validate sharing modes are one of `private`, `inherit`, `enforce` - `inst-val-2e`
3. [x] - `p1` - **IF** entity is `Route` - `inst-val-3`
   1. [x] - `p1` - Validate `match_type` is `http` or `grpc` - `inst-val-3a`
   2. [x] - `p1` - Validate `priority` is a non-negative integer - `inst-val-3b`
   3. [ ] - `p1` - **IF** `match_type` is `http`, validate `path_prefix` is normalized and within segment limit (not enforced yet: only the 2048-byte, no-control-character limit applies) - `inst-val-3c`
   4. [ ] - `p1` - **IF** `match_type` is `grpc`, validate `service` and `method` are non-empty (not enforced yet: only the 256-byte, no-control-character limit applies) - `inst-val-3d`
4. [ ] - `p1` - **IF** entity is `Plugin` - `inst-val-4`
   1. [ ] - `p1` - Validate `plugin_type` is `auth`, `guard`, or `transform` - `inst-val-4a`
   2. [ ] - `p1` - Validate `name` is non-empty and unique within tenant scope - `inst-val-4b`
   3. [ ] - `p1` - Validate `config_schema` is valid JSON schema - `inst-val-4c`
   4. [ ] - `p1` - Validate `source_code` is non-empty - `inst-val-4d`
5. [x] - `p1` - **IF** entity is `Endpoint` - `inst-val-5`
   1. [x] - `p1` - Validate `scheme` is `https` (HTTPS-only constraint for MVP) - `inst-val-5a`
   2. [x] - `p1` - Validate `host` is a valid hostname or IP address - `inst-val-5b`
   3. [x] - `p1` - Validate `port` is in valid range (1–65535) - `inst-val-5c`
6. [x] - `p1` - **RETURN** validation result with collected errors - `inst-val-6`

### SDK Trait Definition

- [x] `p2` - **ID**: `cpt-cf-oagw-algo-domain-sdk-definition`

**Input**: Design contract for `ServiceGatewayClientV1` from `cpt-cf-oagw-design-layers`

**Output**: SDK crate public API surface (`api.rs`, `models.rs`, `error.rs`)

**Steps**:
1. [x] - `p1` - Define `ServiceGatewayClientV1` async trait with methods matching management API and proxy operations - `inst-sdk-1`
2. [x] - `p1` - Define SDK model types that mirror domain entities without infrastructure dependencies - `inst-sdk-2`
3. [x] - `p1` - Define `ServiceGatewayError` enum covering all domain error cases - `inst-sdk-3`
4. [x] - `p1` - Ensure SDK types derive `Clone`, `Debug`, `Serialize`, `Deserialize` where appropriate - `inst-sdk-4`
5. [x] - `p1` - Export public API surface from `lib.rs` - `inst-sdk-5`
6. [x] - `p1` - **RETURN** SDK crate with trait, models, and error types - `inst-sdk-6`

## 4. States (CDSL)

Not applicable — the domain foundation feature establishes schema and wiring but does not define entity lifecycle state machines. State machines for entities (e.g., plugin GC lifecycle) belong to their respective features (Feature 3: Plugin System).

## 5. Definitions of Done

### Implement Domain Entities

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-entities`

The system **MUST** implement Rust domain model types for `Upstream`, `Route`, `Plugin`, `ServerConfig`, and `Endpoint` with all fields defined in `cpt-cf-oagw-design-domain-model`. Domain types **MUST** reside in the `domain/` layer and have no infrastructure dependencies.

**Implements**:
- `cpt-cf-oagw-flow-domain-gear-bootstrap`
- `cpt-cf-oagw-algo-domain-entity-validation`

**Touches**:
- Entities: `Upstream`, `Route`, `Plugin`, `ServerConfig`, `Endpoint`

### Implement Database Schema & Migrations

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-db-schema`

The system **MUST** create SeaORM migrations for every `cpt-cf-oagw-adr-storage-schema` table except `oagw_plugin`, with the constraints, indexes, and cascading deletes defined there. Every table, including child tables, **MUST** have a `tenant_id` column, and child foreign keys **MUST** be composite (`(tenant_id, parent_id)` → parent `(tenant_id, id)`). Migrations **MUST** be portable across PostgreSQL, MySQL, and SQLite and **MUST** be tested on SQLite, PostgreSQL, and MySQL. `oagw_plugin` is added with custom plugin support (Feature 3).

**Implements**:
- `cpt-cf-oagw-flow-domain-gear-bootstrap`
- `cpt-cf-oagw-algo-domain-migration`

**Touches**:
- DB: `oagw_upstream`, `oagw_upstream_tag`, `oagw_upstream_plugin`, `oagw_route`, `oagw_route_http_match`, `oagw_route_method`, `oagw_route_grpc_match`, `oagw_route_tag`, `oagw_route_plugin`

### Implement SeaORM Entities

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-orm-entities`

The system **MUST** implement SeaORM entity structs for every migrated table, each declaring `tenant_id` as its tenant column for the secure ORM. All database repository operations **MUST** use secure ORM with tenant scoping per `cpt-cf-oagw-principle-tenant-scope`; a child row's `tenant_id` **MUST** be taken from its parent. The database repositories **MUST** implement the `UpstreamRepository` and `RouteRepository` traits, which keep their other existing methods while `RouteRepository` replaces `find_matching` with `find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)` (`find_matching` is removed) and `UpstreamRepository` drops `get_by_alias`, mapping domain fields to tables per `cpt-cf-oagw-adr-optional-persistence` (Domain-to-Table Mapping): each create and update runs in one transaction opened by the repository and replaces the child rows, each delete is one statement, and `get_by_id` and `list` read in one read-only snapshot transaction; child rows **MUST** be batch-loaded once per child table per call (`<parent>_id IN (…)`), never per parent; `list_by_alias_for_tenants` loads the upstreams of a whole tenant chain, with their plugin rows, in one query; a `(tenant_id, alias)` unique violation **MUST** map to `RepositoryError::Conflict` with backend-neutral detection. `find_matching_in_tenants` (implemented by both backends) takes the ordered upstream IDs (selected upstream first, then ancestor upstreams closest-first) and loads their enabled HTTP routes across the whole tenant chain with only their match rows. It selects the winner with the shared matching logic in the order defined by `cpt-cf-oagw-adr-optional-persistence` (upstream preference, tenant chain position, longest path prefix, higher priority number, lowest route ID), then loads the winning route with its method and plugin rows, and its tag rows only for `Tags::Load`. Absent and empty optional fields (for example `plugins`) **MUST** round-trip unchanged.

**Implements**:
- `cpt-cf-oagw-algo-domain-migration`

**Touches**:
- DB: all migrated `oagw_*` tables
- Entities: `UpstreamRepository`, `RouteRepository`

### Implement Storage Selection

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-storage-selection`

The system **MUST** declare the ToolKit `db` capability and implement `DatabaseCapability::migrations()`. Gear initialization **MUST** select database repositories when `ctx.db()` returns a provider and in-memory repositories when no database is configured. Initialization **MUST** fail when the gear configuration contains a non-null `database` value but no database is available; a `null` value counts as absent. Running with in-memory repositories **MUST** log a warning that configuration is not persisted.

**Implements**:
- `cpt-cf-oagw-flow-domain-gear-bootstrap`
- `cpt-cf-oagw-algo-domain-storage-selection`

**Touches**:
- Entities: `OutboundApiGatewayGear`, `InMemoryUpstreamRepo`, `InMemoryRouteRepo`

### Implement Repository Conformance Tests

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-repo-conformance`

The system **MUST** provide one repository conformance test suite that runs against the in-memory repositories, SQLite, PostgreSQL, and MySQL, covering create, get, list pagination, update, delete, alias conflict, tenant isolation, route matching, round-trips of absent versus empty optional fields, upstream delete cascading to routes and child rows, alias shadowing and route selection across a multi-level tenant hierarchy, duplicate ID on create returning a conflict (upstreams and routes), route update matching `(id, tenant_id)`, canonical order of tags and HTTP methods, byte-wise comparison of tags and aliases, and route selection that falls back to routes on ancestor upstreams. Separate query-count tests on SQLite **MUST** assert that alias resolution and route matching issue the same number of queries for tenant chains of 1 and 5 tenants, and list calls the same number for page sizes 1 and 50; the counts do not depend on the backend, so the conformance suite does not repeat them on PostgreSQL or MySQL.

**Implements**:
- `cpt-cf-oagw-algo-domain-storage-selection`
- `cpt-cf-oagw-algo-domain-migration`

**Touches**:
- DB: all migrated `oagw_*` tables

### Implement Idempotent Registry Provisioning

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-registry-provisioning`

Startup provisioning **MUST** make the registry-managed upstreams and routes match the instances in types_registry: create missing ones, update changed ones, and remove ones the registry no longer has, except an upstream that routes still use, which is kept with a warning. A route the registry moved to another upstream **MUST** be removed before the upstreams are applied, so that its old upstream does not hold the alias the new one may take. Route instances that overlap each other **MUST** fail startup before any write, and registry routes **MUST NOT** fail their own overlap check against registry routes the same boot rewrites. Unchanged rows **MUST** be left as stored, so that a gear with a configured database restarts without errors or writes. A create that conflicts **MUST** continue only when the instance exists on a second lookup, so that replicas booting together all start while a clash with a Management API row still fails startup. A lookup or listing error **MUST** fail startup rather than read as absent. A changed registry upstream **MUST** take the alias its instance gives in place, keeping its ID and routes, and stored upstream instances **MUST** be applied before new ones, so that a new instance can take an alias freed in the same boot. A route instance **MUST** name a registry-managed upstream. Rows created through the Management API **MUST NOT** be changed by provisioning, and registry-managed rows **MUST NOT** be changed through the Management API or the SDK.

**Implements**:
- `cpt-cf-oagw-flow-domain-registry-provisioning`

**Touches**:
- Entities: `TypeProvisioningService`, `ProvisionedUpstream`, `ProvisionedRoute`, `RegistryProvisioner`
- DB: `managed_by` on `oagw_upstream` and `oagw_route`

### Implement SDK Crate

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-sdk-crate`

The system **MUST** implement the `oagw-sdk` crate with `ServiceGatewayClientV1` async trait, SDK model types, and `ServiceGatewayError` enum. The SDK crate **MUST** have no dependency on infrastructure or transport crates.

**Implements**:
- `cpt-cf-oagw-algo-domain-sdk-definition`

**Touches**:
- Entities: `ServiceGatewayClientV1`, SDK models, `ServiceGatewayError`

### Implement ToolKit Gear Wiring

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-toolkit-wiring`

The system **MUST** implement `gear.rs` with ToolKit `Gear` trait, `config.rs` with `OagwConfig`, and lifecycle hooks (`init`, `start`). Gear initialization **MUST** provision GTS types and register local client in ClientHub. Migrations and storage selection are covered by `cpt-cf-oagw-dod-domain-storage-selection`.

**Implements**:
- `cpt-cf-oagw-flow-domain-gear-bootstrap`

**Touches**:
- Entities: `OagwGear`, `OagwConfig`

### Implement GTS Type Provisioning

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-gts-provisioning`

The system **MUST** implement `type_provisioning.rs` that registers all OAGW GTS schemas (`upstream.v1~`, `route.v1~`, `auth_plugin.v1~`, `guard_plugin.v1~`, `transform_plugin.v1~`) and built-in plugin instances with `types_registry` during gear startup.

**Implements**:
- `cpt-cf-oagw-flow-domain-gts-provisioning`

**Touches**:
- Entities: GTS schemas and instances

### Implement DDD-Light Layering

- [x] `p1` - **ID**: `cpt-cf-oagw-dod-domain-layering`

The system **MUST** establish the DDD-Light directory structure (`domain/`, `infra/`, `api/rest/`) per `cpt-cf-oagw-design-layers`. Domain layer **MUST** define repository traits (`UpstreamRepository`, `RouteRepository`; `PluginRepository` comes with custom plugin support). Infrastructure layer **MUST** provide stub implementations. Domain layer **MUST NOT** depend on infrastructure.

**Implements**:
- `cpt-cf-oagw-flow-domain-gear-bootstrap`
- `cpt-cf-oagw-algo-domain-entity-validation`

**Touches**:
- Entities: `UpstreamRepository`, `RouteRepository` (`PluginRepository` comes with custom plugin support)

## 6. Acceptance Criteria

- [x] Migrations run successfully on SQLite, PostgreSQL, and MySQL in CI
- [x] All `cpt-cf-oagw-adr-storage-schema` tables except `oagw_plugin` are created with primary keys, unique constraints, composite foreign keys, and indexes; every table has `tenant_id`
- [x] The database rejects a child row whose `tenant_id` differs from its parent's
- [x] Deleting an upstream row removes, through the foreign-key cascade, its tags and plugin bindings and its routes with their match keys, methods, tags, and plugin bindings
- [x] Without database configuration, the gear starts with in-memory repositories and logs a warning
- [x] With a `database` section but no database provided (no global `database:` section), gear startup fails; an unreachable database already fails startup in `toolkit-db`, before `init`
- [x] With `database: null`, the gear starts with in-memory repositories and logs a warning
- [x] Upstreams and routes created through the Management API survive a gear restart when a database is configured
- [x] Repository conformance suite passes identically for in-memory, SQLite, PostgreSQL, and MySQL repositories
- [x] A second boot against the same database leaves unchanged registry instances as stored, without errors
- [x] A boot applies changed registry content to registry-managed rows and removes the ones the registry no longer has, keeping an upstream that routes still use
- [x] Management API and SDK update and delete of a registry-managed upstream or route fail with 400 `failed_precondition` (`REGISTRY_MANAGED`)
- [x] Domain entity types compile and enforce field-level validation per `cpt-cf-oagw-algo-domain-entity-validation`
- [x] `oagw-sdk` crate exports `ServiceGatewayClientV1` trait, all SDK model types, and `ServiceGatewayError`
- [x] `oagw-sdk` crate compiles with no dependency on infrastructure or transport crates
- [x] ToolKit gear starts successfully and registers in cf-gears-server
- [x] GTS schemas and built-in plugin instances are registered during gear startup
- [x] Secure ORM scoping is enforced: all repository queries include a tenant_id predicate, joined child rows are confined by the composite join key, and only `list_registry_keys` (startup reconcile) reads across tenants
- [x] Repository traits are defined in domain layer with no infrastructure imports
- [x] `cargo test` passes for all domain model unit tests and migration tests

## 7. Non-Applicable Concerns

- **Performance (PERF)**: Not applicable — this feature establishes schema and wiring only; no runtime hot paths or data processing. Performance-critical proxy paths belong to Feature 4.
- **Security — Audit Trail (SEC-FDESIGN-005)**: Not applicable — audit logging is scoped to Feature 8 (Observability & Metrics). Foundation layer does not produce auditable user actions.
- **Security — Data Protection (SEC-FDESIGN-004)**: Not applicable — OAGW does not store credentials directly; secret access is delegated to `cred_store` per `cpt-cf-oagw-design-dependencies`.
- **Compliance (COMPL)**: Not applicable — internal infrastructure gear with no regulatory or privacy obligations.
- **Usability (UX)**: Not applicable — no user interface; all interaction is programmatic via SDK or REST API (defined in Feature 2).
- **Operations — Observability (OPS-FDESIGN-001)**: Not applicable — logging, metrics, and tracing are scoped to Feature 8 (Observability & Metrics). Foundation provides structural hooks only.
- **Operations — Rollout (OPS-FDESIGN-004)**: Not applicable — schema migrations are forward-only and idempotent; re-running gear init after partial failure safely resumes from the last unapplied migration. Enabling persistence on an existing deployment needs no data migration: in-memory configuration was never persisted, and registry instances are re-provisioned on the first boot with a database.
