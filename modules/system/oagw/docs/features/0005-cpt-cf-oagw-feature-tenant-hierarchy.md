# Feature: Multi-Tenant Configuration Hierarchy


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Ancestor Shares Upstream](#ancestor-shares-upstream)
  - [Descendant Binds to Inherited Upstream](#descendant-binds-to-inherited-upstream)
  - [Alias Resolution with Shadowing](#alias-resolution-with-shadowing)
  - [Enforced Constraint Validation](#enforced-constraint-validation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Hierarchical Config Merge](#hierarchical-config-merge)
  - [Alias Shadowing Resolution](#alias-shadowing-resolution)
  - [Permission Validation for Override](#permission-validation-for-override)
  - [Effective Rate Limit Computation](#effective-rate-limit-computation)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Sharing Mode Fields](#implement-sharing-mode-fields)
  - [Implement Hierarchical Config Merge](#implement-hierarchical-config-merge)
  - [Implement Alias Shadowing](#implement-alias-shadowing)
  - [Implement Permission-Based Override Control](#implement-permission-based-override-control)
  - [Implement Secret Access Control Integration](#implement-secret-access-control-integration)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-oagw-featstatus-tenant-hierarchy-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-oagw-feature-tenant-hierarchy`

## 1. Feature Context

### 1.1 Overview

Implement hierarchical configuration override across tenant tree with sharing modes, alias shadowing, merge strategies, and permission-based override control.

### 1.2 Purpose

Enables partner/customer hierarchies where partners share upstream access with controlled credential and rate limit policies. Covers `cpt-cf-oagw-fr-config-layering`, `cpt-cf-oagw-fr-hierarchical-config`, `cpt-cf-oagw-fr-alias-resolution`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Configures sharing modes and enforced policies on upstreams |
| `cpt-cf-oagw-actor-tenant-admin` | Overrides inherited configurations within allowed sharing policies |
| `cpt-cf-oagw-actor-cred-store` | Validates tenant visibility for secret references during hierarchy resolution |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Requirements**: `cpt-cf-oagw-fr-config-layering`, `cpt-cf-oagw-fr-hierarchical-config`, `cpt-cf-oagw-fr-alias-resolution`
- **Design elements**: `cpt-cf-oagw-component-model`, `cpt-cf-oagw-db-schema`
- **Principles**: `cpt-cf-oagw-principle-tenant-scope`, `cpt-cf-oagw-principle-cred-isolation`
- **Constraints**: `cpt-cf-oagw-constraint-no-direct-internet`
- **Dependencies**: `cpt-cf-oagw-feature-management-api`

### 1.5 Out of Scope

- `cred_store` internals — secret storage and sharing policies are an external dependency
- Tenant hierarchy tree resolution — platform responsibility; OAGW receives the resolved ancestor chain
- Rate limiting enforcement and circuit breaker integration (Feature 6: Rate Limiting & Resilience)
- Proxy request execution flow (Feature 4: HTTP Proxy Engine)

## 2. Actor Flows (CDSL)

### Ancestor Shares Upstream

- [x] `p2` - **ID**: `cpt-cf-oagw-flow-tenant-share-upstream`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Success Scenarios**:
- Upstream is created with sharing modes configured per field
- Descendant tenants can discover and bind to the upstream according to sharing policies

**Error Scenarios**:
- Invalid sharing mode value (not `private`, `inherit`, or `enforce`)
- Upstream validation fails (missing required fields)

**Steps**:
1. [x] - `p2` - Platform operator creates upstream with sharing configuration via Management API - `inst-share-1`
2. [x] - `p2` - API: POST /api/oagw/v1/upstreams (body includes sharing fields per `cpt-cf-oagw-component-model`) - `inst-share-2`
3. [x] - `p2` - Validate sharing mode for each field is one of `private`, `inherit`, `enforce` - `inst-share-3`
4. [x] - `p2` - **IF** any sharing mode is invalid - `inst-share-4`
   1. [x] - `p2` - **RETURN** 400 ValidationError with invalid field details - `inst-share-4a`
5. [x] - `p2` - Validate upstream fields per `cpt-cf-oagw-algo-domain-entity-validation` - `inst-share-5`
6. [x] - `p2` - DB: INSERT oagw_upstream with sharing mode columns (auth_sharing, rate_limit_sharing, plugins_sharing, cors_sharing) - `inst-share-6`
7. [x] - `p2` - **IF** `(tenant_id, alias)` uniqueness constraint fails - `inst-share-7`
   1. [x] - `p2` - **RETURN** 409 Conflict with alias collision details - `inst-share-7a`
8. [x] - `p2` - **RETURN** created upstream with sharing configuration - `inst-share-8`

### Descendant Binds to Inherited Upstream

- [x] `p2` - **ID**: `cpt-cf-oagw-flow-tenant-bind-inherited`

**Actor**: `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Descendant creates a binding to ancestor's upstream with allowed overrides applied
- Tenant-local tags are added to effective tags without mutating ancestor tags

**Error Scenarios**:
- Descendant lacks `oagw:upstream:bind` permission
- Descendant attempts override on `enforce`-mode field
- Descendant attempts auth override without `oagw:upstream:override_auth` permission
- `secret_ref` not accessible to descendant tenant via `cred_store`

**Steps**:
1. [x] - `p2` - Tenant admin submits upstream creation request that resolves to an existing ancestor upstream alias - `inst-bind-1`
2. [x] - `p2` - API: POST /api/oagw/v1/upstreams (alias matches ancestor upstream) - `inst-bind-2`
3. [x] - `p2` - Resolve ancestor upstream by walking hierarchy via `cpt-cf-oagw-algo-tenant-alias-shadow` - `inst-bind-3`
4. [x] - `p2` - Check `oagw:upstream:bind` permission via `cpt-cf-oagw-algo-tenant-permission-check` - `inst-bind-4`
5. [x] - `p2` - **IF** permission denied - `inst-bind-5`
   1. [x] - `p2` - **RETURN** 403 Forbidden with missing permission details - `inst-bind-5a`
6. [x] - `p2` - **FOR EACH** overridden field in request - `inst-bind-6`
   1. [x] - `p2` - **IF** ancestor field sharing is `enforce` - `inst-bind-6a`
      1. [x] - `p2` - **RETURN** 400 ValidationError: field cannot be overridden (sharing: enforce) - `inst-bind-6a1`
   2. [x] - `p2` - **IF** ancestor field sharing is `private` - `inst-bind-6b`
      1. [x] - `p2` - **RETURN** 400 ValidationError: field not visible (sharing: private) - `inst-bind-6b1`
   3. [x] - `p2` - **IF** field is auth and override requested - `inst-bind-6c`
      1. [x] - `p2` - Check `oagw:upstream:override_auth` permission - `inst-bind-6c1`
      2. [x] - `p2` - **IF** auth override has `secret_ref` - `inst-bind-6c2`
         1. [x] - `p2` - Validate `secret_ref` accessibility via `cred_store` for descendant tenant - `inst-bind-6c2a`
         2. [x] - `p2` - **IF** secret not accessible - `inst-bind-6c2b`
            1. [x] - `p2` - **RETURN** 400 ValidationError: secret_ref not accessible to tenant - `inst-bind-6c2b1`
   4. [x] - `p2` - **IF** field is rate_limit and override requested - `inst-bind-6d`
      1. [x] - `p2` - Check `oagw:upstream:override_rate` permission - `inst-bind-6d1`
   5. [x] - `p2` - **IF** field is plugins and additions requested - `inst-bind-6e`
      1. [x] - `p2` - Check `oagw:upstream:add_plugins` permission - `inst-bind-6e1`
7. [x] - `p2` - **IF** `cred_store` is unavailable during secret_ref validation (timeout or connection error) - `inst-bind-7`
   1. [x] - `p2` - **RETURN** 503 ServiceUnavailable: credential validation unavailable (fail-closed) - `inst-bind-7a`
8. [x] - `p2` - **IF** request includes tags - `inst-bind-8`
   1. [x] - `p2` - Treat tags as tenant-local additions; do not mutate ancestor tags - `inst-bind-8a`
9. [x] - `p2` - DB: BEGIN transaction - `inst-bind-9`
10. [x] - `p2` - DB: INSERT oagw_upstream with descendant tenant_id, storing local overrides - `inst-bind-10`
11. [x] - `p2` - **IF** `(tenant_id, alias)` uniqueness constraint fails - `inst-bind-11`
    1. [x] - `p2` - DB: ROLLBACK - `inst-bind-11a`
    2. [x] - `p2` - **RETURN** 409 Conflict with alias collision details - `inst-bind-11b`
12. [x] - `p2` - DB: COMMIT - `inst-bind-12`
13. [x] - `p2` - **RETURN** created upstream binding with effective configuration - `inst-bind-13`

### Alias Resolution with Shadowing

- [x] `p2` - **ID**: `cpt-cf-oagw-flow-tenant-alias-resolve`

**Actor**: `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Proxy request resolves alias to the closest upstream in tenant hierarchy
- Enforced ancestor constraints remain active even when descendant shadows alias

**Error Scenarios**:
- Alias not found in any ancestor tenant
- All matching upstreams are disabled

**Steps**:
1. [x] - `p2` - System receives proxy request with alias in path: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}` - `inst-resolve-1`
2. [x] - `p2` - Extract requesting tenant_id from SecurityContext - `inst-resolve-2`
3. [x] - `p2` - Resolve upstream via `cpt-cf-oagw-algo-tenant-alias-shadow` - `inst-resolve-3`
4. [x] - `p2` - **IF** no upstream found in hierarchy - `inst-resolve-4`
   1. [x] - `p2` - **RETURN** 404 RouteNotFound - `inst-resolve-4a`
5. [x] - `p2` - **IF** resolved upstream is disabled and no enabled ancestor exists - `inst-resolve-5`
   1. [x] - `p2` - **RETURN** 503 LinkUnavailable - `inst-resolve-5a`
6. [x] - `p2` - Compute effective configuration via `cpt-cf-oagw-algo-tenant-config-merge` - `inst-resolve-6`
7. [x] - `p2` - **RETURN** effective upstream configuration for proxy execution - `inst-resolve-7`

### Enforced Constraint Validation

- [ ] `p2` - **ID**: `cpt-cf-oagw-flow-tenant-enforce-constraints`

**Actor**: `cpt-cf-oagw-actor-platform-operator`

**Success Scenarios**:
- Ancestor's enforced constraints are applied to effective configuration
- Descendant cannot bypass enforced fields even through alias shadowing

**Error Scenarios**:
- Constraint resolution encounters inconsistent ancestor chain (orphaned tenant)

**Steps**:
1. [x] - `p2` - System computes effective configuration after alias resolution - `inst-enforce-1`
2. [x] - `p2` - Collect all ancestor upstreams in hierarchy with `sharing: enforce` on any field - `inst-enforce-2`
3. [x] - `p2` - **FOR EACH** enforced ancestor upstream - `inst-enforce-3`
   1. [x] - `p2` - **IF** field is rate_limit with `sharing: enforce` - `inst-enforce-3a`
      1. [x] - `p2` - Apply `min(enforced_rate, effective_rate)` per `cpt-cf-oagw-algo-tenant-effective-rate` - `inst-enforce-3a1`
   2. [x] - `p2` - **IF** field is auth with `sharing: enforce` - `inst-enforce-3b`
      1. [x] - `p2` - Override effective auth with ancestor's enforced auth configuration - `inst-enforce-3b1`
   3. [x] - `p2` - **IF** field is plugins with `sharing: enforce` - `inst-enforce-3c`
      1. [x] - `p2` - Ensure enforced plugins remain in chain; descendant cannot remove them - `inst-enforce-3c1`
   4. [ ] - `p2` - **IF** field is cors with `sharing: enforce` - `inst-enforce-3d`
      1. [ ] - `p2` - Override effective CORS with ancestor's enforced CORS configuration - `inst-enforce-3d1`
4. [x] - `p2` - **RETURN** final effective configuration with all enforced constraints applied - `inst-enforce-4`

## 3. Processes / Business Logic (CDSL)

### Hierarchical Config Merge

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-tenant-config-merge`

**Input**: Selected upstream (from alias resolution), ancestor chain (ordered root → descendant), matched route (if any)

**Output**: Effective configuration with all merge strategies applied across all three layers

**Steps**:
1. [x] - `p2` - Initialize effective config from root ancestor's upstream (if shared) — absent fields default to null/unset - `inst-merge-1`
2. [x] - `p2` - Apply route-level overrides: **IF** matched route has field-level config, overlay onto effective upstream base — route config takes priority over upstream base per `cpt-cf-oagw-fr-config-layering` (upstream base < route < tenant); route-level sharing follows the same `private`/`inherit`/`enforce` semantics as upstream-level sharing - `inst-merge-2`
3. [x] - `p2` - **FOR EACH** ancestor in chain from root to selected tenant (inclusive) - `inst-merge-3`
   1. [x] - `p2` - **IF** ancestor has upstream with matching alias - `inst-merge-3a`
      1. [x] - `p2` - Merge auth: **IF** field is absent on current level, inherit from previous level; **IF** ancestor auth is `enforce`, keep ancestor auth (enforce is sticky — no descendant can override regardless of sharing mode); **IF** `auth_sharing == private`, replace with descendant auth (local-only); **IF** `auth_sharing: inherit`, use descendant auth; **IF** `auth_sharing: enforce`, descendant's enforce becomes sticky for further descendants - `inst-merge-3a1`
      2. [x] - `p2` - Merge rate_limit: **IF** field is absent, inherit from previous level (no limit = unbounded); **IF** `rate_limit_sharing == private` and ancestor is `enforce`, apply `min(ancestor_enforced, descendant)` (enforce cannot be bypassed); **IF** `rate_limit_sharing == private` and no ancestor enforce, replace with descendant rate (local-only); **IF** present with `inherit` or `enforce`, `effective = min(ancestor, descendant)` — stricter always wins - `inst-merge-3a2`
      3. [x] - `p2` - Merge plugins: **IF** field is absent, inherit ancestor plugins; **IF** `plugins_sharing == private` and ancestor is `enforce`, preserve enforced items and append descendant items; **IF** `plugins_sharing == private` and no ancestor enforce, replace with descendant plugins (local-only); **IF** present with `inherit` or `enforce`, concatenate `ancestor.plugins + descendant.plugins`; enforced plugins cannot be removed - `inst-merge-3a3`
      4. [x] - `p2` - Merge tags: `effective_tags = union(ancestor_tags, descendant_tags)` — always add-only semantics; absent tags treated as empty set - `inst-merge-3a4`
      5. [ ] - `p2` - Merge CORS: **IF** `cors_sharing == private`, skip; **IF** field is absent, inherit from previous level; **IF** `cors_sharing: inherit`, union origins; **IF** `cors_sharing: enforce`, use ancestor CORS - `inst-merge-3a5`
4. [x] - `p2` - **RETURN** effective configuration with all three layers merged - `inst-merge-4`

> **Layering note**: Per `cpt-cf-oagw-fr-config-layering`, the merge priority is: Upstream (base) < Route < Tenant (highest priority). Step 1 initializes the upstream base. Step 2 applies route-level overrides (route > upstream). Step 3 applies tenant hierarchy overrides last (tenant > route), ensuring tenant `enforce` constraints are never bypassed. Route-level sharing follows the same `private`/`inherit`/`enforce` semantics as upstream-level sharing.

### Alias Shadowing Resolution

- [x] `p2` - **ID**: `cpt-cf-oagw-algo-tenant-alias-shadow`

**Input**: Alias string, requesting tenant_id, tenant ancestor chain

**Output**: Resolved upstream (closest match in hierarchy) or not-found error

**Steps**:
1. [x] - `p2` - Obtain ancestor chain for requesting tenant (ordered: self → parent → ... → root) - `inst-shadow-1`
2. [x] - `p2` - **FOR EACH** tenant_id in chain (self first) - `inst-shadow-2`
   1. [x] - `p2` - DB: SELECT from oagw_upstream WHERE tenant_id = :current AND alias = :alias - `inst-shadow-2a`
   2. [x] - `p2` - **IF** upstream found AND (tenant_id == requesting_tenant OR any per-field sharing flag — `auth_sharing`, `rate_limit_sharing`, `plugins_sharing`, `cors_sharing` — is != `private`) - `inst-shadow-2b`
      1. [x] - `p2` - **IF** upstream is enabled - `inst-shadow-2b1`
         1. [x] - `p2` - **RETURN** found upstream as selected match - `inst-shadow-2b1a`
      2. [x] - `p2` - **ELSE** (upstream disabled) - `inst-shadow-2b2`
         1. [x] - `p2` - Record as disabled match; continue walking for enabled ancestor - `inst-shadow-2b2a`
3. [x] - `p2` - **IF** no match found in entire chain - `inst-shadow-3`
   1. [x] - `p2` - **RETURN** not-found error - `inst-shadow-3a`
4. [x] - `p2` - **IF** only disabled matches found - `inst-shadow-4`
   1. [x] - `p2` - **RETURN** link-unavailable error - `inst-shadow-4a`

### Permission Validation for Override

- [x] `p2` - **ID**: `cpt-cf-oagw-algo-tenant-permission-check`

**Input**: Requesting tenant SecurityContext, required permission string, target upstream

**Output**: Allowed or denied with reason

**Steps**:
1. [x] - `p2` - Extract permissions from SecurityContext bearer token via `modkit-auth` - `inst-perm-1`
2. [x] - `p2` - **IF** required permission is `oagw:upstream:bind` - `inst-perm-2`
   1. [x] - `p2` - Check token has `oagw:upstream:bind` for target upstream's tenant scope - `inst-perm-2a`
3. [x] - `p2` - **IF** required permission is `oagw:upstream:override_auth` - `inst-perm-3`
   1. [x] - `p2` - Check token has `oagw:upstream:override_auth` AND ancestor auth sharing is `inherit` - `inst-perm-3a`
4. [x] - `p2` - **IF** required permission is `oagw:upstream:override_rate` - `inst-perm-4`
   1. [x] - `p2` - Check token has `oagw:upstream:override_rate` AND ancestor rate_limit_sharing is `inherit` - `inst-perm-4a`
5. [x] - `p2` - **IF** required permission is `oagw:upstream:add_plugins` - `inst-perm-5`
   1. [x] - `p2` - Check token has `oagw:upstream:add_plugins` AND ancestor plugins sharing is `inherit` - `inst-perm-5a`
6. [x] - `p2` - **IF** permission check fails - `inst-perm-6`
   1. [x] - `p2` - **RETURN** denied with missing permission and sharing mode reason - `inst-perm-6a`
7. [x] - `p2` - **RETURN** allowed - `inst-perm-7`

### Effective Rate Limit Computation

- [x] `p2` - **ID**: `cpt-cf-oagw-algo-tenant-effective-rate`

**Input**: Descendant rate_limit config (may be absent/null), list of ancestor enforced rate_limit configs, route-level rate_limit (if any)

**Output**: Effective rate limit value (or unbounded if no limits configured at any level)

**Steps**:
1. [x] - `p2` - **IF** descendant has configured rate_limit, start with descendant's rate as candidate; **ELSE** set candidate to unbounded (no limit) - `inst-rate-1`
2. [x] - `p2` - **FOR EACH** ancestor with `rate_limit_sharing: enforce` - `inst-rate-2`
   1. [x] - `p2` - `candidate = min(candidate, ancestor.rate_limit.rate)` — enforced ancestor always constrains, even if descendant is unbounded - `inst-rate-2a`
3. [x] - `p2` - **IF** route-level rate_limit is defined - `inst-rate-3`
   1. [x] - `p2` - `candidate = min(candidate, route.rate_limit.rate)` - `inst-rate-3a`
4. [x] - `p2` - **RETURN** candidate as effective rate limit - `inst-rate-4`

## 4. States (CDSL)

Not applicable — sharing modes (`private`, `inherit`, `enforce`) are static configuration values set at upstream creation or update time. They do not represent lifecycle state transitions. No entity in this feature has state machine behavior.

## 5. Definitions of Done

### Implement Sharing Mode Fields

- [x] `p2` - **ID**: `cpt-cf-oagw-dod-tenant-sharing-modes`

The system **MUST** support `private`, `inherit`, and `enforce` sharing modes on upstream and route configuration fields (auth, rate_limit, plugins, CORS). Tags do not have a sharing mode — they always use add-only union semantics across the hierarchy. The default sharing mode **MUST** be `private`. Sharing modes **MUST** be stored as columns on `oagw_upstream` and `oagw_route` and validated during create/update operations. Route-level sharing follows the same semantics as upstream-level sharing and participates in the 3-layer merge per `cpt-cf-oagw-fr-config-layering`.

**Implements**:
- `cpt-cf-oagw-flow-tenant-share-upstream`

**Touches**:
- API: `POST /api/oagw/v1/upstreams`, `PUT /api/oagw/v1/upstreams/{id}`, `POST /api/oagw/v1/routes`, `PUT /api/oagw/v1/routes/{id}`
- DB: `oagw_upstream` (sharing mode columns), `oagw_route` (sharing mode columns)
- Entities: `Upstream` (sharing fields), `Route` (sharing fields)

### Implement Hierarchical Config Merge

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-tenant-config-merge`

The system **MUST** merge configurations across tenant hierarchy with the following per-field strategies: auth override if `inherit`; rate limits `min(ancestor.enforced, descendant)` — stricter always wins; plugins concatenation (ancestor + descendant); tags union with add-only semantics. The system **MUST** apply the 3-layer merge priority (upstream base < route < tenant) per `cpt-cf-oagw-fr-config-layering`. Absent/null fields **MUST** inherit from the previous level (absent rate_limit = unbounded). Enforced ancestor constraints **MUST** never be bypassed.

> **TODO**: CORS merge paths (union origins for `inherit`, forced for `enforce`) are not yet implemented — `inst-merge-3a5` is open and `cors_sharing` is not yet on the domain model. Uncheck this DoD item until CORS merge is implemented and validated.

**Implements**:
- `cpt-cf-oagw-algo-tenant-config-merge`
- `cpt-cf-oagw-flow-tenant-enforce-constraints`

**Touches**:
- Entities: `Upstream` (effective configuration computation), `Route` (route-level overrides)

### Implement Alias Shadowing

- [x] `p2` - **ID**: `cpt-cf-oagw-dod-tenant-alias-shadowing`

The system **MUST** resolve aliases by walking the tenant hierarchy from descendant to root. The closest match **MUST** win (descendant shadows ancestor). Enforced limits from ancestors **MUST** still apply across shadowing. Disabled upstreams **MUST** be skipped during resolution, falling through to the next ancestor. If no enabled match is found, the system **MUST** return 404 RouteNotFound or 503 LinkUnavailable as appropriate.

**Implements**:
- `cpt-cf-oagw-algo-tenant-alias-shadow`
- `cpt-cf-oagw-flow-tenant-alias-resolve`

**Touches**:
- DB: `oagw_upstream` (alias lookup with tenant hierarchy walk)
- Entities: `Upstream`

### Implement Permission-Based Override Control

- [x] `p2` - **ID**: `cpt-cf-oagw-dod-tenant-permission-control`

The system **MUST** enforce permission checks before allowing descendant overrides: `oagw:upstream:bind` for binding to ancestor upstream, `oagw:upstream:override_auth` for auth override, `oagw:upstream:override_rate` for rate limit specification, `oagw:upstream:add_plugins` for plugin additions. Without appropriate permissions, descendants **MUST** use ancestor configuration as-is even with `sharing: inherit`.

**Implements**:
- `cpt-cf-oagw-algo-tenant-permission-check`
- `cpt-cf-oagw-flow-tenant-bind-inherited`

**Touches**:
- API: `POST /api/oagw/v1/upstreams`
- Entities: SecurityContext (permission extraction)

### Implement Secret Access Control Integration

- [x] `p2` - **ID**: `cpt-cf-oagw-dod-tenant-secret-access`

The system **MUST** validate `secret_ref` accessibility via `cred_store` for the descendant tenant when auth overrides include a `secret_ref`. If the secret is not accessible to the descendant (not owned and not shared by ancestor), the system **MUST** reject the request with 400 ValidationError. The system **MUST NOT** store or log secret material per `cpt-cf-oagw-principle-cred-isolation`.

**Implements**:
- `cpt-cf-oagw-flow-tenant-bind-inherited`

**Touches**:
- API: `POST /api/oagw/v1/upstreams` (auth override with secret_ref)
- Entities: `cred_store` client (external dependency)

## 6. Acceptance Criteria

- [x] Sharing modes (`private`, `inherit`, `enforce`) can be set per field on upstream create and update
- [x] Default sharing mode is `private` when not specified
- [x] Descendant with `oagw:upstream:bind` permission can create binding to ancestor's inherited upstream
- [x] Descendant without `oagw:upstream:bind` permission receives 403 Forbidden
- [x] Auth override with `sharing: inherit` succeeds when descendant has `oagw:upstream:override_auth`
- [x] Auth override with `sharing: enforce` is rejected with 400 ValidationError
- [x] Rate limit effective value equals `min(all_ancestor_enforced_rates, descendant_rate, route_rate)`
- [x] Plugin merge produces concatenated chain: `ancestor.plugins + descendant.plugins`
- [x] Enforced ancestor plugins cannot be removed by descendant
- [x] Tag merge produces union; descendants can add but not remove inherited tags
- [ ] CORS merge unions origins for `inherit`; uses ancestor config for `enforce`
- [x] Alias resolution walks tenant hierarchy from descendant to root; closest enabled match wins
- [x] Alias shadowing preserves all enforced ancestor constraints on the shadowed upstream
- [x] Disabled upstream is skipped during resolution, falling through to next ancestor
- [x] `secret_ref` in auth override is validated against `cred_store` for descendant tenant accessibility
- [x] Inaccessible `secret_ref` is rejected with 400 ValidationError
- [x] No secret material is stored or logged by OAGW
- [x] All DB operations use secure ORM with tenant scoping per `cpt-cf-oagw-principle-tenant-scope`
- [x] Route-level sharing modes (`private`, `inherit`, `enforce`) can be set per field on route create and update
- [x] 3-layer merge applies: upstream base < route overrides < tenant hierarchy overrides
- [x] Absent/null fields inherit from the previous level; absent rate_limit is treated as unbounded
- [x] `cred_store` unavailability during secret_ref validation results in 503 ServiceUnavailable (fail-closed)

## 7. Non-Applicable Concerns

- **Performance (PERF)**: Not applicable for initial specification — hierarchy walk is bounded by tenant tree depth (typically ≤ 5 levels). Performance-critical caching of effective configuration belongs to Feature 8 (Observability & Security Hardening) multi-layer cache.
- **States (CDSL)**: Not applicable — sharing modes are static configuration, not lifecycle states. See Section 4.
- **Reliability — Fault Tolerance (REL-FDESIGN-002)**: Partially addressed — `cred_store` unavailability is handled fail-closed (503 ServiceUnavailable) in the bind flow. Retry logic and circuit breaker patterns for `cred_store` calls are scoped to Feature 6 (Rate Limiting & Resilience). Tenant hierarchy resolution failures are a platform responsibility; OAGW receives the resolved ancestor chain as input.
- **Security — Audit Trail (SEC-FDESIGN-005)**: Not applicable — audit logging for configuration changes is scoped to Feature 8 (Observability & Security Hardening).
- **Data Privacy (DATA-FDESIGN-005)**: Not applicable — OAGW does not store PII. Secret material is never stored or logged per `cpt-cf-oagw-principle-cred-isolation`; credential management is delegated to `cred_store`.
- **Compliance (COMPL)**: Not applicable — internal infrastructure module with no regulatory or privacy obligations.
- **Usability (UX)**: Not applicable — no user interface; all interaction is programmatic via REST API.
- **Operations — Observability (OPS-FDESIGN-001)**: Not applicable — logging and metrics instrumentation scoped to Feature 8.
- **Operations — Rollout (OPS-FDESIGN-004)**: Not applicable — sharing mode fields are additive schema changes; existing upstreams default to `private` (no behavioral change for current tenants).
