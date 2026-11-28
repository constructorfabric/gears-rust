# Feature: Plugin System


<!-- toc -->

- [Feature: Plugin System](#feature-plugin-system)
  - [1. Feature Context](#1-feature-context)
    - [1.1 Overview](#11-overview)
    - [1.2 Purpose](#12-purpose)
    - [1.3 Actors](#13-actors)
    - [1.4 References](#14-references)
  - [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
    - [Create Custom Plugin Flow](#create-custom-plugin-flow)
    - [List and Get Plugin Flow](#list-and-get-plugin-flow)
    - [Delete Custom Plugin Flow](#delete-custom-plugin-flow)
    - [Bind Plugin to Upstream or Route Flow](#bind-plugin-to-upstream-or-route-flow)
  - [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
    - [Plugin Ref Resolution Algorithm](#plugin-ref-resolution-algorithm)
    - [Plugin Validation Algorithm](#plugin-validation-algorithm)
    - [Plugin In-Use Check Algorithm](#plugin-in-use-check-algorithm)
    - [Plugin GC Marking Algorithm](#plugin-gc-marking-algorithm)
    - [Plugin GC Cleanup Algorithm](#plugin-gc-cleanup-algorithm)
  - [4. States (CDSL)](#4-states-cdsl)
    - [Custom Plugin Lifecycle State Machine](#custom-plugin-lifecycle-state-machine)
  - [5. Definitions of Done](#5-definitions-of-done)
    - [Implement Plugin Trait Definitions](#implement-plugin-trait-definitions)
    - [Implement All 12 Built-in Plugins](#implement-all-12-built-in-plugins)
    - [Implement Plugin CRUD REST Handlers](#implement-plugin-crud-rest-handlers)
    - [Implement Plugin Identification Model](#implement-plugin-identification-model)
    - [Implement Starlark Sandbox](#implement-starlark-sandbox)
    - [Implement Plugin Garbage Collection](#implement-plugin-garbage-collection)
  - [6. Acceptance Criteria](#6-acceptance-criteria)
  - [7. Additional Context](#7-additional-context)
    - [Auth Plugin Credential Resolution](#auth-plugin-credential-resolution)
    - [Auth Plugin Request Signing](#auth-plugin-request-signing)
    - [AWS SigV4 Auth Plugin](#aws-sigv4-auth-plugin)
    - [Scope Boundaries](#scope-boundaries)
    - [Performance Considerations](#performance-considerations)
    - [Usability Considerations](#usability-considerations)
    - [Compliance Considerations](#compliance-considerations)
    - [Audit Trail](#audit-trail)
    - [Observability](#observability)
    - [Fault Tolerance](#fault-tolerance)
    - [Component Interactions](#component-interactions)
    - [Configuration Parameters](#configuration-parameters)
    - [Known Limitations](#known-limitations)
    - [Test Guidance](#test-guidance)
    - [Rollout \& Rollback](#rollout--rollback)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-oagw-featstatus-plugin-system-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-oagw-feature-plugin-system`

## 1. Feature Context

### 1.1 Overview

Implement the three-type plugin model (Auth/Guard/Transform) with built-in plugins, plugin registry, GTS identification, custom Starlark sandbox, lifecycle management, and garbage collection.

### 1.2 Purpose

Provides extensibility for custom authentication schemes, validation rules, and request/response transformations. Covers `cpt-cf-oagw-fr-plugin-system`, `cpt-cf-oagw-fr-builtin-plugins`, `cpt-cf-oagw-fr-auth-injection`.

Adheres to `cpt-cf-oagw-principle-plugin-immutable` (plugins immutable after creation) and `cpt-cf-oagw-principle-cred-isolation` (credentials via `cred_store` references only).

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Manages plugin definitions and built-in plugin configuration |
| `cpt-cf-oagw-actor-tenant-admin` | Creates tenant-scoped custom Starlark plugins |
| `cpt-cf-oagw-actor-cred-store` | Provides secret material for auth plugin credential injection |
| `cpt-cf-oagw-actor-types-registry` | Validates plugin type schemas via GTS |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [0003-plugin-system.md](../ADR/0003-plugin-system.md), [0009-storage-schema.md](../ADR/0009-storage-schema.md), [0023-request-signing.md](../ADR/0023-request-signing.md)
- **Requirements**: `cpt-cf-oagw-fr-plugin-system`, `cpt-cf-oagw-fr-builtin-plugins`, `cpt-cf-oagw-fr-auth-injection`, `cpt-cf-oagw-nfr-credential-isolation`, `cpt-cf-oagw-nfr-starlark-sandbox`
- **Design elements**: `cpt-cf-oagw-component-model`, `cpt-cf-oagw-db-schema`, `cpt-cf-oagw-adr-plugin-system`, `cpt-cf-oagw-adr-storage-schema`
- **Dependencies**: `cpt-cf-oagw-feature-domain-foundation`

## 2. Actor Flows (CDSL)

### Create Custom Plugin Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-plugin-create`

**Actor**: `cpt-cf-oagw-actor-platform-operator` or `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Custom Starlark plugin is created and persisted in `oagw_plugin`
- Plugin is available for binding to upstreams and routes

**Error Scenarios**:
- Starlark source fails sandbox validation (syntax error, forbidden I/O)
- Config schema is invalid JSON Schema
- Plugin name already exists for tenant (`409 Conflict`)
- Plugin type not in `{auth, guard, transform}`

**Steps**:
1. [ ] - `p1` - Actor sends POST /api/oagw/v1/plugins with name, plugin_type, config_schema, source_code - `inst-create-1`
2. [ ] - `p1` - API: Extract SecurityContext (tenant_id, permissions) - `inst-create-2`
3. [ ] - `p1` - API: Validate permission `gts.cf.core.oagw.{type}_plugin.v1~:create` - `inst-create-3`
4. [ ] - `p1` - Validate plugin_type is one of `auth`, `guard`, `transform` - `inst-create-4`
5. [ ] - `p1` - Validate config_schema is valid JSON Schema - `inst-create-5`
6. [ ] - `p1` - Validate Starlark source via sandbox (syntax check, no network/file I/O imports) - `inst-create-6`
7. [ ] - `p1` - DB: Check uniqueness of `(tenant_id, name)` in `oagw_plugin` - `inst-create-7`
8. [ ] - `p1` - **IF** name conflict - `inst-create-8`
   1. [ ] - `p1` - **RETURN** 409 Conflict - `inst-create-8a`
9. [ ] - `p1` - **ELSE** - `inst-create-9`
   1. [ ] - `p1` - DB: INSERT INTO oagw_plugin (id, tenant_id, plugin_type, name, config_schema, source_code, created_at, updated_at) - `inst-create-9a`
   2. [ ] - `p1` - Generate GTS identifier: `gts.cf.core.oagw.{type}_plugin.v1~{uuid}` - `inst-create-9b`
   3. [ ] - `p1` - **RETURN** 201 Created with plugin resource - `inst-create-9c`

### List and Get Plugin Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-plugin-read`

**Actor**: `cpt-cf-oagw-actor-platform-operator` or `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- List returns tenant-scoped custom plugins with OData query support
- Get returns plugin details by GTS anonymous identifier
- Get source returns Starlark source code for a custom plugin

**Error Scenarios**:
- Plugin not found (`404 Not Found`)
- Get source on a named (built-in) plugin (`404 Not Found` — named plugins have no stored source)

**Steps**:
1. [ ] - `p1` - Actor sends GET /api/oagw/v1/plugins (list) or GET /api/oagw/v1/plugins/{id} (get) or GET /api/oagw/v1/plugins/{id}/source (get source) - `inst-read-1`
2. [ ] - `p1` - API: Extract SecurityContext (tenant_id, permissions) - `inst-read-2`
3. [ ] - `p1` - API: Validate permission `gts.cf.core.oagw.{type}_plugin.v1~:read` - `inst-read-3`
4. [ ] - `p1` - **IF** list request - `inst-read-4`
   1. [ ] - `p1` - DB: SELECT FROM oagw_plugin WHERE tenant_id = :tenant_id with OData filters ($filter, $select, $top, $skip) - `inst-read-4a`
   2. [ ] - `p1` - **RETURN** 200 OK with plugin collection - `inst-read-4b`
5. [ ] - `p1` - **IF** get-by-id request - `inst-read-5`
   1. [ ] - `p1` - Parse GTS identifier to extract UUID - `inst-read-5a`
   2. [ ] - `p1` - DB: SELECT FROM oagw_plugin WHERE id = :uuid AND tenant_id = :tenant_id - `inst-read-5b`
   3. [ ] - `p1` - **IF** not found, **RETURN** 404 Not Found - `inst-read-5c`
   4. [ ] - `p1` - **RETURN** 200 OK with plugin resource - `inst-read-5d`
6. [ ] - `p1` - **IF** get-source request - `inst-read-6`
   1. [ ] - `p1` - Parse GTS identifier to extract UUID - `inst-read-6a`
   2. [ ] - `p1` - DB: SELECT source_code FROM oagw_plugin WHERE id = :uuid AND tenant_id = :tenant_id - `inst-read-6b`
   3. [ ] - `p1` - **IF** not found, **RETURN** 404 Not Found - `inst-read-6c`
   4. [ ] - `p1` - **RETURN** 200 OK with source content - `inst-read-6d`

### Delete Custom Plugin Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-plugin-delete`

**Actor**: `cpt-cf-oagw-actor-platform-operator` or `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Unlinked plugin is deleted immediately
- Plugin referenced by upstreams/routes is rejected with `409 PluginInUse`

**Error Scenarios**:
- Plugin not found (`404 Not Found`)
- Plugin still in use (`409 PluginInUse`)

**Steps**:
1. [ ] - `p1` - Actor sends DELETE /api/oagw/v1/plugins/{id} - `inst-delete-1`
2. [ ] - `p1` - API: Extract SecurityContext (tenant_id, permissions) - `inst-delete-2`
3. [ ] - `p1` - API: Validate permission `gts.cf.core.oagw.{type}_plugin.v1~:delete` - `inst-delete-3`
4. [ ] - `p1` - Parse GTS identifier to extract UUID - `inst-delete-4`
5. [ ] - `p1` - DB: SELECT FROM oagw_plugin WHERE id = :uuid AND tenant_id = :tenant_id - `inst-delete-5`
6. [ ] - `p1` - **IF** not found - `inst-delete-6`
   1. [ ] - `p1` - **RETURN** 404 Not Found - `inst-delete-6a`
7. [ ] - `p1` - Execute plugin in-use check (`cpt-cf-oagw-algo-plugin-in-use-check`) - `inst-delete-7`
8. [ ] - `p1` - **IF** plugin in use - `inst-delete-8`
   1. [ ] - `p1` - **RETURN** 409 PluginInUse (RFC 9457, `gts.cf.core.errors.err.v1~cf.oagw.plugin.in_use.v1`) - `inst-delete-8a`
9. [ ] - `p1` - **ELSE** - `inst-delete-9`
   1. [ ] - `p1` - DB: DELETE FROM oagw_plugin WHERE id = :uuid AND tenant_id = :tenant_id - `inst-delete-9a`
   2. [ ] - `p1` - **RETURN** 204 No Content - `inst-delete-9b`

### Bind Plugin to Upstream or Route Flow

- [ ] `p1` - **ID**: `cpt-cf-oagw-flow-plugin-bind`

**Actor**: `cpt-cf-oagw-actor-platform-operator` or `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Plugin (named or custom) is bound to upstream or route at specified position
- Auth plugin bound as scalar on upstream (`auth_plugin_ref`, `auth_plugin_uuid`)

**Error Scenarios**:
- Plugin reference does not resolve (named not in registry, UUID not in `oagw_plugin`). On upstream and route writes, any id that is not an implemented built-in plugin, custom UUID ids included, is rejected with 400 `NOT_IMPLEMENTED`; an auth plugin in `plugins.items` with 400 `VALIDATION`
- Plugin type mismatch (e.g., auth plugin in guard/transform binding)
- Position not contiguous from 0
- Auth on routes is unsupported: the proxy uses upstream auth and ignores route auth references today; rejecting this configuration is planned in [#4974](https://github.com/constructorfabric/gears-rust/issues/4974). The upstream chooses the plugin, header name and prefix. [ADR-0021](../ADR/0021-upstream-credentials.md) proposes letting the caller name a credential on that upstream to specify which API key the plugin should use.

**Steps**:
1. [x] - `p1` - Actor sends POST or PUT for upstream/route with plugin bindings in request body - `inst-bind-1`
2. [x] - `p1` - API: Extract SecurityContext (tenant_id, permissions) - `inst-bind-2`
3. [ ] - `p1` - **FOR EACH** plugin reference in bindings - `inst-bind-3`
   1. [ ] - `p1` - Execute plugin ref resolution (`cpt-cf-oagw-algo-plugin-ref-resolution`) - `inst-bind-3a`
   2. [ ] - `p1` - **IF** resolution fails, **RETURN** 400 ValidationError - `inst-bind-3b`
   3. [ ] - `p1` - Validate plugin type matches binding context (guard/transform only for binding tables; auth only for upstream scalar) - `inst-bind-3c`
   4. [ ] - `p1` - **IF** type mismatch, **RETURN** 400 ValidationError - `inst-bind-3d`
4. [ ] - `p1` - Validate positions are contiguous from 0 with no gaps - `inst-bind-4`
5. [x] - `p1` - **IF** auth plugin binding on upstream - `inst-bind-5`
   1. [x] - `p1` - DB: UPDATE oagw_upstream SET auth_plugin_ref = :ref, auth_plugin_uuid = :uuid, auth_config = :config - `inst-bind-5a`
6. [x] - `p1` - **IF** guard/transform plugin bindings - `inst-bind-6`
   1. [x] - `p1` - DB: DELETE existing bindings for parent, INSERT new oagw_upstream_plugin / oagw_route_plugin rows (atomic transaction) - `inst-bind-6a`
7. [ ] - `p1` - Clear `gc_eligible_at` on any newly-referenced custom plugins - `inst-bind-7`
8. [ ] - `p1` - Mark `gc_eligible_at` on any previously-referenced custom plugins that are now unlinked - `inst-bind-8`
9. [x] - `p1` - **RETURN** success response with updated resource - `inst-bind-9`

## 3. Processes / Business Logic (CDSL)

### Plugin Ref Resolution Algorithm

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-plugin-ref-resolution`

**Input**: GTS plugin identifier string (e.g., `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1` or `gts.cf.core.oagw.guard_plugin.v1~550e8400-...`)

**Output**: Resolved plugin (named registry entry or custom plugin row) or error

**Steps**:
1. [x] - `p1` - Parse GTS identifier to extract schema type and instance part (after `~`) - `inst-resolve-1`
2. [ ] - `p1` - **IF** instance part parses as UUID - `inst-resolve-2`
   1. [ ] - `p1` - DB: SELECT FROM oagw_plugin WHERE id = :uuid AND tenant_id = :tenant_id - `inst-resolve-2a`
   2. [ ] - `p1` - **IF** not found, **RETURN** error (PluginNotFound) - `inst-resolve-2b`
   3. [ ] - `p1` - Validate that plugin_type matches the schema type extracted from GTS identifier - `inst-resolve-2c`
   4. [ ] - `p1` - **IF** type mismatch, **RETURN** error (ValidationError) - `inst-resolve-2d`
   5. [ ] - `p1` - **RETURN** resolved custom plugin - `inst-resolve-2e`
3. [x] - `p1` - **ELSE** (named plugin) - `inst-resolve-3`
   1. [x] - `p1` - Look up full GTS identifier in the in-process plugin registry - `inst-resolve-3a`
   2. [x] - `p1` - **IF** not found in registry, **RETURN** error (PluginNotFound) - `inst-resolve-3b`
   3. [x] - `p1` - **RETURN** resolved named plugin - `inst-resolve-3c`

### Plugin Validation Algorithm

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-plugin-validation`

**Input**: Plugin creation payload (name, plugin_type, config_schema, source_code)

**Output**: Validation result with errors array

**Steps**:
1. [ ] - `p1` - Validate name is non-empty, trimmed, and ≤ 255 characters - `inst-val-1`
2. [ ] - `p1` - Validate plugin_type is one of `auth`, `guard`, `transform` - `inst-val-2`
3. [ ] - `p1` - Validate config_schema is valid JSON Schema (draft 2020-12 or compatible) - `inst-val-3`
4. [ ] - `p1` - Parse Starlark source_code for syntax errors - `inst-val-4`
5. [ ] - `p1` - **IF** plugin_type is `guard` - `inst-val-5`
   1. [ ] - `p1` - Verify source defines `on_request(ctx)` function (required); check for optional `on_response(ctx)` - `inst-val-5a`
6. [ ] - `p1` - **IF** plugin_type is `transform` - `inst-val-6`
   1. [ ] - `p1` - Verify source defines at least one of `on_request(ctx)`, `on_response(ctx)`, `on_error(ctx)` - `inst-val-6a`
7. [ ] - `p1` - **IF** plugin_type is `auth` - `inst-val-7`
   1. [ ] - `p1` - Verify source defines `authenticate(ctx)` function - `inst-val-7a`
8. [ ] - `p1` - Scan source AST for forbidden operations (network I/O, file I/O, imports) - `inst-val-8`
9. [ ] - `p1` - **IF** any validation errors, **RETURN** { valid: false, errors } - `inst-val-9`
10. [ ] - `p1` - **RETURN** { valid: true, errors: [] } - `inst-val-10`

### Plugin In-Use Check Algorithm

- [ ] `p1` - **ID**: `cpt-cf-oagw-algo-plugin-in-use-check`

**Input**: Plugin UUID

**Output**: Boolean (in use) and usage locations

**Steps**:
1. [ ] - `p1` - DB: SELECT COUNT(*) FROM oagw_upstream WHERE auth_plugin_uuid = :plugin_uuid - `inst-inuse-1`
2. [ ] - `p1` - DB: SELECT COUNT(*) FROM oagw_upstream_plugin WHERE plugin_uuid = :plugin_uuid - `inst-inuse-2`
3. [ ] - `p1` - DB: SELECT COUNT(*) FROM oagw_route_plugin WHERE plugin_uuid = :plugin_uuid - `inst-inuse-3`
4. [ ] - `p1` - **IF** any count > 0, **RETURN** { in_use: true, upstream_auth: count1, upstream_bindings: count2, route_bindings: count3 } - `inst-inuse-4`
5. [ ] - `p1` - **RETURN** { in_use: false } - `inst-inuse-5`

### Plugin GC Marking Algorithm

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-plugin-gc-mark`

**Input**: Plugin UUID, event type (unlinked or re-linked)

**Output**: Updated `gc_eligible_at` timestamp or cleared

**Steps**:
1. [ ] - `p1` - **IF** event is "unlinked" - `inst-gcmark-1`
   1. [ ] - `p1` - Execute plugin in-use check (`cpt-cf-oagw-algo-plugin-in-use-check`) - `inst-gcmark-1a`
   2. [ ] - `p1` - **IF** plugin is still in use elsewhere, **RETURN** (no action) - `inst-gcmark-1b`
   3. [ ] - `p1` - DB: UPDATE oagw_plugin SET gc_eligible_at = NOW() + gc_ttl WHERE id = :uuid AND gc_eligible_at IS NULL - `inst-gcmark-1c`
2. [ ] - `p1` - **IF** event is "re-linked" - `inst-gcmark-2`
   1. [ ] - `p1` - DB: UPDATE oagw_plugin SET gc_eligible_at = NULL WHERE id = :uuid - `inst-gcmark-2a`

### Plugin GC Cleanup Algorithm

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-plugin-gc-cleanup`

**Input**: Current timestamp, GC TTL (default: 30 days)

**Output**: Count of deleted plugins

**Steps**:
1. [ ] - `p1` - DB: SELECT id FROM oagw_plugin WHERE gc_eligible_at IS NOT NULL AND gc_eligible_at <= :now - `inst-gcclean-1`
2. [ ] - `p1` - **FOR EACH** candidate plugin - `inst-gcclean-2`
   1. [ ] - `p1` - Re-check plugin in-use (`cpt-cf-oagw-algo-plugin-in-use-check`) to guard against race conditions - `inst-gcclean-2a`
   2. [ ] - `p1` - **IF** still not in use - `inst-gcclean-2b`
      1. [ ] - `p1` - DB: DELETE FROM oagw_plugin WHERE id = :uuid AND gc_eligible_at <= :now - `inst-gcclean-2b1`
   3. [ ] - `p1` - **ELSE** (re-linked since marking) - `inst-gcclean-2c`
      1. [ ] - `p1` - DB: UPDATE oagw_plugin SET gc_eligible_at = NULL WHERE id = :uuid - `inst-gcclean-2c1`
3. [ ] - `p1` - **RETURN** count of deleted plugins - `inst-gcclean-3`

## 4. States (CDSL)

### Custom Plugin Lifecycle State Machine

- [ ] `p2` - **ID**: `cpt-cf-oagw-state-plugin-lifecycle`

**States**: active, gc_eligible, deleted

**Initial State**: active

**Transitions**:
1. [ ] - `p1` - **FROM** active **TO** gc_eligible **WHEN** plugin becomes unlinked from all upstreams and routes (no references in `oagw_upstream.auth_plugin_uuid`, `oagw_upstream_plugin.plugin_uuid`, `oagw_route_plugin.plugin_uuid`) - `inst-state-1`
2. [ ] - `p1` - **FROM** gc_eligible **TO** active **WHEN** plugin is re-linked to an upstream or route before GC TTL expires - `inst-state-2`
3. [ ] - `p1` - **FROM** gc_eligible **TO** deleted **WHEN** `gc_eligible_at` is in the past AND plugin is still unreferenced (GC cleanup job) - `inst-state-3`
4. [ ] - `p1` - **FROM** active **TO** deleted **WHEN** operator explicitly deletes an unlinked plugin via DELETE /api/oagw/v1/plugins/{id} - `inst-state-4`

**Invalid Transitions**:
- **FROM** deleted **TO** any state — deleted is terminal; plugins cannot be restored after deletion or GC cleanup
- **FROM** active **TO** active — no self-transition; active state persists until an unlink or explicit delete event
- **FROM** gc_eligible **TO** gc_eligible — no re-marking; gc_eligible_at is set once and only cleared on re-link or expired by GC

**State Persistence**: The `active` vs `gc_eligible` distinction is derived from the `gc_eligible_at` column in `oagw_plugin` (NULL = active, non-NULL = gc_eligible). The `deleted` state corresponds to row removal.

## 5. Definitions of Done

### Implement Plugin Trait Definitions

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-plugin-traits`

The system **MUST** define `AuthPlugin`, `GuardPlugin`, and `TransformPlugin` async traits in `oagw-sdk`. Each trait **MUST** include `id()`, `plugin_type()`, and type-specific methods (`authenticate`, `guard_request`/`guard_response`, `transform_request`/`transform_response`/`transform_error`). `AuthPlugin` **MUST** also offer an optional `sign` phase, enabled by a `signs` capability flag that defaults to `false` ([ADR-0023](../ADR/0023-request-signing.md)). Traits **MUST** be `Send + Sync` for async runtime compatibility.

**Implements**:
- `cpt-cf-oagw-flow-plugin-bind`
- `cpt-cf-oagw-algo-plugin-ref-resolution`

**Touches**:
- Entities: `Plugin`

### Implement All 12 Built-in Plugins

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-builtin-plugins`

The system **MUST** implement 8 auth plugins (`noop`, `apikey`, `basic`, `bearer`, `oauth2_client_cred`, `oauth2_client_cred_basic`, `oauth2_client_cred_private_key_jwt`, `aws_sigv4`), 2 guard plugins (`timeout`, `cors`), and 3 transform plugins (`logging`, `metrics`, `request_id`). Each built-in **MUST** be registered in the in-process plugin registry using its full GTS identifier. Auth plugins **MUST** resolve credentials via `cred_store` secret references at request time without storing secret material.

**Implements**:
- `cpt-cf-oagw-flow-plugin-bind`
- `cpt-cf-oagw-algo-plugin-ref-resolution`

**Touches**:
- Entities: `Plugin`

### Implement Plugin CRUD REST Handlers

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-plugin-crud`

The system **MUST** implement REST handlers for `POST /api/oagw/v1/plugins` (create), `GET /api/oagw/v1/plugins` (list with OData), `GET /api/oagw/v1/plugins/{id}` (get by GTS ID), `DELETE /api/oagw/v1/plugins/{id}` (delete with in-use check), and `GET /api/oagw/v1/plugins/{id}/source` (get Starlark source). All operations **MUST** be tenant-scoped via secure ORM. Plugins **MUST** be immutable (no PUT endpoint). Delete **MUST** return `409 PluginInUse` when the plugin is referenced.

**Implements**:
- `cpt-cf-oagw-flow-plugin-create`
- `cpt-cf-oagw-flow-plugin-read`
- `cpt-cf-oagw-flow-plugin-delete`

**Touches**:
- API: `POST /api/oagw/v1/plugins`, `GET /api/oagw/v1/plugins`, `GET /api/oagw/v1/plugins/{id}`, `DELETE /api/oagw/v1/plugins/{id}`, `GET /api/oagw/v1/plugins/{id}/source`
- DB: `oagw_plugin`
- Entities: `Plugin`

### Implement Plugin Identification Model

- [ ] `p1` - **ID**: `cpt-cf-oagw-dod-plugin-identification`

The system **MUST** implement the dual identification model: named plugins resolved via in-process registry by full GTS identifier, custom plugins resolved via `oagw_plugin` by UUID. Plugin bindings **MUST** store `plugin_ref` (always) and `plugin_uuid` (only for UUID-backed). Auth plugin identity **MUST** be stored as scalar columns (`auth_plugin_ref`, `auth_plugin_uuid`) on `oagw_upstream`. Binding tables **MUST NOT** have FK to `oagw_plugin` (named plugins have no DB rows).

**Implements**:
- `cpt-cf-oagw-algo-plugin-ref-resolution`
- `cpt-cf-oagw-flow-plugin-bind`

**Touches**:
- DB: `oagw_upstream_plugin`, `oagw_route_plugin`, `oagw_upstream`
- Entities: `Plugin`, `Upstream`, `Route`

### Implement Starlark Sandbox

- [ ] `p3` - **ID**: `cpt-cf-oagw-dod-starlark-sandbox`

The system **MUST** execute custom Starlark plugins in a sandbox with no network I/O, no file I/O, no imports, enforced timeout (≤ 100ms per invocation), and enforced memory limit (≤ 10MB per invocation). Source validation **MUST** reject scripts that attempt forbidden operations. The sandbox **MUST** provide a `ctx` object with request/response access and `ctx.reject()`, `ctx.next()`, `ctx.log` helpers.

**Implements**:
- `cpt-cf-oagw-flow-plugin-create`
- `cpt-cf-oagw-algo-plugin-validation`

**Touches**:
- Entities: `Plugin`

### Implement Plugin Garbage Collection

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-plugin-gc`

The system **MUST** mark unlinked custom plugins with `gc_eligible_at = NOW() + TTL` (default: 30 days) when they become unreferenced. The system **MUST** clear `gc_eligible_at` when a plugin is re-linked. A periodic GC job **MUST** delete plugins where `gc_eligible_at` is in the past and the plugin is still unreferenced (double-check to guard against races).

**Implements**:
- `cpt-cf-oagw-algo-plugin-gc-mark`
- `cpt-cf-oagw-algo-plugin-gc-cleanup`
- `cpt-cf-oagw-state-plugin-lifecycle`

**Touches**:
- DB: `oagw_plugin`
- Entities: `Plugin`

## 6. Acceptance Criteria

- [ ] Custom Starlark plugin can be created via POST, listed via GET, retrieved by ID (including source), and deleted via DELETE
- [x] Named (built-in) plugins resolve via in-process registry by full GTS identifier without database lookup
- [ ] Custom (UUID-backed) plugins resolve via `oagw_plugin` database lookup with tenant scoping
- [ ] Plugin deletion returns `409 PluginInUse` when plugin is referenced by any upstream auth config, upstream binding, or route binding
- [ ] GC marks unlinked custom plugins with `gc_eligible_at` and periodic cleanup deletes them after TTL expiry
- [ ] Starlark sandbox enforces no network I/O, no file I/O, no imports, timeout ≤ 100ms, and memory ≤ 10MB per invocation
- [ ] All 11 built-in plugins (6 auth, 2 guard, 3 transform) register in the in-process registry and are resolvable by GTS identifier
- [ ] Plugin bindings validate type compatibility (auth plugins only on upstream scalar; guard/transform only in binding tables)
- [ ] Plugin binding positions are contiguous from 0 with no gaps, validated on write
- [x] Auth plugin credential references resolve via `cred_store` at request time; no secret material is stored or logged by OAGW
- [ ] Plugins are immutable after creation; no PUT endpoint exists for plugins
- [ ] All plugin operations are tenant-scoped via secure ORM

## 7. Additional Context

### Auth Plugin Credential Resolution

Auth plugins do not store credentials directly. At proxy request time, the auth plugin reads the `secret_ref` from `auth_config` (e.g., `cred://partner-openai-key`) and resolves it via `cred_store`. The `cred_store` gear enforces tenant visibility — OAGW receives secret material only if the current tenant has access. Every auth plugin's failures map the same way (DESIGN "Credential Failures"):

| Failure | Response |
|---|---|
| Secret missing or not accessible to the tenant | 401 `unauthenticated`, reason `AUTH_CREDENTIAL_NOT_FOUND` |
| Credential store unavailable | 503 `service_unavailable`, reason `CREDENTIAL_STORE_UNAVAILABLE`, the store's retry hint (default 5 s) |
| Identity provider unreachable, timing out or answering 408, 429 or 5xx | 503 `service_unavailable`, reason `IDENTITY_PROVIDER_UNAVAILABLE`, `retry_after_seconds: 5` |
| Identity provider rejects the client (400, 401, 403 to the token request) | 401, reason `AUTH_PLUGIN_FAILED` |
| Identity provider answers with an invalid response (discovery 400, 401 and 403 included; zero lifetime; unusable access token) | 502 `service_unavailable`, reason `REQUEST_NOT_SENT` |
| Plugin misconfiguration (`PluginError::InvalidConfig`, e.g. an endpoint scheme the HTTP client cannot use, or a `private_key_jwt` key that does not parse or does not match `signing_alg`) | 500 `internal` |
| Other plugin failure (`PluginError::Internal`, e.g. a secret value that is not UTF-8 or not a header value) | 401, reason `AUTH_PLUGIN_INTERNAL` |

Errors and logs name the auth config key, never the reference. These failures do not count toward the upstream's circuit breaker.

The `oauth2_client_cred_private_key_jwt` plugin reads its signing key the same way, from the `cred_store` secret under `private_key_ref` (PKCS#8 PEM), as the caller; the key never appears in the config, errors or logs. Its config (exactly one of `token_endpoint` and `issuer_url`, `private_key_ref`, `signing_alg` one of `ES256` / `RS256` / `PS256`, optional `kid`, optional `audience` for the assertion's `aud`, no `client_secret_ref`) is checked on upstream create and update ([ADR-0016](../ADR/0016-oauth2-client-credentials-auth-plugin.md)).

In `authenticate`, an auth plugin does not see the outbound headers: it returns the headers to set, and the proxy sets them on the request, each replacing the lines of that name. The client's other headers reach the upstream as sent, repeated and non-UTF-8 ones included. A header the plugin returns that is not a valid name or value fails the request.

### Auth Plugin Request Signing

An auth plugin can also sign the request ([ADR-0023](../ADR/0023-request-signing.md)). When its `signs` flag is `true`, its `sign` phase runs once per request, as the last step before the request is sent: after header rules, transforms, endpoint selection, the `Host` header, rate limits and the URL build, on the HTTP and WebSocket paths, and after the upstream engine's own header sanitization, so it sees only header lines the engine sends unchanged. It gets the method, the URL parts (scheme, host, port, path and query as sent), every header line as sent (repeated names keep all lines, in order; values as bytes), the body (the buffered bytes, a streamed marker, or an empty body for a WebSocket handshake), the upstream's `auth.config` and the caller's security context. It returns headers to set, each replacing the lines of that name and marked sensitive, and a query change: keep (the default), remove, or replace with a query sent exactly as returned. Headers the gateway sets or strips itself (`Host`, `Content-Length`, hop-by-hop, forwarding, `X-OAGW-*`) or the engine drops (`Expect`, `Proxy-Connection`, `HTTP2-Settings`) are refused, and so is a name set twice. A signing error or an output the gateway cannot send stops the request before anything reaches the upstream, with the same mapping as an `authenticate` error (see the table above); an output the gateway cannot send is a plugin misconfiguration (500 `internal`). When the config is valid but the request as sent cannot be signed (`PluginError::InvalidRequest`), the request fails with 400 `VALIDATION` on the field the plugin names. Of the built-in plugins only `aws_sigv4` signs; `noop`, `apikey`, `oauth2_client_cred`, `oauth2_client_cred_basic` and `oauth2_client_cred_private_key_jwt` keep the default `signs` of `false` (the `private_key_jwt` plugin signs its token request, not the proxied request), so the phase is skipped for them. `request_ref` mode runs no auth plugin and therefore no signing.

### AWS SigV4 Auth Plugin

`gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1` signs requests to AWS APIs (Bedrock, S3, SQS, API Gateway and others) with [Signature Version 4](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_sigv.html) and IAM access keys ([#5842](https://github.com/constructorfabric/gears-rust/issues/5842)). `authenticate` only checks the config; the request is signed in the signing phase, on the request as sent.

| `auth.config` key | Required | Meaning |
|---|---|---|
| `region` | yes | Signing region, e.g. `us-east-1`. 1 to 64 ASCII letters, digits, `-`, `_` or `.`: AWS regions are lowercase, S3-compatible servers may use names such as `RegionOne`. |
| `service` | yes | Signing name of the service, e.g. `bedrock` (also for `bedrock-runtime` endpoints), `s3`, `s3express`, `execute-api`. Same format as `region`. |
| `access_key_id_ref` | yes | CredStore reference of the access key id. |
| `secret_access_key_ref` | yes | CredStore reference of the secret access key. |
| `session_token_ref` | no | CredStore reference of the session token of temporary credentials; sent as `X-Amz-Security-Token` and signed. |
| `unsigned_payload` | no, `false` | `true` keeps a request body with a `Content-Length` streamed and sends it as `UNSIGNED-PAYLOAD` (a body without one is still read whole, see **Body** below); with `false` the gateway reads the body whole (within `max_body_size_bytes`) to hash it. A buffered body is always hashed. |
| `uri_encoding` | no, derived | `single` signs the path as sent, without normalization, decoded and encoded once in SigV4 encoding as S3 checks it, so `:` or `(` a client sends unencoded are signed as `%3A`, `%28` (Amazon S3); `double` removes `.`/`..` segments and repeated slashes and encodes the path again (every other service). Default: `single` for the S3 signing names `s3`, `s3-object-lambda`, `s3-outposts` and `s3express`, matched exactly (the names the S3 and S3 Control endpoint rules sign without double encoding), else `double`. |
| `content_sha256_header` | no, `true` | Send the payload hash as `X-Amz-Content-Sha256`. With `false` the header is not added, but a client's one is still replaced with the real hash. |

- **Writes** check the config with the same parser the plugin uses: a missing or malformed key is 400 naming `auth.config.<key>`. The three references are in `CREDENTIAL_KEYS`, so each one present gets the format and access checks of every credential reference (400 `failed_precondition` naming the key when it does not resolve for the caller).
- **Credentials** are read from CredStore as the caller on every signed request, the references concurrently, behind an internal credential source, so temporary credentials from the runtime's identity ([#5844](https://github.com/constructorfabric/gears-rust/issues/5844)) can plug in later. The access key id must be ASCII letters and digits and the session token visible ASCII. Failures follow the credential-failure table above: a missing or inaccessible secret is 401 `AUTH_CREDENTIAL_NOT_FOUND`, a credential store outage 503 `CREDENTIAL_STORE_UNAVAILABLE`, a stored value that is not an access key id, an empty secret key or a session token with other characters 401 `AUTH_PLUGIN_INTERNAL`, and a config the plugin cannot parse at proxy time (one written before a check existed) 500 `internal`. Errors name the config key, never the reference or a value. The secret key and the derived signing key are zeroized after use and never logged.
- **Signed headers**: `host`, every `x-amz-*`, `content-type` and `content-md5`, each with every line it is sent with. Never `cookie`, `content-length`, `user-agent`, hop-by-hop or other fields the engine rewrites after signing ([ADR-0023](../ADR/0023-request-signing.md#what-the-plugin-sees)).
- **Headers set**: `Authorization`, `X-Amz-Date`, `X-Amz-Content-Sha256` (see above) and, with a session token, `X-Amz-Security-Token`. They replace any client line of the same name. A signing phase cannot remove a header, so a client's `X-Amz-Security-Token` on an upstream without `session_token_ref` fails the request with 400 `VALIDATION` on field `x-amz-security-token`; drop it with `headers.request.remove`. Other client `x-amz-*` headers (for example `X-Amz-Target`) are signed as sent. Client `Authorization` is never forwarded.
- **Query**: the outbound query is the client's as received, or form-encoded by the gateway when a transform changed it or it holds a literal `;`. The plugin decodes it as form-urlencoded (`+` for a space), as the allowlist reads it, and rewrites it in SigV4 encoding (`%20` for a space, `%2B` for a plus, `%2A`, unencoded `~`), keeping the order, so the upstream receives exactly what was signed; the canonical query is the same pairs sorted. The query is replaced only when the encoding differs.
- **Body**: a buffered body is hashed with SHA-256, whatever `unsigned_payload` says; on a multi-threaded runtime a body over 64 KiB is hashed under `block_in_place`, so the executor thread's other tasks move to another worker meanwhile. Request bodies stream from the caller (REST and SDK); without `unsigned_payload` the plugin's `signs_body` asks the gateway to read the body whole before signing, so it is hashed and sent with `Content-Length`. With `unsigned_payload` a body with a `Content-Length` stays streamed, with that length, and is signed as `UNSIGNED-PAYLOAD`. One without a length (a chunked client) is still read whole, within `max_body_size_bytes`, and sent with `Content-Length`: S3 refuses a chunked upload unless it carries `x-amz-decoded-content-length`, which the gateway does not set. A WebSocket handshake is signed with an empty payload.
- **Circuit breaker**: the CredStore reads in the signing phase are the gateway's time, not the upstream's: a deadline hit after signing used more than a tenth of the request timeout does not count against the upstream ([ADR-0005](../ADR/0005-circuit-breaker.md)).
- **Time**: `X-Amz-Date` and the credential scope date are taken from the system clock when the request is signed.
- **Crypto**: every hash and HMAC goes through `toolkit-security::crypto::sigv4`; the canonical request, encoding and header selection are the plugin's.
- **Rewritten paths**: the gateway normalizes the request path before route matching: it collapses `//`, resolves `.`/`..` and drops a trailing `/`. S3 does not normalize, so with `single` encoding a request whose path the gateway rewrote would sign and address another object key: it is refused with `400 VALIDATION` on field `path`, before any credential is read ([negative-9.14](../../scenarios/proxy-api/authentication/negative-9.14-aws-sigv4-s3-rewritten-path.md)). The trailing `/` is compared with the URL as built: one the route path gives back (route `/dir/`, request `/dir/`) is not a rewrite, one it adds (route `/dir/`, request `/dir`) is. With `double` encoding the normalized path is signed: the service normalizes the same way.
- **Out of scope**: presigned URLs, SigV4a, S3 `aws-chunked` payload signing (streaming signatures: a large S3 `PutObject` is read whole to be hashed, or sent as `UNSIGNED-PAYLOAD`), S3 object keys with repeated slashes, dot segments or a trailing `/` (refused, see above).

### Scope Boundaries

- **In scope**: Plugin trait definitions, built-in implementations, CRUD REST handlers, identification model, Starlark sandbox, GC lifecycle
- **Out of scope**: Plugin chain execution order during proxy request (Feature 4: `cpt-cf-oagw-feature-proxy-engine`), Starlark standard library extensions (future work)

### Performance Considerations

Not applicable for management-plane plugin CRUD. Plugin execution performance (hot path) is addressed by Feature 4. Starlark sandbox timeout (≤ 100ms) prevents plugin execution from degrading proxy latency.

### Usability Considerations

Not applicable — this is a backend API feature with no direct user interface.

### Compliance Considerations

Not applicable — no regulatory requirements specific to plugin management beyond tenant isolation (covered by secure ORM).

### Audit Trail

Plugin create and delete operations are auditable actions. The OAGW audit trail (inherited from ToolKit) records:
- **Action type**: create, delete, bind, unbind
- **Actor identity**: tenant_id and user_id from SecurityContext
- **Resource identifier**: plugin GTS identifier
- **Timestamp**: UTC, server-clock
- **Outcome**: success or rejection reason (e.g., PluginInUse, ValidationError)

Secret material is never included in audit logs. Plugin source code is not logged — only plugin metadata (id, name, type).

### Observability

Plugin operations emit the following observability signals:
- **Logging**: Plugin CRUD operations logged at INFO level with plugin_id, tenant_id, action. Validation failures logged at WARN with error details. GC cleanup logged at INFO with deleted count.
- **Metrics**: `oagw_plugin_total` (gauge, by plugin_type), `oagw_plugin_gc_cleaned_total` (counter), `oagw_plugin_crud_duration_seconds` (histogram, by operation)
- **Tracing**: Plugin CRUD operations carry the request correlation_id from ToolKit middleware. GC job runs emit their own trace span.

### Fault Tolerance

- **Database failures**: Standard ToolKit error propagation applies. Plugin CRUD operations are short-lived transactions with no external side effects beyond DB writes. Transient DB failures return `503 Service Unavailable`.
- **cred_store unavailability**: Not applicable for plugin CRUD. Auth plugin credential resolution happens at proxy request time (Feature 4: `cpt-cf-oagw-feature-proxy-engine`), not during plugin management. Plugin binding validation does not resolve secrets. Upstream writes do check that the credential references of the auth config (`secret_ref`, `client_id_ref`, `client_secret_ref`) are readable in the credential store (best effort, see Feature 2).
- **Starlark sandbox failures**: Timeout or memory exhaustion during source validation returns a `400 ValidationError` at creation time. Sandbox execution failures at proxy time are addressed by Feature 4.
- **GC job failures**: GC cleanup is idempotent. If the periodic job fails mid-run, the next invocation picks up remaining candidates. The double-check (re-verify in-use before delete) guards against race conditions.

### Component Interactions

- **SecurityContext**: Extracted by ToolKit middleware on every request. Provides tenant_id for DB scoping and permissions for authorization checks.
- **types_registry** (`cpt-cf-oagw-actor-types-registry`): Plugin type schemas are validated against GTS at creation time. The types_registry is queried synchronously as an in-process call (no network hop).
- **cred_store** (`cpt-cf-oagw-actor-cred-store`): Interacted with only at proxy time (Feature 4). Not called during plugin CRUD or binding operations.
- **Async operations**: All plugin CRUD handlers are standard async request-response (no background tasks spawned). The GC cleanup job runs as a periodic async task on the server's runtime.

### Configuration Parameters

| Parameter | Default | Validation | Description |
|-----------|---------|------------|-------------|
| `oagw.plugin.gc_ttl` | `30d` | ≥ 1h | Time before unreferenced custom plugins are eligible for GC cleanup |
| `oagw.plugin.gc_interval` | `1h` | ≥ 1m | Interval between GC cleanup job runs |
| `oagw.plugin.starlark.timeout_ms` | `100` | 1–10000 | Maximum Starlark execution time per invocation (ms) |
| `oagw.plugin.starlark.memory_mb` | `10` | 1–256 | Maximum Starlark memory per invocation (MB) |

No spec flags are defined for this feature. All plugin functionality is always-on when the OAGW gear is enabled.

### Known Limitations

- Custom plugins are Starlark-only; Rust-based custom plugins require compilation into the server binary and are not dynamically loadable (ADR-0003 §7.2)
- No hot-reloading of built-in plugins; server restart required for built-in plugin registry changes
- Plugin versioning is append-only — operators create new plugins rather than updating existing ones (immutability principle)
- No plugin marketplace or sharing across tenants; each custom plugin is scoped to a single tenant

### Test Guidance

- **Unit tests**: Plugin validation algorithm (valid/invalid source, config_schema), ref resolution algorithm (named vs UUID, type mismatch), in-use check (all three binding locations), GC marking/cleanup (mark, re-link clear, TTL expiry, race condition guard)
- **Integration tests**: Plugin CRUD REST handlers with tenant isolation (multi-tenant scenarios), plugin binding with upstream/route (type validation, position contiguity), OData query support on list endpoint
- **E2E tests**: Full plugin lifecycle — create custom plugin → bind to upstream → unbind → verify GC marking → wait/simulate TTL → verify GC cleanup

### Rollout & Rollback

Not applicable for initial implementation. Plugin immutability simplifies rollback — operators create new plugin versions rather than mutating existing ones.
