# Feature: IdP User Operations Contract


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Provision User](#provision-user)
  - [Deprovision User](#deprovision-user)
  - [List Users](#list-users)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [IdP Contract Invocation](#idp-contract-invocation)
  - [Deprovision Idempotency Guard](#deprovision-idempotency-guard)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [IdP Contract Trait Surface](#idp-contract-trait-surface)
  - [No Local User Storage](#no-local-user-storage)
  - [IdP Unavailability Contract](#idp-unavailability-contract)
  - [Deprovision Idempotency](#deprovision-idempotency)
  - [Published User Projection Schema](#published-user-projection-schema)
  - [Authenticated Tenant-Scoped Invocation](#authenticated-tenant-scoped-invocation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-idp-user-operations-contract`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-idp-user-operations-contract`

## 1. Feature Context

### 1.1 Overview

Owns the pluggable `IdpProviderPluginClient` user-operations contract — tenant-scoped provision, deprovision, and query — that makes the configured IdP the authoritative source of truth for user identity and user-tenant binding. Publishes the GTS user-projection schema `gts.x.core.am.user.v1~` consumed by downstream features (e.g., `user-groups` membership checks) and exposes three tenant-scoped REST operations layered on top of the contract. Concrete provider adapters (Keycloak, Zitadel, Dex, etc.) conform to this contract but are delivered outside this module.

### 1.2 Purpose

Delivers the user-operations half of the AM↔IdP integration described in PRD §5.5 and DESIGN §4.1 (`IdpProviderPluginClient`), so tenant-scoped user provisioning, deprovisioning, and listing can be handled uniformly regardless of which IdP implementation ships with a deployment. Enforces the no-user-storage invariant (`cpt-cf-account-management-constraint-no-user-storage`): AM persists no local user table, projection, or membership cache — every user read and write is a pass-through to the IdP through `ClientHub` → plugin resolution. Maps transport failure or timeout of any IdP call to the public `idp_unavailable` sub-code (catalogued authoritatively by `feature-errors-observability`), and treats an already-absent user on `delete_user` as a successful idempotent no-op so `DELETE /tenants/{tenant_id}/users/{user_id}` remains retry-safe.

**Requirements**: `cpt-cf-account-management-fr-idp-user-provision`, `cpt-cf-account-management-fr-idp-user-deprovision`, `cpt-cf-account-management-fr-idp-user-query`, `cpt-cf-account-management-nfr-authentication-context`

**Principles**: `cpt-cf-account-management-principle-idp-agnostic`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Authenticated caller of every tenant-scoped user endpoint (`POST /tenants/{tenant_id}/users`, `DELETE /tenants/{tenant_id}/users/{user_id}`, `GET /tenants/{tenant_id}/users`); acts within an authorized tenant scope per platform AuthN/AuthZ contracts. |
| `cpt-cf-account-management-actor-idp` | External system reached through `IdpProviderPluginClient` via `ClientHub` plugin resolution; the single source of truth for user identity, credentials, and user-tenant binding. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.5 IdP Tenant & User Operations Contract (`fr-idp-user-provision`, `fr-idp-user-deprovision`, `fr-idp-user-query`); §6.2 Authentication Context (`nfr-authentication-context`); §8.5 IdP User Operations (scenario walkthroughs for happy path + IdP-unavailable path).
- **Design**: [DESIGN.md](../DESIGN.md) §2.1 IdP-Agnostic Principle (`principle-idp-agnostic`); §3.2 Component Model (`IdpProviderPluginClient` trait surface); §3.8 Error Codes Reference (`idp_unavailable`, `idp_unsupported_operation`); [ADR-0001](../ADR/0001-cpt-cf-account-management-adr-idp-contract-separation.md) (`adr-idp-contract-separation`); [ADR-0005](../ADR/0005-cpt-cf-account-management-adr-idp-user-identity-source-of-truth.md) (`adr-idp-user-identity-source-of-truth`); [ADR-0006](../ADR/0006-cpt-cf-account-management-adr-idp-user-tenant-binding.md) (`adr-idp-user-tenant-binding`).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.5 IdP User Operations Contract.
- **Dependencies**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` — owns tenant existence and `SecurityContext`-scoped tenant resolution consumed at contract-invocation time; also owns the companion `IdpProviderPluginClient::provision_tenant` / `deprovision_tenant` operations at tenant boundary (excluded from this feature's scope).

## 2. Actor Flows (CDSL)

### Provision User

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-idp-user-operations-contract-provision-user`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin POSTs a user payload to `/tenants/{tenant_id}/users`; UserService resolves the target tenant via `TenantService`, invokes `IdpProviderPluginClient::create_user` with tenant-scope binding and resolved tenant context (for IdP-specific context resolution such as effective Keycloak realm), and returns 201 Created with the IdP-assigned `user_id` projected through the `gts.x.core.am.user.v1~` schema.

**Error Scenarios**:

- `{tenant_id}` does not resolve to an `active` tenant (not found, `provisioning`, `suspended`, or `deleted`) — rejected by the tenant-existence guard with `not_found` or `validation` via the `feature-errors-observability` envelope; no IdP call issued.
- IdP contract call fails or times out — mapped to `idp_unavailable` via the envelope; no partial state either at AM or IdP beyond what the provider returned before the failure.
- IdP rejects the payload (e.g., duplicate username in the tenant scope) — mapped to the envelope per the provider's returned error category; AM stores no rejected intent.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` via platform AuthN middleware per `nfr-authentication-context` - `inst-flow-puser-validate-caller`
2. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant via `TenantService`; forward any not-found / non-active outcome as the envelope-mapped error - `inst-flow-puser-resolve-tenant`
3. [ ] - `p1` - Invoke `algo-idp-contract-invocation` with operation `create_user(tenant_id, tenant_context, payload)` via `ClientHub` plugin resolution - `inst-flow-puser-invoke-contract`
4. [ ] - `p1` - **IF** contract invocation returned `idp_unavailable` - `inst-flow-puser-unavailable-branch`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=idp_unavailable)` via the `feature-errors-observability` envelope; no local state mutated (AM owns none) - `inst-flow-puser-unavailable-return`
5. [ ] - `p1` - **IF** contract returned any other provider error - `inst-flow-puser-provider-error-branch`
   1. [ ] - `p1` - **RETURN** the mapped provider error through the envelope (category owned by `feature-errors-observability`) - `inst-flow-puser-provider-error-return`
6. [ ] - `p1` - **RETURN** 201 Created with the created user projected through `gts.x.core.am.user.v1~` (IdP-assigned `user_id`, `tenant_id`, projection-minimal profile fields per schema) - `inst-flow-puser-success-return`

### Deprovision User

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin DELETEs `/tenants/{tenant_id}/users/{user_id}`; UserService resolves the tenant, invokes `IdpProviderPluginClient::delete_user` with session revocation semantics, and returns 204 No Content on successful removal.
- IdP reports the target user as already absent in the tenant scope — idempotent no-op per `algo-deprovision-idempotency-guard`; UserService returns 204 No Content. `DELETE` remains retry-safe.

**Error Scenarios**:

- `{tenant_id}` does not resolve to an `active` tenant — rejected with `not_found` / `validation` via the envelope; no IdP call issued.
- IdP contract call fails or times out — mapped to `idp_unavailable`; no partial state.
- Provider does not support user deprovisioning (legacy IdP behind the contract) — returned as `idp_unsupported_operation` per PRD §5.5 and DESIGN §3.8; providers **MUST NOT** silently no-op mutating operations.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` - `inst-flow-duser-validate-caller`
2. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant via `TenantService`; forward any not-found / non-active outcome as the envelope-mapped error - `inst-flow-duser-resolve-tenant`
3. [ ] - `p1` - Invoke `algo-idp-contract-invocation` with operation `delete_user(tenant_id, user_id)` via `ClientHub` plugin resolution - `inst-flow-duser-invoke-contract`
4. [ ] - `p1` - **IF** contract invocation returned `idp_unavailable` - `inst-flow-duser-unavailable-branch`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=idp_unavailable)` via the envelope - `inst-flow-duser-unavailable-return`
5. [ ] - `p1` - **IF** contract invocation returned "user not found in this tenant scope" - `inst-flow-duser-absent-branch`
   1. [ ] - `p1` - Invoke `algo-deprovision-idempotency-guard` to confirm absent-is-success semantics per `fr-idp-user-deprovision` - `inst-flow-duser-idempotency-check`
   2. [ ] - `p1` - **RETURN** 204 No Content (idempotent success) - `inst-flow-duser-idempotent-return`
6. [ ] - `p1` - **IF** contract returned `idp_unsupported_operation` or any other provider error - `inst-flow-duser-provider-error-branch`
   1. [ ] - `p1` - **RETURN** the mapped provider error through the envelope - `inst-flow-duser-provider-error-return`
7. [ ] - `p1` - **RETURN** 204 No Content on successful removal - `inst-flow-duser-success-return`

### List Users

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin GETs `/tenants/{tenant_id}/users` with optional `user_id` filter and pagination params; UserService invokes `IdpProviderPluginClient::list_users` with the tenant filter and projects the response through `gts.x.core.am.user.v1~`; returns 200 OK with the paginated user list.
- A single-user existence check (`?user_id=<id>`) returns either a one-element list (user exists in this tenant scope) or an empty list (user does not exist in this tenant scope); both outcomes are 200 OK and are the authoritative existence signal consumed by downstream features such as `user-groups`.

**Error Scenarios**:

- `{tenant_id}` does not resolve to an `active` tenant — rejected with `not_found` / `validation` via the envelope; no IdP call issued.
- IdP contract call fails or times out — mapped to `idp_unavailable`; no partial data returned to the caller, no stale projection served.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext` - `inst-flow-luser-validate-caller`
2. [ ] - `p1` - Resolve `{tenant_id}` to an `active` tenant via `TenantService`; forward any not-found / non-active outcome as the envelope-mapped error - `inst-flow-luser-resolve-tenant`
3. [ ] - `p1` - Invoke `algo-idp-contract-invocation` with operation `list_users(tenant_id, optional user_id filter, pagination)` via `ClientHub` plugin resolution - `inst-flow-luser-invoke-contract`
4. [ ] - `p1` - **IF** contract invocation returned `idp_unavailable` - `inst-flow-luser-unavailable-branch`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=idp_unavailable)` via the envelope; NO stale projection is served per `principle-idp-agnostic` - `inst-flow-luser-unavailable-return`
5. [ ] - `p1` - Project the IdP-returned user records through the `gts.x.core.am.user.v1~` schema (tenant-minimal profile fields) - `inst-flow-luser-project`
6. [ ] - `p1` - **RETURN** 200 OK with the paginated projection - `inst-flow-luser-success-return`

## 3. Processes / Business Logic (CDSL)

### IdP Contract Invocation

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation`

**Input**: Operation name (`create_user` / `delete_user` / `list_users`), resolved `tenant_id`, resolved tenant context (for IdP-specific resolution such as effective Keycloak realm), and operation-specific payload.

**Output**: Contract-level outcome: success with the provider-returned projection, or a mapped failure (`idp_unavailable`, `idp_unsupported_operation`, or another provider-returned error category).

**Steps**:

> The contract surface is `IdpProviderPluginClient` (DESIGN §3.2 and §4.1). Plugin resolution goes through `ClientHub` per the CyberFabric gateway + plugin pattern. Providers MUST NOT silently no-op on mutating operations per PRD §5.5; unsupported mutating operations surface as `idp_unsupported_operation`.

1. [ ] - `p1` - Resolve the active `IdpProviderPluginClient` instance via `ClientHub` plugin registration - `inst-algo-ici-resolve-plugin`
2. [ ] - `p1` - Package the operation payload with tenant-scope metadata (tenant_id + resolved tenant context) per `principle-idp-agnostic` — AM never hard-codes provider-specific fields - `inst-algo-ici-package-request`
3. [ ] - `p1` - Invoke the resolved `IdpProviderPluginClient` operation with timeout governed by platform configuration - `inst-algo-ici-invoke`
4. [ ] - `p1` - **IF** invocation raised transport failure or timed out - `inst-algo-ici-transport-failure`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=idp_unavailable)` for the caller to surface through the `feature-errors-observability` envelope; AM holds no fallback projection per `constraint-no-user-storage` - `inst-algo-ici-transport-return`
5. [ ] - `p1` - **IF** provider returned `idp_unsupported_operation` (mutating operation on a legacy provider) - `inst-algo-ici-unsupported-branch`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=idp_unsupported_operation)` for envelope mapping - `inst-algo-ici-unsupported-return`
6. [ ] - `p1` - **IF** provider returned any other error category - `inst-algo-ici-provider-error`
   1. [ ] - `p1` - **RETURN** the mapped provider error for envelope mapping (category catalogued by `feature-errors-observability`) - `inst-algo-ici-provider-error-return`
7. [ ] - `p1` - **RETURN** success with the provider-returned projection (to be re-projected through `gts.x.core.am.user.v1~` by the caller when appropriate) - `inst-algo-ici-success-return`

### Deprovision Idempotency Guard

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard`

**Input**: `tenant_id`, `user_id`, provider-returned outcome from a `delete_user` contract call.

**Output**: Idempotent success verdict (treat "user not found in this tenant scope" as success) OR pass-through of any non-absent outcome.

**Steps**:

> Per `fr-idp-user-deprovision`, an already-absent IdP user is a successful no-op so `DELETE /tenants/{tenant_id}/users/{user_id}` remains idempotent and retry-safe. "Not found in this tenant scope" is the ONLY outcome that maps to success; `idp_unavailable` and `idp_unsupported_operation` are NOT absent-like and pass through unchanged.

1. [ ] - `p1` - **IF** provider outcome is "user not found in this tenant scope" - `inst-algo-dig-absent-branch`
   1. [ ] - `p1` - **RETURN** idempotent success (caller projects 204 No Content) per `fr-idp-user-deprovision` - `inst-algo-dig-absent-return`
2. [ ] - `p1` - **ELSE** provider outcome is any other category - `inst-algo-dig-other-branch`
   1. [ ] - `p1` - **RETURN** pass-through (caller handles per its own error / success path) - `inst-algo-dig-other-return`

## 4. States (CDSL)

**Not applicable.** Per `cpt-cf-account-management-constraint-no-user-storage`, this feature owns no AM-side tables, projections, or membership caches: the `User` entity, `UserTenantBinding`, and every user-operation outcome live in the configured IdP. There is no AM-owned lifecycle to model here — tenant lifecycle states (provisioning / active / suspended / deleted) that gate user operations are owned by `feature-tenant-hierarchy-management`, and the IdP's own user lifecycle is opaque to AM behind the `IdpProviderPluginClient` contract.

## 5. Definitions of Done

### IdP Contract Trait Surface

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-idp-user-operations-contract-contract-trait-surface`

The system **MUST** expose the `IdpProviderPluginClient` trait for `create_user`, `delete_user`, and `list_users` (tenant-scoped) as the single outbound integration point for user operations, resolved through `ClientHub` per `adr-idp-contract-separation`. Handler code **MUST NOT** hard-code IdP-specific logic, call any IdP library directly, or bypass the plugin resolution. The trait **MUST** accept tenant-scope metadata (tenant_id + resolved tenant context) on every invocation so provider implementations can resolve IdP-specific context (e.g., effective Keycloak realm) per `fr-idp-user-provision`.

**Implements**:

- `cpt-cf-account-management-flow-idp-user-operations-contract-provision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`
- `cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation`

**Constraints**: `cpt-cf-account-management-constraint-legacy-integration`

**Touches**:

- Entities: `User`, `UserTenantBinding`, `TenantId`
- Data: `gts://gts.x.core.am.user.v1~` (published user-projection schema)
- Sibling integration: `ClientHub` plugin resolution (external to this feature's surface)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### No Local User Storage

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-idp-user-operations-contract-no-local-user-storage`

The system **MUST NOT** maintain any AM-side user table, user projection cache, user-tenant binding table, or in-memory membership cache used for admit decisions. Every user read and write **MUST** be a live pass-through to the IdP through the contract; no per-request fallback to a local store is permitted when the IdP is unavailable, consistent with `principle-idp-agnostic` + `constraint-no-user-storage`. AM **MUST NOT** precompute or mirror the IdP's user catalog for tenant-scoped queries; `list_users` invocations are live calls into the IdP with tenant filtering.

**Implements**:

- `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`
- `cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation`

**Constraints**: `cpt-cf-account-management-constraint-no-user-storage`

**Touches**:

- Entities: `User`, `UserTenantBinding`
- Data: `gts://gts.x.core.am.user.v1~`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### IdP Unavailability Contract

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-idp-user-operations-contract-idp-unavailability-contract`

When the IdP contract call fails or times out on any user operation, the system **MUST** map the failure to `sub_code=idp_unavailable` via the `feature-errors-observability` envelope and **MUST NOT** serve a stale projection, an optimistic success, or a cached membership decision to the caller. `list_users` during an IdP outage **MUST** return the envelope-mapped error, not a degraded or partial result. The contract call timeout is governed by platform configuration and is observable; per-operation retry policy is not owned here (delegated to platform reliability operations).

**Implements**:

- `cpt-cf-account-management-flow-idp-user-operations-contract-provision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`
- `cpt-cf-account-management-algo-idp-user-operations-contract-idp-contract-invocation`

**Touches**:

- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `idp_unavailable` and `idp_unsupported_operation` sub-codes referenced by name only.

### Deprovision Idempotency

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-idp-user-operations-contract-deprovision-idempotency`

The system **MUST** treat an already-absent user on `DELETE /tenants/{tenant_id}/users/{user_id}` as a successful no-op so the endpoint remains idempotent and retry-safe per `fr-idp-user-deprovision`. Only the provider-returned "user not found in this tenant scope" outcome qualifies as absent-equivalent-success; `idp_unavailable` and `idp_unsupported_operation` **MUST** pass through unchanged. Provider implementations **MUST NOT** silently no-op on a genuinely supported mutating operation; unsupported operations surface as `idp_unsupported_operation` per PRD §5.5.

**Implements**:

- `cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user`
- `cpt-cf-account-management-algo-idp-user-operations-contract-deprovision-idempotency-guard`

**Touches**:

- Entities: `User`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `idp_unsupported_operation` sub-code referenced by name only.

### Published User Projection Schema

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-idp-user-operations-contract-user-projection-schema`

The system **MUST** publish the user-projection schema at the chained GTS schema identifier `gts.x.core.am.user.v1~` and **MUST** project every user response returned from the contract through that schema before returning to REST clients. Downstream consumers (e.g., `feature-user-groups` for user-existence checks, platform observability for audit emission) consume the published schema — no per-response custom projection is permitted. The schema **MUST NOT** include IdP-internal fields or credentials; the projection is tenant-minimal per `adr-idp-user-identity-source-of-truth` and `adr-idp-user-tenant-binding`.

**Implements**:

- `cpt-cf-account-management-flow-idp-user-operations-contract-provision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`

**Touches**:

- Entities: `User`, `UserTenantBinding`
- Data: `gts://gts.x.core.am.user.v1~`

### Authenticated Tenant-Scoped Invocation

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation`

Every REST endpoint layered over the contract **MUST** require a valid `SecurityContext` provided by the platform AuthN pipeline per `nfr-authentication-context`; unauthenticated calls **MUST** return 401 without invoking the contract. Every contract invocation **MUST** carry a resolved `tenant_id` scoped to an `active` tenant per `feature-tenant-hierarchy-management`; operations against a non-existent, `provisioning`, `suspended`, or `deleted` tenant **MUST** fail before the IdP call is issued. Cross-tenant reads and writes **MUST NOT** traverse the barrier through this feature's endpoints; AuthZ policy evaluation is inherited from `PolicyEnforcer` at the REST layer.

**Implements**:

- `cpt-cf-account-management-flow-idp-user-operations-contract-provision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-deprovision-user`
- `cpt-cf-account-management-flow-idp-user-operations-contract-list-users`

**Touches**:

- Entities: `TenantId`
- Sibling integration: platform AuthN middleware (`SecurityContext`) and `PolicyEnforcer` at the REST layer (external to this feature's surface)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

## 6. Acceptance Criteria

- [ ] An authenticated tenant-admin POST to `/tenants/{tenant_id}/users` on an `active` tenant resolves the target tenant via `TenantService`, invokes `IdpProviderPluginClient::create_user` through `ClientHub` with tenant-scope metadata, and returns 201 Created with a response body projected through `gts.x.core.am.user.v1~` carrying the IdP-assigned `user_id`; no AM-side user row is written because AM owns no user table per `constraint-no-user-storage`. Fingerprints `dod-idp-user-operations-contract-contract-trait-surface`, `dod-idp-user-operations-contract-no-local-user-storage`, `dod-idp-user-operations-contract-user-projection-schema`.
- [ ] A request against a `{tenant_id}` that is not an `active` tenant (not found, `provisioning`, `suspended`, or `deleted`) is rejected by the tenant-existence guard before any IdP call is issued; the envelope-mapped error (`not_found` or `validation`) is returned and no `IdpProviderPluginClient` call occurs. Fingerprints `dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation`.
- [ ] An unauthenticated call to any of `/tenants/{tenant_id}/users` (GET/POST) or `/tenants/{tenant_id}/users/{user_id}` (DELETE) returns 401 without invoking the `IdpProviderPluginClient` contract; the AuthN middleware rejects the request before it reaches the handler. Fingerprints `dod-idp-user-operations-contract-authenticated-tenant-scoped-invocation`.
- [ ] A contract invocation that fails with transport failure or timeout on any of `create_user`, `delete_user`, or `list_users` returns `sub_code=idp_unavailable` through the `feature-errors-observability` envelope; no stale projection or cached result is served to the caller. A `GET /tenants/{tenant_id}/users` during IdP outage returns the envelope-mapped error and not a partial or degraded result. Fingerprints `dod-idp-user-operations-contract-idp-unavailability-contract`.
- [ ] A `DELETE /tenants/{tenant_id}/users/{user_id}` for a user that the IdP reports as absent in the tenant scope returns 204 No Content (idempotent success) per `fr-idp-user-deprovision`; a subsequent retry of the same DELETE also returns 204. `idp_unavailable` and `idp_unsupported_operation` outcomes on the same endpoint pass through unchanged and are NOT treated as absent-equivalent. Fingerprints `dod-idp-user-operations-contract-deprovision-idempotency`.
- [ ] A `DELETE /tenants/{tenant_id}/users/{user_id}` routed to a provider that does not support user deprovisioning returns `sub_code=idp_unsupported_operation` through the envelope; the provider implementation does not silently no-op per PRD §5.5 and DESIGN §3.8. Fingerprints `dod-idp-user-operations-contract-deprovision-idempotency`, `dod-idp-user-operations-contract-contract-trait-surface`.
- [ ] A `GET /tenants/{tenant_id}/users?user_id=<id>` returns either a one-element list (user exists in this tenant scope) or an empty list (user absent in this tenant scope); both outcomes are 200 OK and are the authoritative user-existence signal consumed by sibling features such as `user-groups`. The response body is projected through `gts.x.core.am.user.v1~` and does not include IdP-internal fields or credentials. Fingerprints `dod-idp-user-operations-contract-user-projection-schema`, `dod-idp-user-operations-contract-no-local-user-storage`.

## 7. Deliberate Omissions

- **Conforming IdP plugin implementations (Keycloak adapter, Zitadel adapter, Dex adapter, etc.)** — *Delivered in separate crates outside this module.* This feature owns the `IdpProviderPluginClient` trait surface and the AM-side handler layer; concrete adapters conform to the trait but ship independently per `adr-idp-contract-separation` and DECOMPOSITION §2.5 scope.
- **Tenant-lifecycle IdP operations (`provision_tenant`, `deprovision_tenant`)** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). Those are side effects of tenant create / delete at the tenant boundary; this feature owns only the user-operations half of the contract.
- **Token validation, session renewal, federation, credential policy, MFA policy** — *Inherited from the platform AuthN layer and the configured IdP provider* (DESIGN §4.2; PRD §6.2). AM does not validate tokens, enforce MFA, rotate credentials, or manage federation; `SecurityContext` validation is a platform-layer precondition for every endpoint.
- **User-group orchestration, user-group membership, nested user groups** — *Owned by `cpt-cf-account-management-feature-user-groups`* (DECOMPOSITION §2.6). That feature depends on this one for authoritative user-existence checks but owns the group hierarchy, membership writes, and Resource Group delegation itself.
- **AuthZ policy evaluation for user-level operations** — *Owned by `PolicyEnforcer` / AuthZ Resolver* (DESIGN §4.2; out of AM domain logic). This feature delegates policy evaluation to the platform AuthZ surface at the REST layer; no authorization decisions are made inside the contract or the handlers beyond tenant-scope resolution.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, metric catalog** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). Sub-codes `idp_unavailable` and `idp_unsupported_operation` are catalogued authoritatively there; this feature emits them by name and defers envelope formatting, HTTP status mapping, audit emission, and metric sample naming to that feature.
- **AM-side user table, user projection cache, user-tenant binding table, or membership cache** — *Forbidden by `cpt-cf-account-management-constraint-no-user-storage`.* AM persists no local user state; every user operation is a live pass-through to the IdP through the contract.
