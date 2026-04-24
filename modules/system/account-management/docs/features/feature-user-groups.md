# Feature: User Groups (via Resource Group delegation)


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [User Group RG Type Registration](#user-group-rg-type-registration)
  - [Tenant Hard-Delete Cascade Cleanup Trigger](#tenant-hard-delete-cascade-cleanup-trigger)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [User Group RG Type Schema Registration](#user-group-rg-type-schema-registration)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [RG Type Schema Idempotent Registration](#rg-type-schema-idempotent-registration)
  - [Cascade Cleanup Trigger at Tenant Hard-Delete](#cascade-cleanup-trigger-at-tenant-hard-delete)
  - [Delegation Boundary](#delegation-boundary)
  - [Membership User-Existence Pattern](#membership-user-existence-pattern)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-user-groups`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-user-groups`

## 1. Feature Context

### 1.1 Overview

Delegates every aspect of user-group hierarchy, membership storage, cycle detection, and tenant-scoped isolation to the Resource Group module. Account Management owns only three thin touchpoints: (1) registering the chained user-group RG type schema `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` at module initialization; (2) triggering Resource Group cleanup of a tenant's user-group subtree during tenant hard-deletion; (3) publishing the tenant-scoped user-query surface that consumers combine with `ResourceGroupClient` membership operations to verify user existence.

### 1.2 Purpose

Delivers the delegation half of PRD §5.6 (User Groups Management) by ensuring Account Management NEVER becomes a second source of truth for user-group state. The Resource Group module already provides typed hierarchy, tenant scoping, forest invariants, cycle detection, and isolation — duplicating any of that inside AM would add pass-through layers without domain value (per `principle-delegation-to-rg` and the rejected alternative recorded in `adr-resource-group-tenant-hierarchy-source`). This feature therefore authorizes consumers to call `ResourceGroupClient` directly for CRUD and membership, while AM idempotently registers the RG type during module init (`fr-user-group-rg-type`) and triggers RG cleanup during tenant hard-deletion so no orphaned user-group subtree survives a deleted tenant. User-existence checks needed for membership writes come from `feature-idp-user-operations-contract` (`GET /tenants/{tenant_id}/users` with `?user_id=<id>`) — this feature documents the combination pattern without adding any REST surface of its own.

**Requirements**: `cpt-cf-account-management-fr-user-group-rg-type`, `cpt-cf-account-management-fr-user-group-lifecycle`, `cpt-cf-account-management-fr-user-group-membership`, `cpt-cf-account-management-fr-nested-user-groups`

**Principles**: `cpt-cf-account-management-principle-delegation-to-rg`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-platform-admin` | Upstream caller of module initialization (registers the user-group RG type schema) and of tenant hard-deletion (triggers the cascade cleanup through `tenant-hierarchy-management`); never invokes user-group operations directly — those go through `ResourceGroupClient`. |
| `cpt-cf-account-management-actor-tenant-admin` | Consumer of the delegated user-group surface; calls `ResourceGroupClient` directly for group CRUD, membership, and nested-group operations within their authorized tenant scope; combines that surface with AM's `GET /tenants/{tenant_id}/users` for user-existence checks. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.6 User Groups Management (`fr-user-group-rg-type`, `fr-user-group-lifecycle`, `fr-user-group-membership`, `fr-nested-user-groups`).
- **Design**: [DESIGN.md](../DESIGN.md) §2.1 Delegation-to-RG Principle (`principle-delegation-to-rg`); §3.1 Domain Model — Tenant → Resource Group (user groups) delegation wiring; §3.4 External Dependencies — `ResourceGroupClient` consumer contract; user-group schema body at [user_group.v1.schema.json](../schemas/user_group.v1.schema.json); rejected-alternative context in [ADR-0004](../ADR/0004-cpt-cf-account-management-adr-resource-group-tenant-hierarchy-source.md) (`adr-resource-group-tenant-hierarchy-source`).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.6 User Groups (via Resource Group delegation).
- **Dependencies**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` — invokes this feature's cascade-cleanup trigger inside its hard-delete flow before the `tenants` row is removed.
  - `cpt-cf-account-management-feature-idp-user-operations-contract` — provides the `GET /tenants/{tenant_id}/users` surface consumers combine with `ResourceGroupClient` membership operations to verify user existence.

## 2. Actor Flows (CDSL)

### User Group RG Type Registration

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-user-groups-rg-type-registration`

**Actor**: `cpt-cf-account-management-actor-platform-admin`

**Success Scenarios**:

- On `AccountManagementModule` initialization, AM invokes `algo-user-group-rg-type-schema-registration` against the Resource Group types registry via `ResourceGroupClient`. If the chained type schema `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` is already registered with identical traits, the call is a successful no-op. If it is absent, AM registers it with `allowed_memberships = [gts.x.core.am.user.v1~]` and self-referential `allowed_parents = [gts.x.core.rg.type.v1~x.core.am.user_group.v1~]` before the module signals ready.

**Error Scenarios**:

- Resource Group is unreachable during module initialization — AM module init fails fast with `service_unavailable` category mapping delegated to the `feature-errors-observability` envelope, consistent with `feature-platform-bootstrap`'s hard-dependency posture. AM does NOT proceed to signal ready with an unregistered type.
- The registered schema exists but its traits diverge from the required shape (e.g., `allowed_memberships` missing `gts.x.core.am.user.v1~`, or `allowed_parents` missing self-nesting) — registration fails with a deterministic `validation` error; AM does NOT auto-repair the diverged schema.

**Steps**:

1. [ ] - `p1` - At `AccountManagementModule` initialization, invoke `algo-user-group-rg-type-schema-registration` with the chained type identifier `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` and the required traits (`allowed_memberships`, `allowed_parents`) - `inst-flow-rgreg-invoke-algo`
2. [ ] - `p1` - **IF** algorithm returned already-present-and-equivalent - `inst-flow-rgreg-noop-branch`
   1. [ ] - `p1` - **RETURN** success no-op; module init continues - `inst-flow-rgreg-noop-return`
3. [ ] - `p1` - **IF** algorithm returned registered-new - `inst-flow-rgreg-registered-branch`
   1. [ ] - `p1` - **RETURN** success; module init continues - `inst-flow-rgreg-registered-return`
4. [ ] - `p1` - **IF** algorithm returned `service_unavailable` (Resource Group unreachable) - `inst-flow-rgreg-unavailable-branch`
   1. [ ] - `p1` - **RETURN** module-init failure; module does NOT signal ready - `inst-flow-rgreg-unavailable-return`
5. [ ] - `p1` - **IF** algorithm returned `validation` (diverged schema already present) - `inst-flow-rgreg-diverged-branch`
   1. [ ] - `p1` - **RETURN** module-init failure; operator intervention required to reconcile RG-side schema - `inst-flow-rgreg-diverged-return`

### Tenant Hard-Delete Cascade Cleanup Trigger

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Actor**: `cpt-cf-account-management-actor-platform-admin`

**Success Scenarios**:

- During tenant hard-deletion (invoked by the retention job owned by `feature-tenant-hierarchy-management`), AM calls `ResourceGroupClient` to delete the tenant's user-group subtree before the `tenants` row is removed. Resource Group is the authoritative owner of the cleanup mechanics — AM only triggers and awaits completion. On success, the hard-delete flow proceeds to the `tenants` row delete.
- Tenant has no user groups — `ResourceGroupClient` cleanup call is a successful no-op; hard-delete proceeds.

**Error Scenarios**:

- Resource Group is unreachable during cleanup — the hard-delete flow is aborted with `service_unavailable` per DESIGN §3.4 ("If RG is unavailable during deletion validation, AM fails the operation with `service_unavailable` rather than proceeding"); the `tenants` row is NOT removed. The retention job retries on its next tick.
- Resource Group cleanup fails for a provider-specific reason — propagated through the `feature-errors-observability` envelope; hard-delete is aborted and the `tenants` row remains for the next retry.

**Steps**:

1. [ ] - `p1` - Called by the hard-delete flow in `feature-tenant-hierarchy-management` with `{tenant_id}` just before the `tenants` row delete - `inst-flow-cascade-entry`
2. [ ] - `p1` - Invoke `ResourceGroupClient` to delete the tenant's user-group subtree (groups of type `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` scoped to `{tenant_id}`) - `inst-flow-cascade-invoke-rg`
3. [ ] - `p1` - **IF** RG call failed transport-level (unreachable or timeout) - `inst-flow-cascade-unavailable-branch`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=service_unavailable)` via the `feature-errors-observability` envelope; hard-delete flow aborts and `tenants` row is NOT removed - `inst-flow-cascade-unavailable-return`
4. [ ] - `p1` - **IF** RG call returned a provider-level error - `inst-flow-cascade-provider-error-branch`
   1. [ ] - `p1` - **RETURN** the mapped provider error via the envelope; hard-delete flow aborts and `tenants` row remains for retry - `inst-flow-cascade-provider-error-return`
5. [ ] - `p1` - **RETURN** success so the hard-delete flow can proceed to remove the `tenants` row - `inst-flow-cascade-success-return`

## 3. Processes / Business Logic (CDSL)

### User Group RG Type Schema Registration

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`

**Input**: Chained type identifier `gts.x.core.rg.type.v1~x.core.am.user_group.v1~`, required traits (`allowed_memberships = [gts.x.core.am.user.v1~]`, `allowed_parents = [gts.x.core.rg.type.v1~x.core.am.user_group.v1~]`), schema body reference (`user_group.v1.schema.json`).

**Output**: Idempotent outcome — `already-present-and-equivalent` (no-op), `registered-new` (newly persisted in RG), OR one of the mapped failure categories (`service_unavailable` when RG is unreachable, `validation` when an existing RG-side schema diverges from the required traits).

**Steps**:

> Registration is idempotent per `fr-user-group-rg-type` so AM module initialization is retry-safe across restarts. AM does NOT own a local cache of the RG type schema; registration status is re-verified against RG on every module init. AM does NOT auto-reconcile a diverged RG-side schema — that is an operator-intervention path per DESIGN §3.4.

1. [ ] - `p1` - Query the Resource Group types registry via `ResourceGroupClient` for the chained type identifier - `inst-algo-rgreg-query-existing`
2. [ ] - `p1` - **IF** RG query raised transport failure or timed out - `inst-algo-rgreg-transport-failure`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=service_unavailable)` so the calling flow can fail module init - `inst-algo-rgreg-transport-return`
3. [ ] - `p1` - **IF** the type is already registered with equivalent traits (`allowed_memberships` includes `gts.x.core.am.user.v1~` AND `allowed_parents` includes itself) - `inst-algo-rgreg-equivalent-branch`
   1. [ ] - `p1` - **RETURN** `already-present-and-equivalent` - `inst-algo-rgreg-equivalent-return`
4. [ ] - `p1` - **IF** the type is registered but traits diverge - `inst-algo-rgreg-diverged-branch`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=validation, reason=diverged_schema)` so the calling flow can surface the envelope-mapped error; AM does NOT auto-repair - `inst-algo-rgreg-diverged-return`
5. [ ] - `p1` - **ELSE** the type is absent - `inst-algo-rgreg-absent-branch`
   1. [ ] - `p1` - Register the chained type schema via `ResourceGroupClient` with the required traits and the schema body at `user_group.v1.schema.json` - `inst-algo-rgreg-register`
   2. [ ] - `p1` - **RETURN** `registered-new` - `inst-algo-rgreg-register-return`

## 4. States (CDSL)

**Not applicable.** This feature owns no AM-side lifecycle. The user-group type-schema registration is a one-shot idempotent operation at module init (no state machine); the cascade cleanup trigger is a pass-through call to `ResourceGroupClient` inside the hard-delete flow (no AM-side state to model). User-group hierarchy, membership, and nested-group state are ALL owned by the Resource Group module — AM stores no user-group rows, no membership adapter tables, no registration mirror.

## 5. Definitions of Done

### RG Type Schema Idempotent Registration

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-rg-type-schema-idempotent-registration`

The system **MUST** invoke `algo-rg-type-schema-registration` during `AccountManagementModule` initialization for the chained type identifier `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` with `allowed_memberships` including `gts.x.core.am.user.v1~` and self-referential `allowed_parents` to support nested user groups, per `fr-user-group-rg-type`. Registration **MUST** be idempotent: an already-present-and-equivalent RG-side schema is a successful no-op; an absent schema is registered with the required traits; a diverged existing schema **MUST** fail module init with `sub_code=validation` rather than silently overwriting operator state. AM **MUST NOT** proceed to module-ready until registration returns success.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`
- `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`

**Touches**:

- Data: `gts://gts.x.core.rg.type.v1~x.core.am.user_group.v1~` (chained RG type schema registered by AM; schema body published for RG-side validation)
- Sibling integration: `ResourceGroupClient` (external to this feature's surface)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `service_unavailable` and `validation` sub-codes referenced by name only.

### Cascade Cleanup Trigger at Tenant Hard-Delete

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-cascade-cleanup-trigger`

The system **MUST** trigger `ResourceGroupClient` cleanup of the tenant's user-group subtree during tenant hard-deletion before the `tenants` row is removed. If the RG cleanup call fails transport-level, the hard-delete flow **MUST** abort with `sub_code=service_unavailable` via the `feature-errors-observability` envelope and the `tenants` row **MUST NOT** be removed — the retention job retries on its next tick. If RG returns a provider-level error, the hard-delete flow **MUST** abort and surface the mapped error; the `tenants` row remains for retry. AM **MUST NOT** perform RG-side cleanup work itself — the trigger is a pass-through call and the actual work is owned by Resource Group.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Touches**:

- Entities: `UserGroup` (delegated view), `UserGroupMembership` (delegated adapter)
- Sibling integration: `ResourceGroupClient`; `feature-tenant-hierarchy-management` hard-delete flow (caller)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `service_unavailable` sub-code referenced by name only.

### Delegation Boundary

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-delegation-boundary`

The system **MUST NOT** expose any AM-side REST endpoint for user-group CRUD, membership add/remove, or nested-group traversal — consumers call `ResourceGroupClient` directly per `principle-delegation-to-rg`. The AM OpenAPI surface **MUST NOT** contain a `/user-groups` family. AM **MUST NOT** own any `user_group_*` or `user_group_membership_*` storage table, any membership adapter table, or any group-hierarchy cache. AM **MUST NOT** proxy, coordinate, or observe individual RG CRUD / membership calls beyond the two authorized touchpoints (module-init registration + hard-delete cascade cleanup). Cycle detection for nested groups **MUST** be enforced by Resource Group's forest invariants — AM does NOT re-implement it.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`
- `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Touches**:

- Entities: `UserGroup`, `UserGroupMembership`
- Sibling integration: `ResourceGroupClient`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### Membership User-Existence Pattern

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-membership-user-existence-pattern`

Membership-write callers **MUST** combine AM's `GET /tenants/{tenant_id}/users` (owned by `feature-idp-user-operations-contract`, with `?user_id=<id>` for point checks) with `ResourceGroupClient` membership operations: caller verifies user existence via AM's user-list surface first, then invokes `ResourceGroupClient` to add the user to a group. AM **MUST NOT** introduce any composite or convenience endpoint that wraps these two steps — the delegation principle requires consumers to call both surfaces directly so the authoritative user-existence signal and the authoritative membership write stay with their respective owners. RG-side integrity (that memberships reference only the platform user resource type) is enforced by the `allowed_memberships` trait on the registered user-group schema.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`

**Touches**:

- Sibling integration: `feature-idp-user-operations-contract` (`GET /tenants/{tenant_id}/users` surface); `ResourceGroupClient` (membership operations)

## 6. Acceptance Criteria

- [ ] On `AccountManagementModule` initialization against a fresh Resource Group registry (no prior user-group type schema), AM invokes `algo-rg-type-schema-registration` and registers the chained type `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` with `allowed_memberships = [gts.x.core.am.user.v1~]` and self-referential `allowed_parents`; the module signals ready only after registration returns `registered-new`. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] On subsequent module restarts with an already-present-and-equivalent RG-side schema, `algo-rg-type-schema-registration` returns `already-present-and-equivalent` as a no-op and module init continues; no duplicate or divergent registration is attempted. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] If Resource Group is unreachable during module initialization, `algo-rg-type-schema-registration` returns `sub_code=service_unavailable`; the module does NOT signal ready and emits an observable error through the `feature-errors-observability` envelope. If a RG-side schema is present but diverges (missing `gts.x.core.am.user.v1~` in `allowed_memberships` or missing self-nesting in `allowed_parents`), registration returns `sub_code=validation` and the module does NOT auto-repair. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] During tenant hard-deletion invoked by the retention job in `feature-tenant-hierarchy-management`, AM calls `ResourceGroupClient` to delete the tenant's user-group subtree before the `tenants` row is removed; if the RG cleanup call fails transport-level, the hard-delete flow aborts with `sub_code=service_unavailable` via the `feature-errors-observability` envelope and the `tenants` row is NOT removed. On the next retry, the hard-delete flow re-attempts cleanup. Fingerprints `dod-user-groups-cascade-cleanup-trigger`.
- [ ] The AM OpenAPI spec contains NO `/user-groups` family of endpoints and NO `/memberships` family — user-group CRUD, membership add/remove, and nested-group operations are not part of AM's REST surface. The AM module contains no `user_group_*` or `user_group_membership_*` tables, no adapter tables, and no in-memory group-hierarchy cache; RG is the single storage owner. Fingerprints `dod-user-groups-delegation-boundary`.
- [ ] A membership-write consumer (e.g., `feature-user-groups` caller) combining `GET /tenants/{tenant_id}/users?user_id=<id>` with a `ResourceGroupClient` membership add returns the authoritative user-existence signal before the RG membership write is issued; AM does NOT expose a convenience endpoint that wraps the two-step pattern, and the user-group schema's `allowed_memberships` restricting membership to the platform user resource type `gts.x.core.am.user.v1~` is the RG-side integrity guard. Fingerprints `dod-user-groups-membership-user-existence-pattern`, `dod-user-groups-delegation-boundary`.
- [ ] Nested user-group cycles (e.g., group `A` as a parent of group `B` and `B` as a parent of `A`) are refused by Resource Group forest invariants at the `ResourceGroupClient` boundary; AM's `allowed_parents` trait on the registered schema permits only the same chained user-group type as parent, so the RG-side cycle check is the authoritative enforcement. AM performs NO cycle detection of its own. Fingerprints `dod-user-groups-delegation-boundary`.

## 7. Deliberate Omissions

- **Resource Group storage (`user_group_*` and `user_group_membership_*` tables) and any other user-group persistence** — *Owned by the Resource Group module* (DECOMPOSITION §2.6 scope). AM stores no user-group rows, no membership adapter tables, and no group-hierarchy cache.
- **The Resource Group engine itself (generic RG CRUD, type-registry machinery, cascade engine, forest invariants, cycle detection, tenant-scoped isolation enforcement)** — *Owned by the Resource Group module.* AM only delegates to it via `ResourceGroupClient`.
- **REST endpoints for group create / update / delete, membership add / remove, nested-group traversal** — *Not part of the AM OpenAPI surface* (DECOMPOSITION §2.6 API: _none_). Consumers call `ResourceGroupClient` directly per `principle-delegation-to-rg`.
- **User identity operations (provisioning, deprovisioning, existence checks)** — *Owned by `cpt-cf-account-management-feature-idp-user-operations-contract`* (DECOMPOSITION §2.5). This feature consumes that feature's `GET /tenants/{tenant_id}/users` surface for documented user-existence combination patterns but does not reimplement it.
- **Tenant lifecycle, tenant hierarchy, and tenant-closure ownership** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). This feature is invoked from that feature's hard-delete flow but does not participate in tenant CRUD or closure maintenance itself.
- **AuthZ policy evaluation for user-group and membership operations** — *Inherited from `PolicyEnforcer` at the RG REST layer* (DESIGN §4.2; RG-side concern). AM makes no authorization decisions at the user-group boundary.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, metric catalog** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). Sub-codes `service_unavailable` and `validation` referenced in this feature are catalogued authoritatively there; this feature emits them by name and defers envelope formatting, HTTP status mapping, audit emission, and metric sample naming to that feature.
