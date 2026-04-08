# Feature: Core Domain & Storage Foundation


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Module Bootstrap Flow](#module-bootstrap-flow)
  - [GTS Type Provisioning Flow](#gts-type-provisioning-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Database Migration Execution](#database-migration-execution)
  - [Domain Model Validation](#domain-model-validation)
  - [SDK Trait Definition](#sdk-trait-definition)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Domain Entities](#implement-domain-entities)
  - [Implement Database Schema & Migrations](#implement-database-schema--migrations)
  - [Implement SeaORM Entities](#implement-seaorm-entities)
  - [Implement SDK Crate](#implement-sdk-crate)
  - [Implement ModKit Module Wiring](#implement-modkit-module-wiring)
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

Establish domain model entities, database schema, ModKit module wiring, SDK crate, and GTS type provisioning for the OAGW module.

### 1.2 Purpose

Foundation layer that all other OAGW features depend on. Provides the shared domain entities (Upstream, Route, Plugin, ServerConfig, Endpoint), all `oagw_*` database tables with migrations, the `oagw-sdk` public crate (`ServiceGatewayClientV1` trait, SDK models, errors), ModKit module wiring, and GTS type registration.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Operates the module; configuration managed via this foundation |
| `cpt-cf-oagw-actor-types-registry` | Receives GTS schema/instance registrations during type provisioning |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Requirements**: `cpt-cf-oagw-nfr-multi-tenancy`
- **Design elements**: `cpt-cf-oagw-design-domain-model`, `cpt-cf-oagw-db-schema`, `cpt-cf-oagw-design-layers`, `cpt-cf-oagw-design-dependencies`, `cpt-cf-oagw-design-drivers`, `cpt-cf-oagw-design-overview`
- **Principles**: `cpt-cf-oagw-principle-tenant-scope`
- **Constraints**: `cpt-cf-oagw-constraint-modkit-deploy`, `cpt-cf-oagw-constraint-multi-sql`
- **ADRs**: `cpt-cf-oagw-adr-storage-schema`
- **Dependencies**: None

### 1.5 Out of Scope

- CRUD handler implementations for upstreams, routes, and plugins (Feature 2: Management API)
- Plugin trait implementations and execution engine (Feature 3: Plugin System)
- Proxy request routing and execution logic (Feature 4: Proxy Engine)
- SSE event streaming (Feature 5: Real-Time Events)

## 2. Actor Flows (CDSL)

### Module Bootstrap Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-domain-module-bootstrap`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Success Scenarios**:
- Module starts, migrations run, GTS types provisioned, local client registered, module ready to serve

**Error Scenarios**:
- Migration failure (DB connection error, schema conflict)
- GTS registration failure (types_registry unavailable)

**Steps**:
1. [ ] - `p1` - Platform operator starts hyperspot-server with OAGW module enabled - `inst-boot-1`
2. [ ] - `p1` - ModKit invokes OAGW `Module::init()` with application context - `inst-boot-2`
3. [ ] - `p1` - Load `OagwConfig` from configuration file (fields defined in `cpt-cf-oagw-design-overview`) - `inst-boot-3`
4. [ ] - `p1` - DB: RUN all `oagw_*` migrations (SeaORM migrator) - `inst-boot-4`
5. [ ] - `p1` - **IF** migration fails - `inst-boot-5`
   1. [ ] - `p1` - Log error and **RETURN** module initialization failure - `inst-boot-5a`
6. [ ] - `p1` - Call GTS type provisioning: register all OAGW schemas and built-in instances - `inst-boot-6`
7. [ ] - `p1` - **IF** GTS registration fails - `inst-boot-7`
   1. [ ] - `p1` - Log error and **RETURN** module initialization failure - `inst-boot-7a`
8. [ ] - `p1` - Register `ServiceGatewayClientV1` local client in ClientHub - `inst-boot-8`
9. [ ] - `p1` - Register REST API routes via OperationBuilder - `inst-boot-9`
10. [ ] - `p1` - **RETURN** module initialized successfully - `inst-boot-10`

### GTS Type Provisioning Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-domain-gts-provisioning`

**Actor**: `cpt-cf-oagw-actor-types-registry`

**Success Scenarios**:
- All OAGW GTS schemas registered
- Built-in plugin instances registered

**Error Scenarios**:
- types_registry unavailable
- Schema conflicts with existing registrations

**Steps**:
1. [ ] - `p1` - OAGW module calls types_registry client to register schemas - `inst-gts-1`
2. [ ] - `p1` - Register upstream schema: `gts.x.core.oagw.upstream.v1~` - `inst-gts-2`
3. [ ] - `p1` - Register route schema: `gts.x.core.oagw.route.v1~` - `inst-gts-3`
4. [ ] - `p1` - Register auth plugin schema: `gts.x.core.oagw.auth_plugin.v1~` - `inst-gts-4`
5. [ ] - `p1` - Register guard plugin schema: `gts.x.core.oagw.guard_plugin.v1~` - `inst-gts-5`
6. [ ] - `p1` - Register transform plugin schema: `gts.x.core.oagw.transform_plugin.v1~` - `inst-gts-6`
7. [ ] - `p1` - Register error type schema: `gts.x.core.errors.err.v1~` (OAGW error instances) - `inst-gts-7`
8. [ ] - `p1` - Register proxy permission: `gts.x.core.oagw.proxy.v1~` - `inst-gts-8`
9. [ ] - `p1` - **FOR EACH** built-in plugin in auth/guard/transform registries - `inst-gts-9`
   1. [ ] - `p1` - Register instance with full GTS identifier - `inst-gts-9a`
10. [ ] - `p1` - **IF** any registration fails - `inst-gts-10`
    1. [ ] - `p1` - **RETURN** provisioning error with failed type identifier - `inst-gts-10a`
11. [ ] - `p1` - **RETURN** all types provisioned successfully - `inst-gts-11`

> **Integration note**: types_registry calls use ModKit default client timeout. No retry on failure — fail-fast during module startup. Operator must ensure types_registry is available before starting OAGW.

## 3. Processes / Business Logic (CDSL)

### Database Migration Execution

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-domain-migration`

**Input**: Database connection from ModKit application context, migration directory

**Output**: Migration result (success with applied count, or error with details)

**Steps**:
1. [ ] - `p1` - Obtain database connection from application context - `inst-mig-1`
2. [ ] - `p1` - Enumerate pending migrations for all `oagw_*` tables - `inst-mig-2`
3. [ ] - `p1` - **FOR EACH** pending migration in order - `inst-mig-3`
   1. [ ] - `p1` - DB: EXECUTE migration DDL within transaction - `inst-mig-3a`
   2. [ ] - `p1` - **IF** DDL fails - `inst-mig-3b`
      1. [ ] - `p1` - DB: ROLLBACK transaction - `inst-mig-3b1`
      2. [ ] - `p1` - **RETURN** migration error with table name and details - `inst-mig-3b2`
   3. [ ] - `p1` - DB: COMMIT migration transaction - `inst-mig-3c`
4. [ ] - `p1` - Verify all required tables exist: `oagw_upstream`, `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`, `oagw_upstream_tag`, `oagw_route_tag`, `oagw_plugin`, `oagw_upstream_plugin`, `oagw_route_plugin` - `inst-mig-4`
5. [ ] - `p1` - Verify unique constraints: `(tenant_id, alias)` on `oagw_upstream`, `(tenant_id, name)` on `oagw_plugin` - `inst-mig-5`
6. [ ] - `p1` - Verify indexes: `(alias, tenant_id)`, `(auth_plugin_uuid)`, `(upstream_id, enabled, match_type, priority)`, `(path_prefix, route_id)`, `(gc_eligible_at)` - `inst-mig-6`
7. [ ] - `p1` - **RETURN** migration success with count of applied migrations - `inst-mig-7`

### Domain Model Validation

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-domain-entity-validation`

**Input**: Domain entity instance (Upstream, Route, Plugin, ServerConfig, Endpoint)

**Output**: Validation result with errors list

**Steps**:
1. [ ] - `p1` - Parse and normalize input fields - `inst-val-1`
2. [ ] - `p1` - **IF** entity is `Upstream` - `inst-val-2`
   1. [ ] - `p1` - Validate `alias` is non-empty, lowercase, and matches allowed characters - `inst-val-2a`
   2. [ ] - `p1` - Validate `protocol` is a recognized GTS protocol identifier - `inst-val-2b`
   3. [ ] - `p1` - Validate `server.endpoints` has at least one endpoint - `inst-val-2c`
   4. [ ] - `p1` - Validate all endpoints share the same `scheme` and `port` - `inst-val-2d`
   5. [ ] - `p1` - Validate sharing modes are one of `private`, `inherit`, `enforce` - `inst-val-2e`
3. [ ] - `p1` - **IF** entity is `Route` - `inst-val-3`
   1. [ ] - `p1` - Validate `match_type` is `http` or `grpc` - `inst-val-3a`
   2. [ ] - `p1` - Validate `priority` is a non-negative integer - `inst-val-3b`
   3. [ ] - `p1` - **IF** `match_type` is `http`, validate `path_prefix` is normalized and within segment limit - `inst-val-3c`
   4. [ ] - `p1` - **IF** `match_type` is `grpc`, validate `service` and `method` are non-empty - `inst-val-3d`
4. [ ] - `p1` - **IF** entity is `Plugin` - `inst-val-4`
   1. [ ] - `p1` - Validate `plugin_type` is `auth`, `guard`, or `transform` - `inst-val-4a`
   2. [ ] - `p1` - Validate `name` is non-empty and unique within tenant scope - `inst-val-4b`
   3. [ ] - `p1` - Validate `config_schema` is valid JSON schema - `inst-val-4c`
   4. [ ] - `p1` - Validate `source_code` is non-empty - `inst-val-4d`
5. [ ] - `p1` - **IF** entity is `Endpoint` - `inst-val-5`
   1. [ ] - `p1` - Validate `scheme` is `https` (HTTPS-only constraint for MVP) - `inst-val-5a`
   2. [ ] - `p1` - Validate `host` is a valid hostname or IP address - `inst-val-5b`
   3. [ ] - `p1` - Validate `port` is in valid range (1–65535) - `inst-val-5c`
6. [ ] - `p1` - **RETURN** validation result with collected errors - `inst-val-6`

### SDK Trait Definition

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-domain-sdk-definition`

**Input**: Design contract for `ServiceGatewayClientV1` from `cpt-cf-oagw-design-layers`

**Output**: SDK crate public API surface (`api.rs`, `models.rs`, `error.rs`)

**Steps**:
1. [ ] - `p1` - Define `ServiceGatewayClientV1` async trait with methods matching management API and proxy operations - `inst-sdk-1`
2. [ ] - `p1` - Define SDK model types that mirror domain entities without infrastructure dependencies - `inst-sdk-2`
3. [ ] - `p1` - Define `ServiceGatewayError` enum covering all domain error cases - `inst-sdk-3`
4. [ ] - `p1` - Ensure SDK types derive `Clone`, `Debug`, `Serialize`, `Deserialize` where appropriate - `inst-sdk-4`
5. [ ] - `p1` - Export public API surface from `lib.rs` - `inst-sdk-5`
6. [ ] - `p1` - **RETURN** SDK crate with trait, models, and error types - `inst-sdk-6`

## 4. States (CDSL)

Not applicable — the domain foundation feature establishes schema and wiring but does not define entity lifecycle state machines. State machines for entities (e.g., plugin GC lifecycle) belong to their respective features (Feature 3: Plugin System).

## 5. Definitions of Done

### Implement Domain Entities

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-entities`

The system **MUST** implement Rust domain model types for `Upstream`, `Route`, `Plugin`, `ServerConfig`, and `Endpoint` with all fields defined in `cpt-cf-oagw-design-domain-model`. Domain types **MUST** reside in the `domain/` layer and have no infrastructure dependencies.

**Implements**:
- `cpt-cf-oagw-flow-domain-module-bootstrap`
- `cpt-cf-oagw-algo-domain-entity-validation`

**Touches**:
- Entities: `Upstream`, `Route`, `Plugin`, `ServerConfig`, `Endpoint`

### Implement Database Schema & Migrations

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-db-schema`

The system **MUST** create SeaORM migrations that establish all `oagw_*` tables (`oagw_upstream`, `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`, `oagw_upstream_tag`, `oagw_route_tag`, `oagw_plugin`, `oagw_upstream_plugin`, `oagw_route_plugin`) with constraints, indexes, and cascading deletes per `cpt-cf-oagw-adr-storage-schema`. Migrations **MUST** run on PostgreSQL, MySQL, and SQLite.

**Implements**:
- `cpt-cf-oagw-flow-domain-module-bootstrap`
- `cpt-cf-oagw-algo-domain-migration`

**Touches**:
- DB: `oagw_upstream`, `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`, `oagw_upstream_tag`, `oagw_route_tag`, `oagw_plugin`, `oagw_upstream_plugin`, `oagw_route_plugin`

### Implement SeaORM Entities

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-orm-entities`

The system **MUST** implement SeaORM entity structs for all `oagw_*` tables. All repository operations **MUST** use secure ORM with tenant scoping per `cpt-cf-oagw-principle-tenant-scope`. Dependent tables without `tenant_id` (tags, methods, match keys, plugin bindings) **MUST** apply scoping via joins against `oagw_upstream` or `oagw_route`.

**Implements**:
- `cpt-cf-oagw-algo-domain-migration`

**Touches**:
- DB: `oagw_upstream`, `oagw_route`, `oagw_route_http_match`, `oagw_route_grpc_match`, `oagw_route_method`, `oagw_upstream_tag`, `oagw_route_tag`, `oagw_plugin`, `oagw_upstream_plugin`, `oagw_route_plugin`

### Implement SDK Crate

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-sdk-crate`

The system **MUST** implement the `oagw-sdk` crate with `ServiceGatewayClientV1` async trait, SDK model types, and `ServiceGatewayError` enum. The SDK crate **MUST** have no dependency on infrastructure or transport crates.

**Implements**:
- `cpt-cf-oagw-algo-domain-sdk-definition`

**Touches**:
- Entities: `ServiceGatewayClientV1`, SDK models, `ServiceGatewayError`

### Implement ModKit Module Wiring

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-modkit-wiring`

The system **MUST** implement `module.rs` with ModKit `Module` trait, `config.rs` with `OagwConfig`, and lifecycle hooks (`init`, `start`). Module initialization **MUST** run migrations, provision GTS types, and register local client in ClientHub.

**Implements**:
- `cpt-cf-oagw-flow-domain-module-bootstrap`

**Touches**:
- Entities: `OagwModule`, `OagwConfig`

### Implement GTS Type Provisioning

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-gts-provisioning`

The system **MUST** implement `type_provisioning.rs` that registers all OAGW GTS schemas (`upstream.v1~`, `route.v1~`, `auth_plugin.v1~`, `guard_plugin.v1~`, `transform_plugin.v1~`) and built-in plugin instances with `types_registry` during module startup.

**Implements**:
- `cpt-cf-oagw-flow-domain-gts-provisioning`

**Touches**:
- Entities: GTS schemas and instances

### Implement DDD-Light Layering

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-domain-layering`

The system **MUST** establish the DDD-Light directory structure (`domain/`, `infra/`, `api/rest/`) per `cpt-cf-oagw-design-layers`. Domain layer **MUST** define repository traits (`UpstreamRepository`, `RouteRepository`, `PluginRepository`). Infrastructure layer **MUST** provide stub implementations. Domain layer **MUST NOT** depend on infrastructure.

**Implements**:
- `cpt-cf-oagw-flow-domain-module-bootstrap`
- `cpt-cf-oagw-algo-domain-entity-validation`

**Touches**:
- Entities: `UpstreamRepository`, `RouteRepository`, `PluginRepository`

## 6. Acceptance Criteria

- [ ] Migrations run successfully on PostgreSQL, MySQL, and SQLite without errors
- [ ] All ten `oagw_*` tables are created with correct primary keys, foreign keys, unique constraints, and indexes per `cpt-cf-oagw-adr-storage-schema`
- [ ] Cascading deletes propagate correctly: deleting an upstream deletes its routes, tags, match keys, methods, and plugin bindings
- [ ] Domain entity types compile and enforce field-level validation per `cpt-cf-oagw-algo-domain-entity-validation`
- [ ] `oagw-sdk` crate exports `ServiceGatewayClientV1` trait, all SDK model types, and `ServiceGatewayError`
- [ ] `oagw-sdk` crate compiles with no dependency on infrastructure or transport crates
- [ ] ModKit module starts successfully and registers in hyperspot-server
- [ ] GTS schemas and built-in plugin instances are registered during module startup
- [ ] Secure ORM scoping is enforced: all repository queries include tenant_id predicate or scoped join
- [ ] Repository traits are defined in domain layer with no infrastructure imports
- [ ] `cargo test` passes for all domain model unit tests and migration tests

## 7. Non-Applicable Concerns

- **Performance (PERF)**: Not applicable — this feature establishes schema and wiring only; no runtime hot paths or data processing. Performance-critical proxy paths belong to Feature 4.
- **Security — Audit Trail (SEC-FDESIGN-005)**: Not applicable — audit logging is scoped to Feature 8 (Observability & Metrics). Foundation layer does not produce auditable user actions.
- **Security — Data Protection (SEC-FDESIGN-004)**: Not applicable — OAGW does not store credentials directly; secret access is delegated to `cred_store` per `cpt-cf-oagw-design-dependencies`.
- **Compliance (COMPL)**: Not applicable — internal infrastructure module with no regulatory or privacy obligations.
- **Usability (UX)**: Not applicable — no user interface; all interaction is programmatic via SDK or REST API (defined in Feature 2).
- **Operations — Observability (OPS-FDESIGN-001)**: Not applicable — logging, metrics, and tracing are scoped to Feature 8 (Observability & Metrics). Foundation provides structural hooks only.
- **Operations — Rollout (OPS-FDESIGN-004)**: Not applicable — schema migrations are forward-only and idempotent; re-running module init after partial failure safely resumes from the last unapplied migration.
