<!-- Created: 2026-04-07 by Constructor Tech -->

# Feature: Integration Read Port & Dual Authentication Modes

- [x] `p1` - **ID**: `cpt-cf-resource-group-featstatus-integration-auth`

- [x] `p1` - `cpt-cf-resource-group-feature-integration-auth`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [JWT Request to RG REST API](#jwt-request-to-rg-rest-api)
  - [MTLS Request from AuthZ Plugin](#mtls-request-from-authz-plugin)
  - [Plugin Gateway Routing](#plugin-gateway-routing)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Tenant Scope Enforcement for Ownership-Graph Writes](#tenant-scope-enforcement-for-ownership-graph-writes)
  - [Authentication Mode Decision](#authentication-mode-decision)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Integration Read Service](#integration-read-service)
  - [Dual Authentication Mode Routing](#dual-authentication-mode-routing)
  - [Tenant Scope Enforcement for Ownership-Graph Profile](#tenant-scope-enforcement-for-ownership-graph-profile)
  - [Unit Test Coverage for Integration Auth](#unit-test-coverage-for-integration-auth)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
  - [Auth Mode Decision](#auth-mode-decision)
  - [Tenant Scope Enforcement](#tenant-scope-enforcement)
  - [Integration Read Service](#integration-read-service-1)
- [8. E2E Test Plan](#8-e2e-test-plan)
  - [S3: `test_authz_tenant_filter_applied`](#s3-test_authz_tenant_filter_applied)
  - [S4: `test_cross_tenant_invisible`](#s4-test_cross_tenant_invisible)
  - [Acceptance Criteria (S3, S4)](#acceptance-criteria-s3-s4)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Expose the integration read service (`ResourceGroupReadHierarchy`) for external consumers such as the AuthZ plugin, implement dual authentication modes (JWT with full AuthZ evaluation, MTLS with hierarchy-only bypass for AuthZ plugin), enforce tenant scope for ownership-graph profile, configure plugin gateway routing for vendor-specific providers, and store barrier as data in group metadata without enforcement.

### 1.2 Purpose

This feature bridges RG with the AuthZ ecosystem. The integration read port provides a stable, policy-agnostic data contract for hierarchy reads. Dual auth modes resolve the circular dependency between RG (needs AuthZ for its own endpoints) and AuthZ (needs RG for hierarchy data). Tenant scope enforcement ensures ownership-graph integrity for AuthZ-facing deployments.

**Requirements**: `cpt-cf-resource-group-fr-integration-read-port`, `cpt-cf-resource-group-fr-dual-auth-modes`, `cpt-cf-resource-group-fr-tenant-scope-ownership-graph`

**Principles**: `cpt-cf-resource-group-principle-tenant-scope-ownership-graph`, `cpt-cf-resource-group-principle-barrier-as-data`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-resource-group-actor-authz-plugin-consumer` | Reads hierarchy data via `ResourceGroupReadHierarchy` (MTLS or in-process ClientHub) |
| `cpt-cf-resource-group-actor-instance-administrator` | Configures MTLS settings, manages tenant hierarchy |
| `cpt-cf-resource-group-actor-tenant-administrator` | Operates within tenant scope; JWT-authenticated requests go through AuthZ |
| `cpt-cf-resource-group-actor-apps` | General consumers using `ResourceGroupClient` via JWT |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — sections 5.7, 5.9, 3.3, 3.4
- **Design**: [DESIGN.md](../DESIGN.md) — sections 3.2 (Integration Read Service), 3.3 (API Contracts, Integration Read), 3.6 (sequences: authz-rg-sql-split, auth-modes, mtls-authz-read, jwt-rg-request, e2e-authz-flow)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.5
- **Dependencies**: Features 0003, 0004 — hierarchy data, membership data
- **Not applicable**: UX (backend API — no user interface); COMPL (internal platform module — no regulatory data handling); OPS observability and rollout are managed at the module infrastructure level (DESIGN §3.7 and platform runbooks); PERF targets are set at the system level in PRD.md NFR section.

## 2. Actor Flows (CDSL)

### JWT Request to RG REST API

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-integration-auth-jwt-request`

**Actor**: `cpt-cf-resource-group-actor-tenant-administrator`

**Success Scenarios**:
- User/service request authenticated via JWT, AuthZ evaluated via PolicyEnforcer, AccessScope applied to query, results returned

**Error Scenarios**:
- Invalid JWT → 401 Unauthorized
- Insufficient permissions → 403 Forbidden
- AuthZ service unavailable → 503

**Steps**:
1. [x] - `p1` - Actor sends request to any RG REST endpoint with JWT bearer token - `inst-jwt-1`
2. [x] - `p1` - API Gateway: authenticate JWT via AuthNResolverClient → SecurityContext {subject_id, subject_tenant_id} - `inst-jwt-2`
3. [x] - `p1` - RG Gateway: call PolicyEnforcer.access_scope(ctx, resource_type, action) - `inst-jwt-3`
4. [x] - `p1` - PolicyEnforcer → AuthZ Resolver: evaluate(EvaluationRequest) - `inst-jwt-4`
5. [x] - `p1` - AuthZ plugin internally: call ResourceGroupReadHierarchy.get_group_descendants() for tenant hierarchy resolution (via MTLS or in-process ClientHub — bypasses AuthZ) - `inst-jwt-5`
6. [x] - `p1` - AuthZ plugin: produce constraints (e.g., owner_tenant_id IN (...)) - `inst-jwt-6`
7. [x] - `p1` - PolicyEnforcer: compile_to_access_scope() → AccessScope - `inst-jwt-7`
8. [x] - `p1` - RG Gateway: apply AccessScope via SecureORM (WHERE tenant_id IN (...)) to query - `inst-jwt-8`
9. [x] - `p1` - RG Service: execute query with SQL predicates, return results - `inst-jwt-9`
10. [x] - `p1` - **RETURN** response to actor - `inst-jwt-10`

### MTLS Request from AuthZ Plugin

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-integration-auth-mtls-request`

**Actor**: `cpt-cf-resource-group-actor-authz-plugin-consumer`

**Success Scenarios**:
- AuthZ plugin reads hierarchy data via MTLS-authenticated request, AuthZ evaluation bypassed

**Error Scenarios**:
- Invalid client certificate → 403 Forbidden
- Client CN not in allowed_clients → 403 Forbidden
- Endpoint not in MTLS allowlist → 403 Forbidden

**Steps**:
1. [x] - `p1` - AuthZ plugin sends GET /api/resource-group/v1/groups/{group_id}/descendants (or /ancestors) with MTLS client certificate - `inst-mtls-1`
2. [x] - `p1` - RG Gateway: extract client certificate from TLS handshake - `inst-mtls-2`
3. [x] - `p1` - Validate certificate against trusted CA bundle (ca_cert): chain, expiration, revocation - `inst-mtls-3`
4. [x] - `p1` - Match client identity (certificate CN/SAN) against allowed_clients list - `inst-mtls-4`
5. [x] - `p1` - **IF** client not in allowed_clients → **RETURN** 403 Forbidden - `inst-mtls-5`
6. [x] - `p1` - Check endpoint against allowed_endpoints allowlist (method + path) - `inst-mtls-6`
7. [x] - `p1` - **IF** endpoint not in allowlist → **RETURN** 403 Forbidden - `inst-mtls-7`
8. [x] - `p1` - Create system SecurityContext (no AuthZ evaluation — trusted system principal) - `inst-mtls-8`
9. [x] - `p1` - RG Hierarchy Service: execute get_group_descendants(system_ctx, group_id, query) / get_group_ancestors(system_ctx, group_id, query) directly - `inst-mtls-9`
10. [x] - `p1` - **RETURN** Page<ResourceGroupWithDepth> — hierarchy data with tenant_id per group, metadata including barrier - `inst-mtls-10`

### Plugin Gateway Routing

- [x] `p1` - **ID**: `cpt-cf-resource-group-flow-integration-auth-plugin-routing`

**Actor**: `cpt-cf-resource-group-actor-authz-plugin-consumer`

**Success Scenarios**:
- Read request routed to built-in provider or vendor-specific plugin based on configuration

**Steps**:
1. [x] - `p1` - Integration read request arrives via ResourceGroupReadHierarchy trait - `inst-plugin-1`
2. [x] - `p1` - RG Module resolves configured provider from module config - `inst-plugin-2`
3. [x] - `p1` - **IF** built-in provider configured - `inst-plugin-3`
   1. [x] - `p1` - Route to local persistence path: execute query against RG database - `inst-plugin-3a`
4. [x] - `p1` - **IF** vendor-specific provider configured - `inst-plugin-4`
   1. [x] - `p1` - Resolve plugin instance by configured vendor via types-registry (scoped by GTS instance ID) - `inst-plugin-4a`
   2. [x] - `p1` - Delegate to ResourceGroupReadPluginClient with SecurityContext passthrough - `inst-plugin-4b`
5. [x] - `p1` - **RETURN** results from selected provider - `inst-plugin-5`

## 3. Processes / Business Logic (CDSL)

### Tenant Scope Enforcement for Ownership-Graph Writes

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement`

**Input**: Write operation context (create/move group or add membership), caller SecurityContext, target group/parent tenant_id

**Output**: Pass or TenantIncompatibility

**Steps**:
1. [x] - `p1` - Extract caller effective tenant scope from SecurityContext.subject_tenant_id - `inst-tenant-enforce-1`
2. [x] - `p1` - **IF** caller is privileged platform-admin (provisioning exception) → **RETURN** pass (but data invariants still checked) - `inst-tenant-enforce-2`
3. [x] - `p1` - **IF** parent-child edge: validate parent and child are in same tenant or related via configured tenant hierarchy scope - `inst-tenant-enforce-3`
4. [x] - `p1` - **IF** membership write: validate target group's tenant_id is compatible with caller's effective tenant scope - `inst-tenant-enforce-4`
5. [x] - `p1` - **IF** tenant-incompatible → **RETURN** TenantIncompatibility with tenant details - `inst-tenant-enforce-5`
6. [x] - `p1` - **RETURN** pass - `inst-tenant-enforce-6`

### Authentication Mode Decision

- [x] `p1` - **ID**: `cpt-cf-resource-group-algo-integration-auth-auth-mode-decision`

**Input**: Incoming request with authentication credentials

**Output**: Authentication mode (JWT or MTLS) and resulting SecurityContext

**Steps**:
1. [x] - `p1` - Inspect request for authentication method - `inst-auth-decide-1`
2. [x] - `p1` - **IF** request has MTLS client certificate - `inst-auth-decide-2`
   1. [x] - `p1` - Verify certificate against CA bundle - `inst-auth-decide-2a`
   2. [x] - `p1` - Match CN against allowed_clients - `inst-auth-decide-2b`
   3. [x] - `p1` - Check endpoint in MTLS allowlist - `inst-auth-decide-2c`
   4. [x] - `p1` - **IF** all checks pass → create system SecurityContext, skip AuthZ → **RETURN** MTLS mode - `inst-auth-decide-2d`
   5. [x] - `p1` - **ELSE** → **RETURN** 403 Forbidden - `inst-auth-decide-2e`
3. [x] - `p1` - **IF** request has JWT bearer token - `inst-auth-decide-3`
   1. [x] - `p1` - Authenticate via AuthNResolverClient → SecurityContext - `inst-auth-decide-3a`
   2. [x] - `p1` - Run PolicyEnforcer.access_scope() → AccessScope - `inst-auth-decide-3b`
   3. [x] - `p1` - **RETURN** JWT mode with SecurityContext + AccessScope - `inst-auth-decide-3c`
4. [x] - `p1` - **ELSE** → **RETURN** 401 Unauthorized - `inst-auth-decide-4`

## 4. States (CDSL)

Not applicable. This feature configures authentication routing and integration read service wiring. There are no entity lifecycle states — authentication mode is determined per-request, not via state transitions.

## 5. Definitions of Done

### Integration Read Service

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-integration-auth-read-service`

The system **MUST** implement an Integration Read Service that exposes `ResourceGroupReadHierarchy` via ClientHub for external consumers.

**Required behavior**:
- Expose `get_group_descendants(ctx, group_id, query)` and `get_group_ancestors(ctx, group_id, query)` returning `Page<ResourceGroupWithDepth>` with hierarchy data including `tenant_id` per group and `metadata` (including `barrier` for applicable types)
- Responses are policy-agnostic: no AuthZ decisions, no SQL fragments, no constraint objects
- Plugin gateway routing: resolve configured provider (built-in vs vendor-specific), delegate with SecurityContext passthrough
- In-process mode (monolith): direct ClientHub call, no network auth needed
- Out-of-process mode (microservices): MTLS-authenticated remote call
- SecurityContext propagated without policy interpretation across gateway layer

**Implements**:
- `cpt-cf-resource-group-flow-integration-auth-plugin-routing`

**Touches**:
- Entities: `ResourceGroupWithDepth`, `ResourceGroupMembership`

### Dual Authentication Mode Routing

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-integration-auth-dual-auth`

The system **MUST** implement dual authentication mode routing in the RG gateway.

**JWT mode (all endpoints)**:
- Authenticate via AuthNResolverClient → SecurityContext
- Run PolicyEnforcer.access_scope() for AuthZ evaluation
- Apply AccessScope via SecureORM to all queries
- Identical flow to any other domain service (courses, users, etc.)

**MTLS mode (hierarchy endpoints only)**:
- Verify client certificate against trusted CA bundle
- Match client CN/SAN against `allowed_clients` configuration
- Check endpoint against `allowed_endpoints` allowlist (`GET /groups/{group_id}/descendants` and `GET /groups/{group_id}/ancestors`)
- All other endpoints return 403 Forbidden in MTLS mode
- Bypass AuthZ evaluation entirely — trusted system principal
- Create system SecurityContext for RG service call

**MTLS configuration**:
- `ca_cert`: path to trusted CA bundle
- `allowed_clients`: list of allowed client CNs (e.g., `authz-resolver-plugin`)
- `allowed_endpoints`: list of method+path pairs (e.g., `GET /api/resource-group/v1/groups/{group_id}/descendants`, `GET /api/resource-group/v1/groups/{group_id}/ancestors`)

**Implements**:
- `cpt-cf-resource-group-flow-integration-auth-jwt-request`
- `cpt-cf-resource-group-flow-integration-auth-mtls-request`
- `cpt-cf-resource-group-algo-integration-auth-auth-mode-decision`

**Touches**:
- API: `GET /api/resource-group/v1/groups/{group_id}/descendants`, `GET /api/resource-group/v1/groups/{group_id}/ancestors` (JWT + MTLS), all other endpoints (JWT only)

### Tenant Scope Enforcement for Ownership-Graph Profile

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-integration-auth-tenant-scope`

The system **MUST** enforce tenant-hierarchy-compatible writes in ownership-graph profile.

**Required behavior**:
- Parent-child edges validated for tenant compatibility (same tenant or allowed related-tenant link)
- Membership writes validated against target group's tenant scope
- Platform-admin provisioning exception: privileged calls may bypass caller tenant scoping for cross-tenant management, but data invariants (parent-child type compat, tenant hierarchy rules) remain strict
- Tenant-scoped reads: in AuthZ query path, `SecurityContext.subject_tenant_id` determines effective tenant scope
- Barrier as data: `metadata.barrier` stored in group metadata JSONB, returned in API responses within `metadata` object. RG does not filter, restrict, or alter query results based on barrier value.

**Implements**:
- `cpt-cf-resource-group-algo-integration-auth-tenant-scope-enforcement`

**Touches**:
- DB: `resource_group` (tenant_id validation, metadata.barrier storage)

### Unit Test Coverage for Integration Auth

- [x] `p1` - **ID**: `cpt-cf-resource-group-dod-testing-integration-auth`

In-source `#[cfg(test)]` tests covering auth-mode decision and tenant-scope enforcement:
- Auth mode decision: JWT path dispatches to PolicyEnforcer; MTLS path bypasses AuthZ with system SecurityContext
- MTLS validation: valid CN in allowed_clients passes; unknown CN returns 403; expired certificate returns 403; endpoint not in allowlist returns 403
- Tenant-scope enforcement: compatible tenant passes; incompatible tenant returns TenantIncompatibility; platform-admin provisioning exception bypasses caller scope but enforces data invariants
- Integration read service: policy-agnostic response (no AuthZ fields in output); plugin gateway routes to built-in vs vendor-specific provider

## 6. Acceptance Criteria

- [x] AuthZ plugin resolves `dyn ResourceGroupReadHierarchy` from ClientHub and successfully calls `get_group_descendants` / `get_group_ancestors`
- [x] Integration read responses include `tenant_id` per group and `metadata` (including `barrier`) but no AuthZ decision fields
- [x] JWT request to any RG endpoint goes through AuthN → AuthZ (PolicyEnforcer) → AccessScope → SecureORM pipeline
- [x] MTLS request to `/groups/{group_id}/descendants` or `/groups/{group_id}/ancestors` bypasses AuthZ and returns hierarchy data
- [x] MTLS request to any other endpoint (e.g., `POST /groups`) returns 403 Forbidden
- [x] MTLS request with invalid certificate returns 403 Forbidden
- [x] MTLS request with valid certificate but client CN not in allowed_clients returns 403 Forbidden
- [x] Plugin gateway routes to built-in provider by default; routes to vendor-specific plugin when configured
- [x] SecurityContext is passed through gateway to provider without policy interpretation
- [x] Parent-child edge in ownership-graph profile with incompatible tenants is rejected with TenantIncompatibility
- [x] Platform-admin provisioning call bypasses caller tenant scoping but still validates data invariants
- [x] Group with `metadata.barrier = true` is stored and returned in API responses — RG does not filter based on barrier
- [x] In monolith deployment, AuthZ plugin uses ClientHub direct call (no MTLS needed)
- [x] In microservice deployment, AuthZ plugin uses MTLS-authenticated remote call to hierarchy endpoint

---

## 7. Unit Test Plan

### Auth Mode Decision

| TC | Scenario | Assert |
|----|----------|--------|
| TC-AUTH-01 | Request with valid MTLS cert + CN in allowed_clients + endpoint in allowlist | system SecurityContext created, AuthZ bypassed |
| TC-AUTH-02 | Request with valid MTLS cert but CN not in allowed_clients | Returns 403 Forbidden |
| TC-AUTH-03 | Request with expired MTLS certificate | Returns 403 Forbidden |
| TC-AUTH-04 | MTLS request to endpoint not in allowed_endpoints | Returns 403 Forbidden |
| TC-AUTH-05 | Request with JWT bearer token | AuthNResolverClient called, PolicyEnforcer evaluated |
| TC-AUTH-06 | Request with no credentials | Returns 401 Unauthorized |

### Tenant Scope Enforcement

| TC | Scenario | Assert |
|----|----------|--------|
| TC-TENANT-01 | Write with parent and child in same tenant | Passes |
| TC-TENANT-02 | Write with parent in tenant A, child in tenant B (incompatible) | Returns TenantIncompatibility |
| TC-TENANT-03 | Platform-admin provisioning call (cross-tenant) | Bypasses caller scope; data invariants still enforced |
| TC-TENANT-04 | Membership write: group and resource in compatible tenant | Passes |
| TC-TENANT-05 | Membership write: tenant mismatch | Returns TenantIncompatibility |

### Integration Read Service

| TC | Scenario | Assert |
|----|----------|--------|
| TC-READ-01 | `get_group_descendants` / `get_group_ancestors` response | Contains `tenant_id` and `metadata` per group; no AuthZ decision fields |
| TC-READ-02 | Plugin gateway with built-in provider configured | Routes to local persistence path |
| TC-READ-03 | Plugin gateway with vendor-specific provider configured | Delegates to `ResourceGroupReadPluginClient` |

---

## 8. E2E Test Plan

> General E2E testing philosophy, patterns, and infrastructure: [`docs/modkit_unified_system/13_e2e_testing.md`](../../../../../docs/modkit_unified_system/13_e2e_testing.md).

Tests S3 and S4 verify the real AuthZ wiring in `module.rs` that unit tests cannot reach: `authz_integration_test.rs` mocks the PDP, `tenant_filtering_db_test.rs` constructs `AccessScope` manually — neither exercises the live `PolicyEnforcer` → `SecureORM` pipeline.

### S3: `test_authz_tenant_filter_applied`

**Seam**: AuthZ → SecureORM full chain — SecurityContext → PolicyEnforcer → AccessScope → `WHERE tenant_id IN (...)`.

**Why not in unit tests**: Unit tests mock the PDP or pass a manually constructed `AccessScope` directly to the repo. Neither verifies the real wiring in `module.rs` where `PolicyEnforcer` is created from `ClientHub` and injected into `GroupService`.

```
POST /groups {name: "AuthZ Test"}       → 201, note tenant_id from response
GET  /groups                            → 200
  assert created group appears in list   (tenant filter allows own groups)
GET  /groups/{id}                       → 200
  assert tenant_id matches              (single-entity fetch also scoped)
```

Positive-only test. Cross-tenant negative testing is in S4.

### S4: `test_cross_tenant_invisible`

**Seam**: AuthZ → SecureORM negative — tenant boundary enforced, existence hidden across tenants.

**Why not in unit tests**: `tenant_filtering_db_test.rs` creates two `AccessScope` objects manually on SQLite. E2E uses two real HTTP tokens producing different SecurityContexts, exercising the full authn → authz → scope → SQL chain on PostgreSQL.

> **Skip if** `E2E_AUTH_TOKEN_TENANT_B` not set.

```
[Token A] POST /groups              → 201, group_id
[Token B] GET  /groups/{group_id}   → 404 (not 403 — hides existence)
[Token B] GET  /groups              → 200, group_id NOT in items
[Token A] GET  /groups/{group_id}   → 200 (still visible to owner)
```

### Acceptance Criteria (S3, S4)

- [x] S3 verifies own data is visible through the real `PolicyEnforcer` + real DB pipeline
- [x] S4 uses two real HTTP tokens and verifies tenant boundary hides existence (404 not 403)
- [x] S4 skips gracefully when `E2E_AUTH_TOKEN_TENANT_B` is not set
