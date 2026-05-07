<!-- Created: 2026-04-20 by Constructor Tech -->

# Feature: Backend Router and Roster

- [ ] `p1` - **ID**: `cpt-cf-file-storage-featstatus-backend-router-and-roster`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-file-storage-feature-backend-router-and-roster`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [List Backends Flow](#list-backends-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Backend Resolve](#backend-resolve)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Backend Registry](#implement-backend-registry)
  - [Implement Resolve and Tenant Access Enforcement](#implement-resolve-and-tenant-access-enforcement)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Resolve a `backend_id` (or the tenant's `default_private` fallback when none is supplied) to a concrete `StorageBackend` adapter. Enforce per-backend tenant access lists, capability declarations, and the at-least-one-default-role invariant. P1 declares `s3-compatible` backends with `PresignedUrls` (mandatory) and optional `PublicReadUrls` capability. Each backend also declares a boolean `versioning` flag (operator-declared in TOML; FileStorage trusts it without runtime validation).

### 1.2 Purpose

Per `cpt-cf-file-storage-principle-modular-backend-roster` the module hosts a fixed set of S3-compatible backends in P1 and routes every operation through this router. Tenant access lists implement the «no enumeration oracle» guarantee — a backend a tenant cannot see returns `NotFound`, never `Forbidden`. P1 deliberately avoids any boot-time backend probe (per `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`); backend connectivity surfaces lazily on the first request.

**Requirements**: `cpt-cf-file-storage-fr-backend-abstraction`, `cpt-cf-file-storage-fr-backend-capabilities`, `cpt-cf-file-storage-fr-tenant-boundary`

**Principles**: `cpt-cf-file-storage-principle-modular-backend-roster`

### 1.3 Actors

- `cpt-cf-file-storage-actor-cf-modules` — every consumer of the SDK ends up routed through this component
- `cpt-cf-file-storage-actor-platform-user` — sees the `list_backends` output via the REST surface

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (§3.2 Backend Router, §3.2 capability surface in P1)
- **ADR**: [ADR-0003](../ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), [ADR-0004](../ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md)
- **Use cases**: `cpt-cf-file-storage-usecase-backend-config`
- **Decomposition**: [DECOMPOSITION.md §2.3](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-file-storage-feature-module-foundation`

## 2. Actor Flows (CDSL)

### List Backends Flow

- [ ] `p1` - **ID**: `cpt-cf-file-storage-flow-backend-router-and-roster-list-backends`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Success Scenarios**:

- Caller's tenant sees only backends on its access list
- The `default_private` flag is surfaced on each entry

**Error Scenarios**:

- Caller has no `SecurityContext` (no tenant) — surfaces as `Unauthorized` upstream

**Steps**:

1. [ ] - `p1` - Caller calls `list_backends(ctx)` via SDK or `GET /api/file-storage/v1/storages` - `inst-list-1`
2. [ ] - `p1` - Read `tenant_id` from `SecurityContext` - `inst-list-2`
3. [ ] - `p1` - **FOR EACH** backend in the static registry - `inst-list-3`
   1. [ ] - `p1` - **IF** backend.tenant_access is empty OR contains `tenant_id` - `inst-list-3a`
      1. [ ] - `p1` - Append `Backend { id, kind, default_private, default_public, transport, capabilities, max_file_size_bytes, versioning }` to the result - `inst-list-3a1`
4. [ ] - `p1` - **RETURN** `Vec<Backend>` (filtered) - `inst-list-4`

## 3. Processes / Business Logic (CDSL)

### Backend Resolve

- [ ] `p1` - **ID**: `cpt-cf-file-storage-algo-backend-router-and-roster-resolve`

**Input**: `(SecurityContext ctx, Option<BackendId> backend_id)` — when `None`, the resolver falls back to the tenant's `default_private` backend

**Output**: `&dyn StorageBackend` reference; `NotFound` when the backend is hidden or absent

**Steps**:

1. [ ] - `p1` - **IF** `backend_id` is `None` - `inst-res-1`
   1. [ ] - `p1` - Delegate to `cpt-cf-file-storage-algo-module-foundation-default-resolver` (returns the tenant's `default_private` backend) - `inst-res-1a`
2. [ ] - `p1` - **ELSE** look up `backend_id` in the immutable registry - `inst-res-2`
3. [ ] - `p1` - **IF** lookup miss - `inst-res-3`
   1. [ ] - `p1` - **RETURN** Err(NotFound) - `inst-res-3a`
4. [ ] - `p1` - **IF** backend.tenant_access is non-empty AND does NOT contain `ctx.tenant_id` - `inst-res-4`
   1. [ ] - `p1` - **RETURN** Err(NotFound)  // no enumeration oracle - `inst-res-4a`
5. [ ] - `p1` - **RETURN** Ok(&adapter) - `inst-res-5`

## 4. States (CDSL)

The Backend Router holds an immutable in-memory registry. There is no runtime state machine in P1; the registry is built once at module init and never mutated. Runtime backend registration is P2.

## 5. Definitions of Done

### Implement Backend Registry

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-backend-router-and-roster-registry`

The system **MUST** build an immutable backend registry from the loaded TOML at module init, indexed by stable `backend_id` (UUID). Each entry **MUST** carry: `id`, `kind` (only `s3-compatible` is accepted in P1), `default_private`, `default_public`, `transport` (only `redirect` in P1), `capabilities` (`PresignedUrls` mandatory; optional `PublicReadUrls`), `max_file_size_bytes`, `versioning` (boolean — operator-declared, no runtime probe), `tenant_access` (empty list = "all tenants"). `backend_id` uniqueness across the roster **MUST** be enforced at registry-load. The boot-time conditional-PUT smoke-test from earlier drafts is removed; FileStorage's correctness on the upload path does not depend on backend-side preconditions.

**Implements**:

- `cpt-cf-file-storage-flow-backend-router-and-roster-list-backends`
- `cpt-cf-file-storage-algo-backend-router-and-roster-resolve`

**Constraints**: `cpt-cf-file-storage-constraint-static-config-p1`, `cpt-cf-file-storage-constraint-no-cross-backend-migration`

**Touches**:

- Crate: `file-storage` (component: Backend Router)
- Trait: `StorageBackend` (declared here, implemented in `s3-compatible-adapter`)

### Implement Resolve and Tenant Access Enforcement

- [ ] `p1` - **ID**: `cpt-cf-file-storage-dod-backend-router-and-roster-resolve`

The system **MUST** expose `resolve(ctx, Option<BackendId>) -> &dyn StorageBackend` plus `list_backends(ctx) -> Vec<Backend>`. Both **MUST** enforce the per-backend tenant access list and return `NotFound` (never `Forbidden`) when the caller's tenant is not on the list — preserving the no-enumeration-oracle guarantee. `resolve` with `None` **MUST** delegate to the foundation's default-backend resolver (returns `default_private`).

**Implements**:

- `cpt-cf-file-storage-flow-backend-router-and-roster-list-backends`
- `cpt-cf-file-storage-algo-backend-router-and-roster-resolve`

**Constraints**: `cpt-cf-file-storage-constraint-no-ambient-authn`, `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`

**Touches**:

- API: SDK `list_backends`; REST `GET /api/file-storage/v1/storages` (wired in `rest-api`)

## 6. Acceptance Criteria

- [ ] `list_backends` returns only backends visible to the caller's tenant; backends with non-matching `tenant_access` are absent from the response. Each entry carries `default_private`, `default_public`, capabilities (`PresignedUrls`, optionally `PublicReadUrls`), and `versioning`.
- [ ] `resolve(ctx, Some(missing_or_hidden_id))` returns `NotFound` with no signal that the `backend_id` exists for a different tenant.
- [ ] `resolve(ctx, None)` returns the tenant's `default_private` backend.
- [ ] Module fails-fast at boot when zero backends declare `default_private = true` for the tenant view.
- [ ] Module fails-fast at boot on a TOML entry whose `kind` is not `s3-compatible`.
- [ ] No backend receives any boot-time HEAD/PUT/DELETE — connectivity is verified lazily on the first real request (per `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`). The operator's `versioning` declaration is trusted without runtime probing.
