# Feature: User Groups (via Resource Group delegation)

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [User Group RG Type Publication](#user-group-rg-type-publication)
  - [Tenant Hard-Delete Cascade Cleanup Trigger](#tenant-hard-delete-cascade-cleanup-trigger)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [User Group RG Type Manifest Publication](#user-group-rg-type-manifest-publication)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Idempotent RG Type Publication](#idempotent-rg-type-publication)
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

Delegates every aspect of user-group hierarchy, membership storage, cycle detection, and tenant-scoped isolation to the Resource Group gear. Account Management owns only three thin touchpoints: (1) declaring the user-group container and user member-handle types and publishing them with AM’s types-registry manifest before application `init`; (2) triggering Resource Group cleanup of a tenant's user-group subtree during tenant hard-deletion; (3) publishing the tenant-scoped user-query surface that consumers combine with `ResourceGroupClient` membership operations to verify user existence.

### 1.2 Purpose

Delivers the delegation half of PRD §5.6 (User Groups Management) by ensuring Account Management NEVER becomes a second source of truth for user-group state. The Resource Group gear already provides typed hierarchy, tenant scoping, forest invariants, cycle detection, and isolation — duplicating any of that inside AM would add pass-through layers without domain value (per `principle-delegation-to-rg` and the rejected alternative recorded in `adr-resource-group-tenant-hierarchy-source`). This feature therefore authorizes consumers to call `ResourceGroupClient` directly for CRUD and membership, while AM publishes its RG type declarations through the shared startup publication mechanism (`fr-user-group-rg-type`) and triggers RG cleanup during tenant hard-deletion so no orphaned user-group subtree survives a deleted tenant. User-existence checks needed for membership writes come from `feature-idp-user-operations-contract` (`GET /tenants/{tenant_id}/users` with `?user_id=<id>`) — this feature documents the combination pattern without adding any REST surface of its own.

**Requirements**: `cpt-cf-account-management-fr-user-group-rg-type`, `cpt-cf-account-management-fr-user-group-lifecycle`, `cpt-cf-account-management-fr-user-group-membership`, `cpt-cf-account-management-fr-nested-user-groups`

**Principles**: `cpt-cf-account-management-principle-delegation-to-rg`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-platform-admin` | Operator of startup publication (admits AM’s RG type declarations before application initialization) and of tenant hard-deletion (triggers the cascade cleanup through `tenant-hierarchy-management`); never invokes user-group operations directly — those go through `ResourceGroupClient`. |
| `cpt-cf-account-management-actor-tenant-admin` | Consumer of the delegated user-group surface; calls `ResourceGroupClient` directly for group CRUD, membership, and nested-group operations within their authorized tenant scope; combines that surface with AM's `GET /tenants/{tenant_id}/users` for user-existence checks. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.6 User Groups Management (`fr-user-group-rg-type`, `fr-user-group-lifecycle`, `fr-user-group-membership`, `fr-nested-user-groups`).
- **Design**: [DESIGN.md](../DESIGN.md) §2.1 Delegation-to-RG Principle (`principle-delegation-to-rg`); §3.1 Domain Model — Tenant → Resource Group (user groups) delegation wiring; §3.4 External Dependencies — `ResourceGroupClient` consumer contract; user-group schema body at [user_group.v1.schema.json](../schemas/user_group.v1.schema.json); rejected-alternative context in [ADR-0004](../ADR/0004-cpt-cf-account-management-adr-resource-group-tenant-hierarchy-source.md) (`adr-resource-group-tenant-hierarchy-source`).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.6 User Groups (via Resource Group delegation).
- **Dependencies**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` — invokes this feature's cascade-cleanup trigger inside its hard-delete flow before the `tenants` row is removed.
  - `cpt-cf-account-management-feature-idp-user-operations-contract` — provides the `GET /tenants/{tenant_id}/users` surface consumers combine with `ResourceGroupClient` membership operations to verify user existence.

## 2. Actor Flows (CDSL)

### User Group RG Type Publication

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-user-groups-rg-type-registration`

**Actor**: `cpt-cf-account-management-actor-platform-admin`

**Success Scenarios**:

- Before application `init`, the runtime publishes AM's manifest through `PlatformTypesRegistryApi`. It includes both AM-owned declarations from [rg_types.rs](../../account-management/src/gts/rg_types.rs): the user-group container and user member-handle. The RG SDK publishes their base `gts.cf.core.rg.type.v1~` with Resource Group's manifest.
- An identical restart leaves the admitted definitions unchanged. Absent definitions are created; differing definitions are reconciled under the registry's revision and admission rules.

**Error Scenarios**:

- Registry unavailability, unresolved dependencies or transient conflicts prevent publication from completing within its bounded startup budget — startup fails before application `init`.
- A declaration is rejected or the registry refuses publication — startup fails with the publisher, affected identifiers and underlying cause. AM does not implement a separate RG-side schema comparison or error mapping.

**Steps**:

1. [ ] - `p1` - Build AM's manifest from `gts_publication(crates = [crate, account_management_sdk], entities = Self::configured_entities)` - `inst-flow-rgreg-build-manifest`
2. [ ] - `p1` - Invoke `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration` through the shared startup publisher - `inst-flow-rgreg-invoke-algo`
3. [ ] - `p1` - **IF** every manifest document is admitted, allow application `init` to proceed - `inst-flow-rgreg-admitted`
4. [ ] - `p1` - **ELSE** propagate the publication failure and stop startup before application `init` - `inst-flow-rgreg-failed`

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
2. [ ] - `p1` - Invoke `ResourceGroupClient` to delete the tenant's user-group subtree (groups of type `gts.cf.core.rg.type.v1~cf.core.am.user_group.v1~` scoped to `{tenant_id}`) - `inst-flow-cascade-invoke-rg`
3. [ ] - `p1` - **IF** RG call failed transport-level (unreachable or timeout) - `inst-flow-cascade-unavailable-branch`
   1. [ ] - `p1` - **RETURN** `(reject, code=service_unavailable)` via the `feature-errors-observability` envelope; hard-delete flow aborts and `tenants` row is NOT removed - `inst-flow-cascade-unavailable-return`
4. [ ] - `p1` - **IF** RG call returned a provider-level error - `inst-flow-cascade-provider-error-branch`
   1. [ ] - `p1` - **RETURN** the mapped provider error via the envelope; hard-delete flow aborts and `tenants` row remains for retry - `inst-flow-cascade-provider-error-return`
5. [ ] - `p1` - **RETURN** success so the hard-delete flow can proceed to remove the `tenants` row - `inst-flow-cascade-success-return`

## 3. Processes / Business Logic (CDSL)

### User Group RG Type Manifest Publication

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`

The existing `rg-type-registration` / `rg-type-schema-registration` traceability IDs now describe manifest publication. The former AM-specific RG bootstrap algorithm has been removed.

**Input**: AM's manifest, including the following declarations:

| Declaration | GTS identifier | Placement and membership traits |
|-------------|----------------|---------------------------------|
| `UserGroupRgTypeV1` | `gts.cf.core.rg.type.v1~cf.core.am.user_group.v1~` | `can_be_root = true`; `allowed_parent_types` contains itself; `allowed_membership_types` contains the user member-handle below; metadata is unconstrained. |
| `UserMemberRgTypeV1` | `gts.cf.core.rg.type.v1~cf.core.am.user.v1~` | `can_be_root = true`; empty `allowed_parent_types` and `allowed_membership_types`. Used as a membership resource type; AM users live in AM/IdP, not as RG groups. |

**Output**: Successful publication (`UpToDate`, or `Reconciled` with every desired identifier `Admitted`), or a publication failure that stops startup.

**Steps**:

1. [ ] - `p1` - Collect both declarations from AM's crate-local inventory into its manifest; retain their existing GTS identifiers so persisted RG groups and memberships keep resolving - `inst-algo-rgreg-collect`
2. [ ] - `p1` - Publish through the shared Types Registry SDK publisher using `PlatformTypesRegistryApi`; reconcile absent or differing documents under registry admission and revision rules - `inst-algo-rgreg-publish`
3. [ ] - `p1` - Await admission of every desired identifier, including dependencies on RG's base and the member handle; retry transient failures and pending dependencies within the shared startup budget - `inst-algo-rgreg-await`
4. [ ] - `p1` - **IF** publication succeeds, **RETURN** success; identical restarts preserve existing revisions - `inst-algo-rgreg-success`
5. [ ] - `p1` - **ELSE RETURN** the publication error with its publisher, identifier and cause context; application `init` does not run - `inst-algo-rgreg-failure`

See [Types Registry SDK publication](../../../types-registry/types-registry-sdk/README.md#publishing-from-a-gear) for reconciliation, retries and failure diagnostics. Resource Group reads definitions through `TypesRegistryApi` under the caller's context when validating writes. It owns no type catalogue, and AM performs no init-time type CRUD through `ResourceGroupClient`.

## 4. States (CDSL)

**Not applicable.** This feature owns no AM-side state machine. The shared publisher awaits Types Registry admission before application `init`; the registry owns type lifecycle and revisions. RG owns group hierarchy and membership state. AM stores no user-group rows, membership adapter tables or registration mirror; its hard-delete hook triggers RG cleanup.

## 5. Definitions of Done

### Idempotent RG Type Publication

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-rg-type-schema-idempotent-registration`

AM's manifest **MUST** include both declarations in §3 with their existing identifiers and traits, per `fr-user-group-rg-type`. The shared publisher **MUST** await admission before application `init`. Identical restarts **MUST** preserve revisions; absent or differing documents **MUST** follow the registry's reconciliation and admission rules. A rejected or incomplete publication **MUST** stop startup. AM **MUST NOT** maintain an RG-local type catalogue or register types through `ResourceGroupClient`.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`
- `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`

**Touches**:

- Data: `UserGroupRgTypeV1` and `UserMemberRgTypeV1` in AM's crate-local GTS inventory; [user_group.v1.schema.json](../schemas/user_group.v1.schema.json) is a documentation copy, not a runtime publication input.
- Sibling integration: shared ToolKit startup publisher, `PlatformTypesRegistryApi`; RG consumes the definitions through `TypesRegistryApi`.
- Errors: shared publication diagnostics preserve the underlying registry failure and identify the publisher and affected identifiers.

### Cascade Cleanup Trigger at Tenant Hard-Delete

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-cascade-cleanup-trigger`

The system **MUST** trigger `ResourceGroupClient` cleanup of the tenant's user-group subtree during tenant hard-deletion before the `tenants` row is removed. If the RG cleanup call fails transport-level, the hard-delete flow **MUST** abort with `code=service_unavailable` via the `feature-errors-observability` envelope and the `tenants` row **MUST NOT** be removed — the retention job retries on its next tick. If RG returns a provider-level error, the hard-delete flow **MUST** abort and surface the mapped error; the `tenants` row remains for retry. AM **MUST NOT** perform RG-side cleanup work itself — the trigger is a pass-through call and the actual work is owned by Resource Group.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Touches**:

- Entities: `UserGroup` (delegated view), `UserGroupMembership` (delegated adapter)
- Sibling integration: `ResourceGroupClient`; `feature-tenant-hierarchy-management` hard-delete flow (caller)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `service_unavailable` code referenced by name only.

### Delegation Boundary

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-delegation-boundary`

The system **MUST NOT** expose any AM-side REST endpoint for user-group CRUD, membership add/remove, or nested-group traversal — consumers call `ResourceGroupClient` directly per `principle-delegation-to-rg`. The AM OpenAPI surface **MUST NOT** contain a `/user-groups` family. AM **MUST NOT** own any `user_group_*` or `user_group_membership_*` storage table, any membership adapter table, or any group-hierarchy cache. AM **MUST NOT** proxy, coordinate, or observe individual RG CRUD / membership calls beyond the two authorized touchpoints (startup type publication + hard-delete cascade cleanup). Cycle detection for nested groups **MUST** be enforced by Resource Group's forest invariants — AM does NOT re-implement it.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`
- `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Touches**:

- Entities: `UserGroup`, `UserGroupMembership`
- Sibling integration: `ResourceGroupClient`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### Membership User-Existence Pattern

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-membership-user-existence-pattern`

Membership-write callers **MUST** combine AM's `GET /tenants/{tenant_id}/users` (owned by `feature-idp-user-operations-contract`, with `?user_id=<id>` for point checks) with `ResourceGroupClient` membership operations: caller verifies user existence via AM's user-list surface first, then invokes `ResourceGroupClient` to add the user to a group. AM **MUST NOT** introduce any composite or convenience endpoint that wraps these two steps — the delegation principle requires consumers to call both surfaces directly so the authoritative user-existence signal and the authoritative membership write stay with their respective owners. RG enforces `allowed_membership_types = [gts.cf.core.rg.type.v1~cf.core.am.user.v1~]` on the published user-group type; this member handle wraps the AM user resource type. User existence remains the caller’s separate AM check.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`

**Touches**:

- Sibling integration: `feature-idp-user-operations-contract` (`GET /tenants/{tenant_id}/users` surface); `ResourceGroupClient` (membership operations)

## 6. Acceptance Criteria

- [ ] On startup with no AM user-group definitions in Types Registry, AM's manifest publishes both declarations from §3 and awaits their admission against the RG base before application `init`. RG can create root and nested user groups, accept AM-user memberships under the member-handle type, and accept unconstrained metadata. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] On an identical restart, publication succeeds without changing either type's revision. A differing document follows the registry's revision and admission rules. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] If publication is rejected, cancelled or cannot complete within its startup budget, application `init` does not run; diagnostics identify the publisher, affected identifiers and underlying cause. No RG type-management API or RG-local type table is used. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] During tenant hard-deletion invoked by the retention job in `feature-tenant-hierarchy-management`, AM calls `ResourceGroupClient` to delete the tenant's user-group subtree before the `tenants` row is removed; if the RG cleanup call fails transport-level, the hard-delete flow aborts with `code=service_unavailable` via the `feature-errors-observability` envelope and the `tenants` row is NOT removed. On the next retry, the hard-delete flow re-attempts cleanup. Fingerprints `dod-user-groups-cascade-cleanup-trigger`.
- [ ] The AM OpenAPI spec contains NO `/user-groups` family of endpoints and NO `/memberships` family — user-group CRUD, membership add/remove, and nested-group operations are not part of AM's REST surface. The AM gear contains no `user_group_*` or `user_group_membership_*` tables, no adapter tables, and no in-memory group-hierarchy cache; RG is the single storage owner. Fingerprints `dod-user-groups-delegation-boundary`.
- [ ] A membership-write consumer (e.g., `feature-user-groups` caller) combining `GET /tenants/{tenant_id}/users?user_id=<id>` with a `ResourceGroupClient` membership add returns the authoritative user-existence signal before the RG membership write is issued; AM does NOT expose a convenience endpoint that wraps the two-step pattern, and the user-group type's `allowed_membership_types` restricting membership to the AM user member-handle `gts.cf.core.rg.type.v1~cf.core.am.user.v1~` is the RG-side integrity guard. Fingerprints `dod-user-groups-membership-user-existence-pattern`, `dod-user-groups-delegation-boundary`.
- [ ] Nested user-group cycles (e.g., group `A` as a parent of group `B` and `B` as a parent of `A`) are refused by Resource Group forest invariants at the `ResourceGroupClient` boundary; AM's `allowed_parent_types` trait on the published type permits only the same chained user-group type as parent, so the RG-side cycle check is the authoritative enforcement. AM performs NO cycle detection of its own. Fingerprints `dod-user-groups-delegation-boundary`.

## 7. Deliberate Omissions

- **Resource Group storage (`user_group_*` and `user_group_membership_*` tables) and any other user-group persistence** — *Owned by the Resource Group gear* (DECOMPOSITION §2.6 scope). AM stores no user-group rows, no membership adapter tables, and no group-hierarchy cache.
- **The Resource Group engine itself (generic RG CRUD, cascade engine, forest invariants, cycle detection, tenant-scoped isolation enforcement)** — *Owned by the Resource Group gear.* AM only delegates to it via `ResourceGroupClient`.
- **Type storage, revisions and admission** — *Owned by Types Registry.* AM supplies its declarations through the shared startup publisher; RG reads their definitions for write validation.
- **REST endpoints for group create / update / delete, membership add / remove, nested-group traversal** — *Not part of the AM OpenAPI surface* (DECOMPOSITION §2.6 API: _none_). Consumers call `ResourceGroupClient` directly per `principle-delegation-to-rg`.
- **User identity operations (provisioning, deprovisioning, existence checks)** — *Owned by `cpt-cf-account-management-feature-idp-user-operations-contract`* (DECOMPOSITION §2.5). This feature consumes that feature's `GET /tenants/{tenant_id}/users` surface for documented user-existence combination patterns but does not reimplement it.
- **Tenant lifecycle, tenant hierarchy, and tenant-closure ownership** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). This feature is invoked from that feature's hard-delete flow but does not participate in tenant CRUD or closure maintenance itself.
- **AuthZ policy evaluation for user-group and membership operations** — *Inherited from `PolicyEnforcer` at the RG REST layer* (DESIGN §4.2; RG-side concern). AM makes no authorization decisions at the user-group boundary.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, metric catalog** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). Runtime cleanup errors defer envelope formatting, HTTP status mapping, audit emission, and metric sample naming to that feature. Startup publication failures use the shared publisher’s diagnostics and preserve the registry cause.
