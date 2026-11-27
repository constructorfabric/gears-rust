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

Resource Group owns the user-group hierarchy, memberships and tenant isolation.
AM publishes the group and membership schemas before application `init`,
triggers RG cleanup at tenant hard-deletion and exposes tenant-scoped user
lookups.

### 1.2 Purpose

Implements PRD §5.6 under `principle-delegation-to-rg` and
`adr-resource-group-tenant-hierarchy-source`. Consumers use `ResourceGroupClient`
for groups and memberships and check user existence through
`GET /tenants/{tenant_id}/users?user_id=<id>`
(`feature-idp-user-operations-contract`). AM publishes its declarations
under `fr-user-group-rg-type` and triggers cleanup to remove the tenant's
user-group subtree.

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

- Before application `init`, the runtime publishes AM's manifest through `PlatformTypesRegistryApi`. It includes the user-group and membership schemas from [rg_types.rs](../../account-management/src/gts/rg_types.rs) and the AM user resource schema from the SDK. Resource Group's manifest publishes the group and membership base schemas.
- Identical restarts preserve revisions. The registry admits new definitions
  and reconciles changes under its revision rules.

**Error Scenarios**:

- A refusal or failure to finish within the startup budget stops application
  `init`. The shared publisher reports the publisher, affected IDs and cause,
  including unresolved dependencies, unavailability and transient conflicts.

**Steps**:

1. [ ] - `p1` - Build AM's manifest from `gts_publication(crates = [crate, account_management_sdk], entities = Self::configured_entities)` - `inst-flow-rgreg-build-manifest`
2. [ ] - `p1` - Invoke `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration` through the shared startup publisher - `inst-flow-rgreg-invoke-algo`
3. [ ] - `p1` - **IF** every manifest document is admitted, allow application `init` to proceed - `inst-flow-rgreg-admitted`
4. [ ] - `p1` - **ELSE** propagate the publication failure and stop startup before application `init` - `inst-flow-rgreg-failed`

### Tenant Hard-Delete Cascade Cleanup Trigger

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Actor**: `cpt-cf-account-management-actor-platform-admin`

**Success Scenarios**:

- During tenant hard-deletion (invoked by the retention job owned by `feature-tenant-hierarchy-management`), AM calls `ResourceGroupTenantCleanup::delete_tenant_groups` (RG's un-gated tenant-cleanup surface — the tenant is already `Deleted`, so no PDP grant can address its rows) to delete the tenant's user groups with their subtrees before the `tenants` row is removed. Resource Group is the authoritative owner of the cleanup mechanics — AM only triggers and awaits completion. On success, the hard-delete flow proceeds to the `tenants` row delete.
- Tenant has no user groups — the RG cleanup call is a successful no-op; hard-delete proceeds.

**Error Scenarios**:

- Resource Group is unreachable during cleanup — the hard-delete flow is aborted with `service_unavailable` per DESIGN §3.4 ("If RG is unavailable during deletion validation, AM fails the operation with `service_unavailable` rather than proceeding"); the `tenants` row is NOT removed. The retention job retries on its next tick.
- Resource Group cleanup fails for a provider-specific reason — propagated through the `feature-errors-observability` envelope; hard-delete is aborted and the `tenants` row remains for the next retry.

**Steps**:

1. [ ] - `p1` - Called by the hard-delete flow in `feature-tenant-hierarchy-management` with `{tenant_id}` just before the `tenants` row delete - `inst-flow-cascade-entry`
2. [ ] - `p1` - Invoke `ResourceGroupTenantCleanup::delete_tenant_groups` to delete the tenant's user groups with their subtrees (groups of type `gts.cf.core.rg.group.v1~cf.core.am.user_group.v1~` scoped to `{tenant_id}`) - `inst-flow-cascade-invoke-rg`
3. [ ] - `p1` - **IF** RG call failed transport-level (unreachable or timeout) - `inst-flow-cascade-unavailable-branch`
   1. [ ] - `p1` - **RETURN** `(reject, code=service_unavailable)` via the `feature-errors-observability` envelope; hard-delete flow aborts and `tenants` row is NOT removed - `inst-flow-cascade-unavailable-return`
4. [ ] - `p1` - **IF** RG call returned a provider-level error - `inst-flow-cascade-provider-error-branch`
   1. [ ] - `p1` - **RETURN** the mapped provider error via the envelope; hard-delete flow aborts and `tenants` row remains for retry - `inst-flow-cascade-provider-error-return`
5. [ ] - `p1` - **RETURN** success so the hard-delete flow can proceed to remove the `tenants` row - `inst-flow-cascade-success-return`

## 3. Processes / Business Logic (CDSL)

### User Group RG Type Manifest Publication

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`

The `rg-type-registration` and `rg-type-schema-registration` IDs track
manifest publication.

**Input**: AM's manifest, including the following declarations:

| Declaration | GTS identifier | Placement and membership |
|-------------|----------------|---------------------------------|
| `UserGroupTypeV1` | `gts.cf.core.rg.group.v1~cf.core.am.user_group.v1~` | `parent_type` accepts roots and parents of the exact AM user-group type. The `membership_type` trait binds the user membership schema. `metadata` may be omitted or empty. |
| `UserV1` | `gts.cf.core.am.user.v1~` | AM user resource type used as the membership's `resource_type`. |
| [User membership schema](../schemas/user_group_member.v1.schema.json) | `gts.cf.core.rg.group_membership.v1~cf.core.am.user_group_member.v1~` | Accepts the AM user resource type and its derived types as `resource_type` through `x-gts-type-ref`. |

**Output**: Successful publication (`UpToDate`, or `Reconciled` with every desired identifier `Admitted`), or a publication failure that stops startup.

**Steps**:

1. [ ] - `p1` - Collect the declarations from AM's crate-local inventory into its manifest; retain the existing user-group identifier and use the AM user resource type for membership - `inst-algo-rgreg-collect`
2. [ ] - `p1` - Publish through the shared Types Registry SDK publisher using `PlatformTypesRegistryApi`; reconcile absent or differing documents under registry admission and revision rules - `inst-algo-rgreg-publish`
3. [ ] - `p1` - Await admission of every desired identifier, including the RG bases and the AM user resource schema; retry transient failures and pending dependencies within the shared startup budget - `inst-algo-rgreg-await`
4. [ ] - `p1` - **IF** publication succeeds, **RETURN** success; identical restarts preserve existing revisions - `inst-algo-rgreg-success`
5. [ ] - `p1` - **ELSE RETURN** the publication error with its publisher, identifier and cause context; application `init` does not run - `inst-algo-rgreg-failure`

RG reads definitions under the caller's context through `TypesRegistryApi`.
See [Types Registry SDK publication](../../../types-registry/types-registry-sdk/README.md#publishing-from-a-gear)
for reconciliation, retries and diagnostics.

## 4. States (CDSL)

Types Registry owns type lifecycle and revisions. RG owns group and
membership state. AM contributes declarations and delegates hard-delete
cleanup to RG.

## 5. Definitions of Done

### Idempotent RG Type Publication

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-rg-type-schema-idempotent-registration`

AM **MUST** publish the §3 declarations under `fr-user-group-rg-type` and
await admission before application `init`. Identical restarts **MUST**
preserve revisions. New or changed declarations follow registry admission
rules. Refused or incomplete publication **MUST** stop startup. AM **MUST
NOT** register types through `ResourceGroupClient` or maintain a local
type catalogue.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`
- `cpt-cf-account-management-algo-user-groups-rg-type-schema-registration`

**Touches**:

- Data: `UserGroupTypeV1` and `UserGroupMembershipV1` in AM's crate-local GTS inventory, and `UserV1` in the SDK inventory. [user_group.v1.schema.json](../schemas/user_group.v1.schema.json) documents the published group schema.
- Sibling integration: shared ToolKit startup publisher, `PlatformTypesRegistryApi`; RG consumes the definitions through `TypesRegistryApi`.
- Errors: shared publication diagnostics preserve the underlying registry failure and identify the publisher and affected identifiers.

### Cascade Cleanup Trigger at Tenant Hard-Delete

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-user-groups-cascade-cleanup-trigger`

The system **MUST** trigger Resource Group cleanup (`ResourceGroupTenantCleanup`) of the tenant's user-group subtree during tenant hard-deletion before the `tenants` row is removed. If the RG cleanup call fails transport-level, the hard-delete flow **MUST** abort with `code=service_unavailable` via the `feature-errors-observability` envelope and the `tenants` row **MUST NOT** be removed — the retention job retries on its next tick. If RG returns a provider-level error, the hard-delete flow **MUST** abort and surface the mapped error; the `tenants` row remains for retry. AM **MUST NOT** perform RG-side cleanup work itself — the trigger is a pass-through call and the actual work is owned by Resource Group.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`

**Touches**:

- Entities: `UserGroup` (delegated view), `UserGroupMembership` (delegated adapter)
- Sibling integration: `ResourceGroupTenantCleanup`; `feature-tenant-hierarchy-management` hard-delete flow (caller)
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

Membership-write callers **MUST** combine AM's `GET /tenants/{tenant_id}/users` (owned by `feature-idp-user-operations-contract`, with `?user_id=<id>` for point checks) with `ResourceGroupClient` membership operations: caller verifies user existence via AM's user-list surface first, then invokes `ResourceGroupClient` to add the user to a group. AM **MUST NOT** introduce any composite or convenience endpoint that wraps these two steps — the delegation principle requires consumers to call both surfaces directly so the authoritative user-existence signal and the authoritative membership write stay with their respective owners. RG validates the member's type against the bound membership schema, which admits the AM user type family. User existence remains the caller’s separate AM check.

**Implements**:

- `cpt-cf-account-management-flow-user-groups-rg-type-registration`

**Touches**:

- Sibling integration: `feature-idp-user-operations-contract` (`GET /tenants/{tenant_id}/users` surface); `ResourceGroupClient` (membership operations)

## 6. Acceptance Criteria

- [ ] On startup with no AM user-group definitions in Types Registry, AM's manifest publishes the declarations from §3 and awaits their admission before application `init`. RG accepts root groups and groups nested under the exact AM user-group type. It accepts AM-user memberships and rejects unknown metadata fields. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] On an identical restart, publication succeeds without changing either type's revision. A differing document follows the registry's revision and admission rules. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] If publication is rejected, cancelled or cannot complete within its startup budget, application `init` does not run; diagnostics identify the publisher, affected identifiers and underlying cause. No RG type-management API or RG-local type table is used. Fingerprints `dod-user-groups-rg-type-schema-idempotent-registration`.
- [ ] During tenant hard-deletion invoked by the retention job in `feature-tenant-hierarchy-management`, AM calls `ResourceGroupTenantCleanup` to delete the tenant's user-group subtree before the `tenants` row is removed; if the RG cleanup call fails transport-level, the hard-delete flow aborts with `code=service_unavailable` via the `feature-errors-observability` envelope and the `tenants` row is NOT removed. On the next retry, the hard-delete flow re-attempts cleanup. Fingerprints `dod-user-groups-cascade-cleanup-trigger`.
- [ ] The AM OpenAPI spec contains NO `/user-groups` family of endpoints and NO `/memberships` family — user-group CRUD, membership add/remove, and nested-group operations are not part of AM's REST surface. The AM gear contains no `user_group_*` or `user_group_membership_*` tables, no adapter tables, and no in-memory group-hierarchy cache; RG is the single storage owner. Fingerprints `dod-user-groups-delegation-boundary`.
- [ ] A membership-write consumer checks `GET /tenants/{tenant_id}/users?user_id=<id>` before adding the AM user via `ResourceGroupClient`. RG validates the member's type against the bound membership schema. Fingerprints `dod-user-groups-membership-user-existence-pattern`, `dod-user-groups-delegation-boundary`.
- [ ] Resource Group forest invariants reject nested user-group cycles at the `ResourceGroupClient` boundary, including cycles between groups of the same AM user-group type. Fingerprints `dod-user-groups-delegation-boundary`.

## 7. Deliberate Omissions

- **Resource Group storage (`user_group_*` and `user_group_membership_*` tables) and any other user-group persistence** — *Owned by the Resource Group gear* (DECOMPOSITION §2.6 scope). AM stores no user-group rows, no membership adapter tables, and no group-hierarchy cache.
- **The Resource Group engine itself (generic RG CRUD, cascade engine, forest invariants, cycle detection, tenant-scoped isolation enforcement)** — *Owned by the Resource Group gear.* AM only delegates to it via `ResourceGroupClient`.
- **Type storage, revisions and admission** — *Owned by Types Registry.* AM supplies its declarations through the shared startup publisher; RG reads their definitions for write validation.
- **REST endpoints for group create / update / delete, membership add / remove, nested-group traversal** — *Not part of the AM OpenAPI surface* (DECOMPOSITION §2.6 API: _none_). Consumers call `ResourceGroupClient` directly per `principle-delegation-to-rg`.
- **User identity operations (provisioning, deprovisioning, existence checks)** — *Owned by `cpt-cf-account-management-feature-idp-user-operations-contract`* (DECOMPOSITION §2.5). This feature consumes that feature's `GET /tenants/{tenant_id}/users` surface for documented user-existence combination patterns but does not reimplement it.
- **Tenant lifecycle, tenant hierarchy, and tenant-closure ownership** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). This feature is invoked from that feature's hard-delete flow but does not participate in tenant CRUD or closure maintenance itself.
- **AuthZ policy evaluation for user-group and membership operations** — *Inherited from `PolicyEnforcer` at the RG REST layer* (DESIGN §4.2; RG-side concern). AM makes no authorization decisions at the user-group boundary.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, metric catalog** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). Runtime cleanup errors defer envelope formatting, HTTP status mapping, audit emission, and metric sample naming to that feature. Startup publication failures use the shared publisher’s diagnostics and preserve the registry cause.
