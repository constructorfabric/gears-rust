# Feature: Tenant Hierarchy Management


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create Child Tenant](#create-child-tenant)
  - [Read Tenant Details](#read-tenant-details)
  - [List Children (Paginated, Status-Filterable)](#list-children-paginated-status-filterable)
  - [Update Tenant Mutable Fields](#update-tenant-mutable-fields)
  - [Soft-Delete Tenant](#soft-delete-tenant)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Create-Tenant Saga](#create-tenant-saga)
  - [Closure-Table Maintenance](#closure-table-maintenance)
  - [Depth-Threshold Evaluation](#depth-threshold-evaluation)
  - [Soft-Delete Preconditions](#soft-delete-preconditions)
  - [Hard-Delete Leaf-First Scheduler](#hard-delete-leaf-first-scheduler)
  - [Provisioning Reaper Compensation](#provisioning-reaper-compensation)
  - [Hierarchy-Integrity Check](#hierarchy-integrity-check)
- [4. States (CDSL)](#4-states-cdsl)
  - [TenantStatus](#tenantstatus)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Create-Child-Tenant Saga](#create-child-tenant-saga)
  - [Closure-Table Invariants](#closure-table-invariants)
  - [Depth-Threshold Enforcement (Advisory + Strict)](#depth-threshold-enforcement-advisory--strict)
  - [Status Change Is Non-Cascading](#status-change-is-non-cascading)
  - [Tenant-Update Mutable-Fields-Only Guard](#tenant-update-mutable-fields-only-guard)
  - [Soft-Delete Preconditions](#soft-delete-preconditions-1)
  - [Hard-Delete Leaf-First Ordering](#hard-delete-leaf-first-ordering)
  - [Tenant-Read Scope](#tenant-read-scope)
  - [Children-Query Pagination](#children-query-pagination)
  - [IdP Tenant-Provision Contract](#idp-tenant-provision-contract)
  - [IdP Tenant-Provisioning-Failure Contract](#idp-tenant-provisioning-failure-contract)
  - [IdP Tenant-Deprovision Contract](#idp-tenant-deprovision-contract)
  - [Hierarchy-Integrity Diagnostics](#hierarchy-integrity-diagnostics)
  - [Data Remediation Telemetry + Documented Path](#data-remediation-telemetry--documented-path)
  - [Data Lifecycle — Soft/Hard Delete + IdP Deprovision](#data-lifecycle--softhard-delete--idp-deprovision)
  - [Production Scale Envelope](#production-scale-envelope)
  - [Concurrency Serializability](#concurrency-serializability)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)
- [8. Hierarchy Integrity Check](#8-hierarchy-integrity-check)
  - [Classifier Catalog](#classifier-catalog)
  - [Snapshot Consistency](#snapshot-consistency)
  - [Single-Flight](#single-flight)
  - [Periodic In-Process Job](#periodic-in-process-job)
  - [Test Strategy](#test-strategy)
  - [Removed Surface (post-refactor)](#removed-surface-post-refactor)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-tenant-hierarchy-management`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-tenant-hierarchy-management`

## 1. Feature Context

### 1.1 Overview

Full lifecycle of tenants inside the canonical tree owned by Account Management — create child tenants, read and list children, enforce a configurable advisory depth threshold (with an opt-in strict mode), transition status between `active` and `suspended`, soft-delete (leaf-first, with retention window) and hard-delete, and transactionally maintain the platform-canonical `tenant_closure` table so every downstream reader observes tree and closure as one consistent state. Tenant-side IdP operations (provision on create, deprovision on hard-delete, and provision-failure reconciliation) are first-class side effects of this feature's CRUD paths.

### 1.2 Purpose

Provides the core tenant CRUD surface the platform is built around: the hierarchy the root bootstrap establishes, the sub-tree every other feature reasons over (mode conversions, metadata, user operations, tenant-resolver plugin), and the canonical transitive-ancestry storage that lets barrier-aware readers answer subtree and ancestor queries in a single indexed lookup. Soft-delete plus retention + leaf-first hard-delete keep the tree referentially sound through tenant end-of-life.

**Requirements**: `cpt-cf-account-management-fr-create-child-tenant`, `cpt-cf-account-management-fr-hierarchy-depth-limit`, `cpt-cf-account-management-fr-tenant-status-change`, `cpt-cf-account-management-fr-tenant-soft-delete`, `cpt-cf-account-management-fr-children-query`, `cpt-cf-account-management-fr-tenant-read`, `cpt-cf-account-management-fr-tenant-update`, `cpt-cf-account-management-fr-tenant-closure`, `cpt-cf-account-management-fr-idp-tenant-provision`, `cpt-cf-account-management-fr-idp-tenant-provision-failure`, `cpt-cf-account-management-fr-idp-tenant-deprovision`, `cpt-cf-account-management-nfr-production-scale`, `cpt-cf-account-management-nfr-data-lifecycle`, `cpt-cf-account-management-nfr-data-quality`, `cpt-cf-account-management-nfr-data-integrity-diagnostics`, `cpt-cf-account-management-nfr-data-remediation`

**Principles**: `cpt-cf-account-management-principle-source-of-truth`, `cpt-cf-account-management-principle-tree-invariant`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Primary lifecycle caller — creates child tenants, reads/lists, updates mutable fields, changes status, initiates soft-delete. |
| `cpt-cf-account-management-actor-platform-admin` | Cross-tenant operator for deletion, retention oversight, and root-scoped reads not reachable by tenant-admin scope. |
| `cpt-cf-account-management-actor-idp` | Downstream provider invoked by the create saga (`provision_tenant`), by the provisioning reaper (`deprovision_tenant` on compensation), and by hard-delete (`deprovision_tenant`). |
| `cpt-cf-account-management-actor-tenant-resolver` | Read-only consumer of the `tenant_closure` output — reads via a dedicated database role, not through this feature's algorithms; referenced to anchor the data-publication contract. |
| `cpt-cf-account-management-actor-authz-resolver` | Read-only consumer of barrier and status columns in `tenant_closure` for policy evaluation; referenced to anchor the publication contract. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.2 Tenant Hierarchy Management (§5.2 concurrency cross-cut + create-child / depth-limit / status-change / soft-delete / children-query / read / update / closure), §5.5 IdP Tenant & User Operations Contract (§5.5 tenant-provision / provision-failure / deprovision), §6.8 Expected Production Scale, §6.11 Data Lifecycle, §6.12 Data Quality + §6.12.1 Data Integrity Diagnostics + §6.12.2 Data Remediation Expectations.
- **Design**: [DESIGN.md](../DESIGN.md) §3.1 Domain Model (Tenant, TenantStatus, TenantClosure invariants), §3.2 Component Model `TenantService` (+ Diagnostic Capabilities), §3.3 API Contracts (Tenant Management REST API), §3.6 Interactions & Sequences `seq-create-child`, §3.7 Database schemas & tables (`dbtable-tenants`, `dbtable-tenant-closure`).
- **ADRs**: [ADR 0004](../ADR/0004-cpt-cf-account-management-adr-resource-group-tenant-hierarchy-source.md) — Resource Group consumes AM as tenant-hierarchy source-of-truth; [ADR 0007](../ADR/0007-cpt-cf-account-management-adr-provisioning-excluded-from-closure.md) — `provisioning` tenants excluded from `tenant_closure`.
- **OpenAPI**: [account-management-v1.yaml](../account-management-v1.yaml) — authoritative wire contract for the five tenant endpoints.
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.2 Tenant Hierarchy Management.
- **Dependencies**: `cpt-cf-account-management-feature-platform-bootstrap` (the root tenant must exist before any child-tenant lifecycle operation can run), `cpt-cf-account-management-feature-errors-observability` (error taxonomy, audit, and metric families emitted by this feature).

## 2. Actor Flows (CDSL)

### Create Child Tenant

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-hierarchy-management-create-child-tenant`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- New child tenant is persisted with `status=active`, `tenant_closure` rows inserted atomically at activation (self-row + one row per strict ancestor; `barrier` materialized from `self_managed` along the path), IdP-side tenant resources provisioned, and any provider-returned metadata persisted.
- Self-managed child creation (`self_managed=true` at create time) succeeds without a `ConversionRequest` because the parent's explicit create call is the consent per `managed-self-managed-modes` boundary.
- Advisory-mode depth-threshold exceedance surfaces an operator-visible warning signal (metric + structured log) and creation proceeds.

**Error Scenarios**:

- Parent is not `active` → `FailedPrecondition` (child creation under a suspended or deleted parent is rejected).
- Tenant-type validation fails (invalid type, parent type not in `allowed_parent_types`) → classified at `tenant-type-enforcement`'s boundary; surfaced here as `InvalidArgument` (`reason=INVALID_TENANT_TYPE`) or `FailedPrecondition` (`reason=TYPE_NOT_ALLOWED`) without modification.
- Strict-mode depth exceedance → `FailedPrecondition` (HTTP 400) with `reason=TENANT_DEPTH_EXCEEDED`.
- IdP `provision_tenant` fails with a clean compensable error → compensating transaction deletes the `provisioning` row; caller receives `ServiceUnavailable` (HTTP 503).
- Finalization transaction (step 3 of the saga) fails after IdP success → tenant remains in internal `provisioning`, SDK-invisible; the provisioning reaper compensates; caller receives `Internal` (HTTP 500).

**Steps**:

1. [ ] - `p1` - Validate caller's `SecurityContext` and authorization scope against the target parent tenant - `inst-flow-create-validate-caller`
2. [ ] - `p1` - Invoke `algo-create-tenant-saga` with the validated request - `inst-flow-create-invoke-saga`
3. [ ] - `p1` - **IF** saga returned success - `inst-flow-create-saga-ok`
   1. [ ] - `p1` - **RETURN** `201 Created` with the tenant response body (id, parent_id, tenant_type, status, self_managed, depth, name, timestamps) - `inst-flow-create-return-201`
4. [ ] - `p1` - **ELSE IF** saga returned a compensated IdP failure - `inst-flow-create-saga-idp-fail`
   1. [ ] - `p1` - **RETURN** `CanonicalError::ServiceUnavailable` (HTTP 503) per the cross-cutting envelope - `inst-flow-create-return-503`
5. [ ] - `p1` - **ELSE** saga returned a non-compensable or finalization failure - `inst-flow-create-saga-other-fail`
   1. [ ] - `p1` - **RETURN** the mapped error per `errors-observability` envelope (`CanonicalError::InvalidArgument` / `CanonicalError::FailedPrecondition` / `CanonicalError::Internal`), preserving diagnostic detail in the audit trail - `inst-flow-create-return-other`

### Read Tenant Details

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-hierarchy-management-read-tenant`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authorized caller reads the tenant row — identifier, parent reference, type (re-hydrated from Types Registry), status, mode (`self_managed`), depth, name, and administrative timestamps — for any tenant inside the caller's authorized scope.
- Platform admin reads root or any tenant across the hierarchy per the `Tenant.read` action allowed by platform AuthZ.

**Error Scenarios**:

- Tenant not found or SDK-invisible (`provisioning` status) → `CanonicalError::NotFound` (HTTP 404).
- Cross-tenant access outside the caller's scope → `CanonicalError::PermissionDenied` (HTTP 403, `reason=CROSS_TENANT_DENIED`; owned by `errors-observability` envelope; AuthZ Resolver evaluates the barrier).

**Steps**:

1. [ ] - `p1` - Validate caller's `SecurityContext` - `inst-flow-read-validate-caller`
2. [ ] - `p1` - Resolve the target tenant from `dbtable-tenants` excluding `provisioning` rows (those are SDK-invisible per §3.1 TenantStatus) - `inst-flow-read-resolve`
3. [ ] - `p1` - **IF** tenant is not present or is in internal `provisioning` state - `inst-flow-read-not-found`
   1. [ ] - `p1` - **RETURN** `CanonicalError::NotFound` (HTTP 404) - `inst-flow-read-return-404`
4. [ ] - `p1` - Re-hydrate the public chained `tenant_type` identifier from the Types Registry - `inst-flow-read-hydrate-type`
5. [ ] - `p1` - **RETURN** `200` with the tenant response body - `inst-flow-read-return-200`

### List Children (Paginated, Status-Filterable)

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-hierarchy-management-list-children`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Caller retrieves the direct children of a given tenant with pagination cursors and optional `status` filter (`active`, `suspended`, `deleted`; `provisioning` is never surfaced).

**Error Scenarios**:

- Parent tenant not found → `CanonicalError::NotFound` (HTTP 404).
- Cross-tenant listing outside the caller's scope → `CanonicalError::PermissionDenied` (HTTP 403, `reason=CROSS_TENANT_DENIED`).

**Steps**:

1. [ ] - `p1` - Validate caller's `SecurityContext` and authorization scope - `inst-flow-listch-validate-caller`
2. [ ] - `p1` - Normalize pagination inputs (cursor, page size capped by platform policy) and optional `status` filter - `inst-flow-listch-normalize`
3. [ ] - `p1` - Query `dbtable-tenants` for direct children (`parent_id = {tenant_id}`) excluding `provisioning` rows, applying the status filter and cursor - `inst-flow-listch-query`
4. [ ] - `p1` - Re-hydrate each row's public `tenant_type` from the Types Registry - `inst-flow-listch-hydrate-type`
5. [ ] - `p1` - **RETURN** `200` with page of children and next-cursor - `inst-flow-listch-return-200`

### Update Tenant Mutable Fields

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-hierarchy-management-update-tenant`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authorized caller updates `name` and/or transitions `status` between `active` and `suspended` via PATCH; closure `descendant_status` is rewritten atomically for every row where this tenant is the descendant when status changes (per `algo-closure-maintenance`).
- Suspend (`active → suspended`) does NOT cascade to children; child tenants stay `active`.
- Unsuspend (`suspended → active`) restores mutability of operations on the tenant itself.

**Error Scenarios**:

- Attempt to modify an immutable field (`id`, `parent_id`, `tenant_type`, `self_managed`, `depth`) → `CanonicalError::InvalidArgument` (HTTP 400).
- Attempt to transition `status=deleted` via PATCH → `CanonicalError::FailedPrecondition` (HTTP 400) — deletion goes through DELETE.
- Attempt to create a child / provision a user / write metadata / initiate a mode conversion on a suspended tenant (enforced at the respective feature's boundary, not here) surfaces as `CanonicalError::FailedPrecondition` (HTTP 400).
- Concurrent status changes on the same tenant resolve deterministically per PRD §5.2 cross-cutting concurrency; losing writer receives `CanonicalError::Aborted` (HTTP 409, `reason=SERIALIZATION_CONFLICT`) after retry-budget exhaustion.

**Steps**:

1. [ ] - `p1` - Validate caller's `SecurityContext` and authorization scope - `inst-flow-update-validate-caller`
2. [ ] - `p1` - Reject the request **IF** the payload references any immutable field - `inst-flow-update-reject-immutable`
3. [ ] - `p1` - **IF** `status` is being changed - `inst-flow-update-status-branch`
   1. [ ] - `p1` - Reject **IF** target status is `deleted` (belongs to DELETE flow) - `inst-flow-update-reject-deleted-via-patch`
   2. [ ] - `p1` - Reject **IF** current status is `deleted` or `provisioning` - `inst-flow-update-reject-terminal-transition`
   3. [ ] - `p1` - Begin transaction; update `tenants.status`; rewrite `tenant_closure.descendant_status` for every row where `descendant_id = {tenant_id}` via `algo-closure-maintenance` status-change branch; commit - `inst-flow-update-status-tx`
4. [ ] - `p1` - **IF** `name` is being changed - `inst-flow-update-name-branch`
   1. [ ] - `p1` - Update `tenants.name` (no closure impact) - `inst-flow-update-name`
5. [ ] - `p1` - **RETURN** `200` with the updated tenant response body - `inst-flow-update-return-200`

### Soft-Delete Tenant

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-hierarchy-management-soft-delete-tenant`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Non-root, childless, resource-free tenant transitions to `status=deleted`; `tenant_closure.descendant_status` updated atomically to `deleted` for every row where this tenant is the descendant; retention timer is scheduled for hard-delete.
- Caller sees the tenant in subsequent `GET` calls with `status=deleted` until the retention period elapses.

**Error Scenarios**:

- Root tenant deletion → `InvalidArgument` (HTTP 400) with `reason=ROOT_TENANT_CANNOT_DELETE`.
- Remaining non-deleted children → `FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_CHILDREN`.
- Remaining Resource-Group-owned resources under the tenant's scope → `FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_RESOURCES`.

**Steps**:

1. [ ] - `p1` - Validate caller's `SecurityContext` and authorization scope - `inst-flow-sdel-validate-caller`
2. [ ] - `p1` - Invoke `algo-soft-delete-preconditions` - `inst-flow-sdel-preconds`
3. [ ] - `p1` - **IF** any precondition fails - `inst-flow-sdel-fail-branch`
   1. [ ] - `p1` - **RETURN** the mapped error per `errors-observability` (`CanonicalError::InvalidArgument` / `CanonicalError::FailedPrecondition` with the appropriate `reason` token) - `inst-flow-sdel-return-error`
4. [ ] - `p1` - Begin transaction; set `tenants.status = deleted`; rewrite `tenant_closure.descendant_status = deleted` for every row where `descendant_id = {tenant_id}` via `algo-closure-maintenance` status-change branch; commit - `inst-flow-sdel-tx`
5. [ ] - `p1` - Schedule the tenant for hard-delete via `algo-hard-delete-leaf-first-scheduler` - `inst-flow-sdel-schedule-hard-delete`
6. [ ] - `p1` - **RETURN** `200` (or `204`) acknowledging the soft-delete - `inst-flow-sdel-return-ok`

## 3. Processes / Business Logic (CDSL)

### Create-Tenant Saga

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga`

**Input**: Validated create request (`parent_id`, `tenant_type`, `name`, `self_managed`), caller identity context.

**Output**: Success with tenant row + closure rows in place and IdP provisioned, OR compensated failure with no residual AM state, OR finalization failure leaving a `provisioning` tenant row that the reaper will compensate asynchronously.

**Steps**:

> This algorithm implements DESIGN `seq-create-child` exactly: short TX to insert `provisioning`, IdP call outside any TX, short TX to finalize `active` + insert closure rows. Closure writes happen ONLY at activation per `fr-tenant-closure` and ADR-0007; no closure rows are written at step 1.

1. [ ] - `p1` - Read parent tenant from `dbtable-tenants`; validate parent `status=active`, parent not SDK-invisible, and caller authorized on parent - `inst-algo-saga-read-parent`
2. [ ] - `p1` - **IF** parent not present OR parent `status ≠ active` - `inst-algo-saga-parent-invalid`
   1. [ ] - `p1` - **RETURN** `CanonicalError::FailedPrecondition` (HTTP 400, parent must be active for child creation per PRD §5.2) - `inst-algo-saga-parent-invalid-return`
3. [ ] - `p1` - Invoke type enforcement (owned by `tenant-type-enforcement`) to validate `tenant_type` is registered, `parent_type` ∈ `allowed_parent_types`, same-type nesting rules satisfied - `inst-algo-saga-type-check`
4. [ ] - `p1` - **IF** type enforcement rejects - `inst-algo-saga-type-reject`
   1. [ ] - `p1` - **RETURN** the mapped error (`InvalidArgument` HTTP 400 with `reason=INVALID_TENANT_TYPE` when type not registered; `FailedPrecondition` HTTP 400 with `reason=TYPE_NOT_ALLOWED` when parent type not in `allowed_parent_types` per DESIGN §3.8) - `inst-algo-saga-type-reject-return`
5. [ ] - `p1` - Invoke `algo-depth-threshold-evaluation` with `parent.depth + 1` - `inst-algo-saga-depth-check`
6. [ ] - `p1` - **IF** depth evaluation returned strict-reject - `inst-algo-saga-depth-reject`
   1. [ ] - `p1` - **RETURN** `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_DEPTH_EXCEEDED` - `inst-algo-saga-depth-return`
7. [ ] - `p1` - **Saga step 1 (short TX)** — insert the tenant row with `status=provisioning`, `parent_id`, `tenant_type`, `self_managed`, `depth = parent.depth + 1`; commit. NO `tenant_closure` rows are written at this step - `inst-algo-saga-step1-insert-provisioning`
8. [ ] - `p1` - **Saga step 2 (no open TX)** — invoke `IdpPluginClient::provision_tenant(child_id, name, type, parent_id, metadata)` - `inst-algo-saga-step2-idp-call`
9. [ ] - `p1` - **IF** step 2 returned a clean provider failure (AM can prove no IdP-side state retained) - `inst-algo-saga-step2-clean-fail`
   1. [ ] - `p1` - **Compensating TX** — delete the `provisioning` row for `{child_id}` (guard on `status=provisioning` to avoid racing an unrelated row); NO closure cleanup because no closure rows were written; commit - `inst-algo-saga-compensate-clean`
   2. [ ] - `p1` - **RETURN** compensated-idp-failure (`idp_unavailable`) per `fr-idp-tenant-provision-failure` - `inst-algo-saga-compensate-return`
10. [ ] - `p1` - **ELSE IF** step 2 returned an ambiguous failure (transport error, timeout, generic 5xx — external outcome may be retained) - `inst-algo-saga-step2-ambiguous-fail`
    1. [ ] - `p1` - Leave the `provisioning` row in place; the provisioning reaper will compensate asynchronously via `algo-provisioning-reaper-compensation` - `inst-algo-saga-ambiguous-defer-reaper`
    2. [ ] - `p1` - **RETURN** `internal` (reconciliation-required) per `fr-idp-tenant-provision-failure`; this path is NOT retry-safe without reconciliation - `inst-algo-saga-ambiguous-return`
11. [ ] - `p1` - **Saga step 3 (short TX) — finalize** - `inst-algo-saga-step3-finalize`
    1. [ ] - `p1` - **IF** the provider returned any metadata entries, insert them into `dbtable-tenant-metadata` (feature `tenant-metadata` owns the schema; this step only persists the rows the provider produced) - `inst-algo-saga-step3-insert-metadata`
    2. [ ] - `p1` - Update `tenants.status = active` - `inst-algo-saga-step3-activate`
    3. [ ] - `p1` - Invoke `algo-closure-maintenance` activation branch to insert the self-row `(child_id, child_id, 0, active)` plus one row per strict ancestor along `parent_id` chain with `barrier` materialized per the canonical rule - `inst-algo-saga-step3-closure-insert`
    4. [ ] - `p1` - Commit - `inst-algo-saga-step3-commit`
12. [ ] - `p1` - **IF** step 3 commit failed - `inst-algo-saga-step3-fail`
    1. [ ] - `p1` - Leave the tenant in `provisioning`; the provisioning reaper will compensate via `algo-provisioning-reaper-compensation` (AM does NOT retry finalization per DESIGN §3.2) - `inst-algo-saga-step3-defer-reaper`
    2. [ ] - `p1` - **RETURN** `internal` - `inst-algo-saga-step3-return`
13. [ ] - `p1` - **RETURN** success with the finalized tenant row - `inst-algo-saga-success-return`

### Closure-Table Maintenance

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance`

**Input**: Transition kind (`activation` / `status-change` / `hard-delete`) + affected tenant identifier + (for activation) ancestor chain + (for status-change) target status.

**Output**: `tenant_closure` rows inserted, updated, or deleted in the same transaction as the owning `tenants` write; closure is either consistent with `tenants` at every commit point, or the transaction rolls back leaving no observable partial state.

**Steps**:

> This algorithm is the single writer of `tenant_closure` for non-root activation, status-change, and hard-delete branches. Bootstrap's root self-row insert is the documented carve-out per ADR-0007 and `feature-platform-bootstrap` saga step 3. Every branch runs inside the OWNING `tenants` transaction — no standalone closure transactions exist. Self-rows always carry `barrier = 0`; strict-ancestor rows carry `barrier = 1` iff some tenant on `(ancestor, descendant]` is `self_managed`. The `descendant_status` domain is `{active, suspended, deleted}` only.

1. [ ] - `p1` - **IF** transition is `activation` (saga step 3 finalizing `provisioning → active`) - `inst-algo-closmnt-activation-branch`
   1. [ ] - `p1` - Insert self-row `(child_id, child_id, barrier=0, descendant_status=active)` - `inst-algo-closmnt-activation-self-row`
   2. [ ] - `p1` - Walk `parent_id` chain from `child_id` up to the root; for each strict ancestor `A`, insert `(A, child_id, barrier, descendant_status=active)` where `barrier = 1` iff any tenant on the strict path `(A, child_id]` has `self_managed = true`, else `0` - `inst-algo-closmnt-activation-ancestor-rows`
2. [ ] - `p1` - **ELSE IF** transition is `status-change` between SDK-visible states (`active` / `suspended` / `deleted`) - `inst-algo-closmnt-status-branch`
   1. [ ] - `p1` - Rewrite `tenant_closure.descendant_status` to `{new_status}` for every row where `{tenant_id}` is the descendant (self-row + every strict-ancestor row; O(depth) update) - `inst-algo-closmnt-status-update`
3. [ ] - `p1` - **ELSE IF** transition is `hard-delete` (leaves only, per `algo-hard-delete-leaf-first-scheduler`) - `inst-algo-closmnt-harddel-branch`
   1. [ ] - `p1` - Remove every `tenant_closure` row where `{tenant_id}` is the descendant (self-row + strict-ancestor rows; O(depth) delete) - `inst-algo-closmnt-harddel`
4. [ ] - `p1` - **ELSE** transition kind is not a closure-affecting event (e.g., `name` update, or compensation of a `provisioning` row) - `inst-algo-closmnt-noop-branch`
   1. [ ] - `p1` - **RETURN** — no closure work; owning transaction proceeds - `inst-algo-closmnt-noop-return`
5. [ ] - `p1` - **RETURN** — closure writes are part of the owning transaction's commit; caller is responsible for committing / rolling back - `inst-algo-closmnt-return`

### Depth-Threshold Evaluation

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-depth-threshold-evaluation`

**Input**: Proposed depth value, configured threshold, strict-mode flag.

**Output**: Evaluation result — either proceed silently, proceed with an advisory warning signal emitted, or strict-reject with `tenant_depth_exceeded`.

**Steps**:

1. [ ] - `p1` - **IF** proposed depth ≤ threshold - `inst-algo-depth-under`
   1. [ ] - `p1` - **RETURN** proceed - `inst-algo-depth-proceed`
2. [ ] - `p1` - **ELSE IF** strict-mode flag is false (advisory mode) - `inst-algo-depth-advisory`
   1. [ ] - `p1` - Emit the advisory warning signal via `errors-observability` `algo-metric-emission` using the `hierarchy_depth_exceedance` metric family (metric increment) - `inst-algo-depth-advisory-metric`
   2. [ ] - `p1` - Emit a structured warning log entry carrying `tenant_id`, `parent_id`, `observed_depth`, `threshold` - `inst-algo-depth-advisory-log`
   3. [ ] - `p1` - **RETURN** proceed - `inst-algo-depth-advisory-return`
3. [ ] - `p1` - **ELSE** strict-mode flag is true - `inst-algo-depth-strict`
   1. [ ] - `p1` - **RETURN** strict-reject (caller translates to `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_DEPTH_EXCEEDED`) - `inst-algo-depth-strict-return`

### Soft-Delete Preconditions

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions`

**Input**: Target `tenant_id`.

**Output**: Pass, or first-failed precondition with its mapped error.

**Steps**:

1. [ ] - `p1` - **IF** target tenant is the root (`parent_id IS NULL`) - `inst-algo-sdelpc-root`
   1. [ ] - `p1` - **RETURN** `CanonicalError::InvalidArgument` (HTTP 400) with `reason=ROOT_TENANT_CANNOT_DELETE` - `inst-algo-sdelpc-root-return`
2. [ ] - `p1` - **IF** target tenant has any non-deleted child — query `dbtable-tenants` for a non-deleted child of `{tenant_id}` (`parent_id={tenant_id}` and `status≠deleted`; `LIMIT 1` existence check) - `inst-algo-sdelpc-children`
   1. [ ] - `p1` - **RETURN** `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_CHILDREN` - `inst-algo-sdelpc-children-return`
3. [ ] - `p1` - Query Resource Group ownership graph for remaining tenant-owned resource associations under `{tenant_id}` scope - `inst-algo-sdelpc-resources-query`
4. [ ] - `p1` - **IF** any resource association remains - `inst-algo-sdelpc-resources`
   1. [ ] - `p1` - **RETURN** `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_RESOURCES` - `inst-algo-sdelpc-resources-return`
5. [ ] - `p1` - **RETURN** pass - `inst-algo-sdelpc-pass`

### Hard-Delete Leaf-First Scheduler

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler`

**Input**: Clock tick (background job invocation); configured retention period (default 90 days).

**Output**: Tenants whose retention window has elapsed are hard-deleted in `depth DESC` order, each with its IdP deprovision hook invoked before row removal; no orphan child rows are left.

**Steps**:

1. [ ] - `p1` - Scan `dbtable-tenants` for rows with `status=deleted` whose soft-delete timestamp + retention period ≤ now - `inst-algo-hdel-scan-due`
2. [ ] - `p1` - **FOR EACH** due tenant, ordered by `depth DESC` (leaf-first) - `inst-algo-hdel-for-each`
   1. [ ] - `p1` - Invoke `IdpPluginClient::deprovision_tenant({tenant_id})` per `fr-idp-tenant-deprovision`; treat already-absent as success - `inst-algo-hdel-idp-deprovision`
   2. [ ] - `p1` - **IF** deprovision returned a terminal failure - `inst-algo-hdel-idp-fail`
      1. [ ] - `p1` - Emit `dependency_health` metric increment with `target=idp` and `op=deprovision_tenant`; emit `actor=system` audit via `errors-observability` `algo-audit-emission`; defer to next tick (do not proceed to DB delete on this tenant) - `inst-algo-hdel-idp-defer`
   3. [ ] - `p1` - **ELSE** begin transaction and re-check whether any child row still references this tenant as its parent under the same write isolation used for the delete - `inst-algo-hdel-child-guard`
      1. [ ] - `p1` - **IF** any child row still exists, roll back or skip the delete, emit the same `dependency_health`/retention telemetry class as a deferred cleanup, and defer the parent to a later tick - `inst-algo-hdel-child-guard-defer`
   4. [ ] - `p1` - **ELSE** invoke `algo-closure-maintenance` hard-delete branch (deletes every `tenant_closure` row where `descendant_id = {tenant_id}`); delete the `tenants` row; commit - `inst-algo-hdel-tx`
   5. [ ] - `p1` - Emit `actor=system` audit for the hard-deletion event via `errors-observability` `algo-audit-emission` - `inst-algo-hdel-audit`
3. [ ] - `p1` - **RETURN** — scheduler yields until the next tick; any tenant deferred due to IdP failure remains eligible on subsequent ticks - `inst-algo-hdel-return`

### Provisioning Reaper Compensation

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-provisioning-reaper-compensation`

**Input**: Clock tick; configured provisioning timeout (default 5 minutes).

**Output**: Stale `provisioning` tenants are compensated via IdP `deprovision_tenant` + `tenants` row deletion only after deprovision succeeds or finds already-absent state; failed deprovision retains the AM row for retry/remediation. No closure cleanup is required because `provisioning` tenants never enter `tenant_closure` per ADR-0007.

**Steps**:

1. [ ] - `p1` - Scan `dbtable-tenants` for rows with `status=provisioning` whose provisioning-start timestamp + provisioning-timeout ≤ now - `inst-algo-reap-scan`
2. [ ] - `p1` - **FOR EACH** stale provisioning tenant - `inst-algo-reap-for-each`
   1. [ ] - `p1` - Invoke `IdpPluginClient::deprovision_tenant({tenant_id})`; idempotent (already-absent is success) per `fr-idp-tenant-deprovision` - `inst-algo-reap-idp-deprovision`
   2. [ ] - `p1` - **IF** deprovision returns retryable or terminal failure - `inst-algo-reap-idp-fail`
      1. [ ] - `p1` - Retain the `provisioning` row, emit `dependency_health` and `tenant_retention` telemetry, and defer this tenant to the next tick or operator remediation. The `actor=system` audit emission required by `cpt-cf-account-management-nfr-audit-completeness` is **deferred to a follow-up** pending the platform append-only audit sink; until then, a structured log on the `am.events` target stands in for the audit envelope - `inst-algo-reap-defer`
   3. [ ] - `p1` - **ELSE** deprovision succeeded or found already-absent state - `inst-algo-reap-idp-ok`
      1. [ ] - `p1` - Begin transaction; delete the `tenants` row for `{tenant_id}` guarded on `status=provisioning` (prevents racing a concurrently-finalized row); commit. NO `tenant_closure` work — no closure rows were ever written for this row - `inst-algo-reap-delete-tx`
   4. [ ] - `p1` - Emit a structured log on the `am.events` target with `kind=provisioningReaperCompensated`, `actor=system`, and the classification of whether IdP deprovision succeeded cleanly or found already-absent state. The full `actor=system` audit record via `errors-observability` `algo-audit-emission` is **deferred to a follow-up** until the platform append-only audit sink lands; the structured log is the v1 stand-in operators correlate with the `tenant_retention` metric - `inst-algo-reap-audit`
   5. [ ] - `p1` - Emit `dependency_health` metric sample (IdP `deprovision_tenant` call) and `tenant_retention` metric sample (compensation-backlog classification) via `errors-observability` `algo-metric-emission` per the catalog naming-alignment contract owned by `dod-ops-metrics-treatment` - `inst-algo-reap-metric`
3. [ ] - `p1` - **RETURN** — reaper yields; AM does NOT retry saga finalization per DESIGN §3.2 - `inst-algo-reap-return`

### Hierarchy-Integrity Check

- [ ] `p2` - **ID**: `cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check`

**Input**: None — runs over the whole hierarchy.

**Output**: Structured diagnostic report assembled from the 8 pure-Rust classifiers enumerated in [Hierarchy Integrity Check](#8-hierarchy-integrity-check) and DESIGN §3.2 Diagnostic Capabilities; per-category metric update on `am.hierarchy_integrity_violations` gauge.

**Steps**:

> Categories per DESIGN §3.2 (Rust classifiers): `orphan`, `cycle`, `depth`, `self_row`, `strict_ancestor`, `extra_edge`, `root`, `barrier`. Each classifier is a synchronous, DB-free function that reads the loaded `Snapshot` and returns a `Vec<Violation>` — classification logic is in Rust, not in SQL.

1. [ ] - `p2` - **Acquire** the single-flight gate by inserting a `(id=1, worker_id, started_at)` row into `integrity_check_runs` inside its own short, committed transaction (`lock::acquire_committed`). The committed write makes the row immediately visible to concurrent workers so they surface `DomainError::IntegrityCheckInProgress` mapped to `CanonicalError::ResourceExhausted` (HTTP 429) per `errors-observability` from their own acquire — the gate row stays committed for the duration of the snapshot/work tx and is **not** held inside it. The acquire transaction also sweeps stale rows (older than `MAX_LOCK_AGE`) so a crashed previous holder cannot block indefinitely. On PK conflict from a live holder, **RETURN** with `IntegrityCheckInProgress` without proceeding to step 2 - `inst-algo-integ-single-flight`
2. [ ] - `p2` - **Open** a separate `REPEATABLE READ` transaction on `PostgreSQL` (transparently mapped to `SERIALIZABLE` on `SQLite` by `modkit-db` per the `TxIsolationLevel` backend-notes contract — `SQLite` does not implement other levels). This snapshot tx is **read-only** (no writes execute inside it; the gate row was committed in step 1 and lives in a different tx), so a long-running classifier pass cannot self-evict on SI conflicts against `tenants` / `tenant_closure` - `inst-algo-integ-snapshot-tx`
3. [ ] - `p2` - Load `tenants` + `tenant_closure` via SecureSelect (`secure().scope_with(...).all(tx)`) within the snapshot tx, so all 8 classifiers observe one consistent `(tenants, tenant_closure)` snapshot - `inst-algo-integ-snapshot-load`
4. [ ] - `p2` - Run the 8 pure-Rust classifiers (`orphan`, `cycle`, `depth`, `self_row`, `strict_ancestor`, `extra_edge`, `root`, `barrier`) over the loaded `Snapshot`. Each returns a `Vec<Violation>` carrying the offending-row fields documented in DESIGN §3.2 - `inst-algo-integ-rust-classifier`
5. [ ] - `p2` - Update the `am.hierarchy_integrity_violations` gauge per `errors-observability` `algo-metric-emission` with a `category` label for each of the 10 fixed-shape categories (8 classifiers — `barrier` and `orphan` each emit two — see DESIGN §3.2 mapping; zero-value where no anomaly detected so alert rules see a known baseline) - `inst-algo-integ-metric`
6. [ ] - `p2` - **Release** the gate by deleting the `integrity_check_runs` row in its own short, committed transaction (`lock::release_committed`), pinned by `worker_id`. Commit happens-before the structured report is returned. A zero-rows-affected DELETE means the row was reclaimed by a stale-lock sweep on a contender's acquire — non-fatal, surfaced as a warn-log only. Release **MUST** run on every post-acquire exit path (Ok return, snapshot/load failure, classifier panic recovery, metric-emission error) — implement as a `try { steps 2-5 } finally { release }` (or equivalent unwind-safe pattern) so a mid-run failure cannot strand the committed gate row until `MAX_LOCK_AGE`, which would otherwise convert one failed check into a sustained stream of `IntegrityCheckInProgress` 429s - `inst-algo-integ-return`

## 4. States (CDSL)

### TenantStatus

- [ ] `p1` - **ID**: `cpt-cf-account-management-state-tenant-hierarchy-management-tenant-status`

**States**: `provisioning`, `active`, `suspended`, `deleted`, `(hard-deleted)` (terminal — row removed)

**Initial State**: `provisioning` (saga step 1)

**SDK visibility**: `active`, `suspended`, `deleted` are SDK-visible; `provisioning` is internal-only and never projected through the public API or the read-only database role consumed by the Tenant Resolver Plugin.

**Transitions**:

1. [ ] - `p1` - **FROM** `provisioning` **TO** `active` **WHEN** saga step 3 (finalization TX) commits; `tenant_closure` rows are inserted in the same TX via `algo-closure-maintenance` activation branch - `inst-state-tenant-status-provisioning-to-active`
2. [ ] - `p1` - **FROM** `provisioning` **TO** `(hard-deleted)` **WHEN** saga step 2 returns a clean compensable failure OR the provisioning reaper compensates a stale provisioning row; the `tenants` row is deleted in the compensating TX and NO closure work occurs because no closure rows were ever written - `inst-state-tenant-status-provisioning-to-removed`
3. [ ] - `p1` - **FROM** `active` **TO** `suspended` **WHEN** administrator invokes PATCH `status=suspended`; closure `descendant_status` rewritten atomically for every row where this tenant is descendant; non-cascading (child tenants stay `active`) - `inst-state-tenant-status-active-to-suspended`
4. [ ] - `p1` - **FROM** `suspended` **TO** `active` **WHEN** administrator invokes PATCH `status=active`; closure `descendant_status` rewritten atomically - `inst-state-tenant-status-suspended-to-active`
5. [ ] - `p1` - **FROM** `active` **TO** `deleted` **WHEN** DELETE succeeds and `algo-soft-delete-preconditions` passes; closure `descendant_status` rewritten atomically to `deleted`; retention timer started - `inst-state-tenant-status-active-to-deleted`
6. [ ] - `p1` - **FROM** `suspended` **TO** `deleted` **WHEN** DELETE succeeds and preconditions pass; closure `descendant_status` rewritten atomically - `inst-state-tenant-status-suspended-to-deleted`
7. [ ] - `p1` - **FROM** `deleted` **TO** `(hard-deleted)` **WHEN** retention period elapses and `algo-hard-delete-leaf-first-scheduler` processes the tenant (leaf-first, IdP deprovision succeeded); closure rows for this tenant are removed atomically with the `tenants` row delete - `inst-state-tenant-status-deleted-to-removed`

**Forbidden transitions**:

- `deleted → active` / `deleted → suspended` — deletion is terminal for the SDK surface until hard-delete removes the row; resurrection is not in v1.
- `* → provisioning` — `provisioning` is the initial state only.
- Any status change to/from `deleted` via PATCH — deletion is reached only through the DELETE flow (`flow-soft-delete-tenant`).

## 5. Definitions of Done

### Create-Child-Tenant Saga

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-create-child-tenant-saga`

The module **MUST** implement child-tenant creation as a three-step saga exactly matching DESIGN `seq-create-child`: (1) short TX inserting the tenant row with `status=provisioning` and NO `tenant_closure` rows; (2) `IdpPluginClient::provision_tenant` call outside any open transaction; (3) short finalization TX persisting any provider-returned metadata, updating `tenants.status=active`, and inserting closure rows via `algo-closure-maintenance` activation branch. IdP failures classified as clean compensable (`idp_unavailable`) **MUST** trigger a compensating TX that deletes the `provisioning` row. Finalization failures after IdP success **MUST** leave the `provisioning` row for the reaper; AM **MUST NOT** retry finalization. `POST /tenants` remains intentionally non-idempotent: only the compensated-`idp_unavailable` path is retry-safe.

**Implements**:

- `cpt-cf-account-management-flow-tenant-hierarchy-management-create-child-tenant`
- `cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga`

**Touches**:

- Component: `cpt-cf-account-management-component-tenant-service`
- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- API: `POST /api/account-management/v1/tenants` (`createTenant`)
- IdP contract: `IdpPluginClient::provision_tenant`
- Sequence: `cpt-cf-account-management-seq-create-child`

### Closure-Table Invariants

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-closure-invariants`

**PR1 scope**: writer-side primitives ship — `domain::tenant::closure::build_activation_rows` enforces self-row + barrier + status-denormalization invariants, and the SeaORM-backed `TenantRepoImpl` writes `tenants` + `tenant_closure` transactionally in `activate_tenant` / `update_tenant_mutable` / `schedule_deletion` / `hard_delete_one`. Service-layer flows (`TenantService`) that drive these primitives, plus the integrity classifier set, land in subsequent PRs.

`tenant_closure` **MUST** be maintained transactionally with the owning `tenants` write at every mutation point; every SDK-visible tenant **MUST** own exactly one self-row `(id, id, barrier=0, descendant_status=<tenants.status>)` and one strict-ancestor row per step up the `parent_id` chain; `barrier = 1` **MUST** materialize whether any tenant on the strict path `(ancestor, descendant]` has `self_managed=true`; `descendant_status` **MUST** denormalize `tenants.status` for the descendant (domain `{active, suspended, deleted}` only — `provisioning` is excluded by construction per ADR-0007). No standalone-closure writes are permitted.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance`

**Touches**:

- DB: `cpt-cf-account-management-dbtable-tenant-closure`, `cpt-cf-account-management-dbtable-tenants`
- ADR: `cpt-cf-account-management-adr-provisioning-excluded-from-closure`

### Depth-Threshold Enforcement (Advisory + Strict)

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-depth-threshold`

The module **MUST** evaluate `parent.depth + 1` against the configured advisory threshold (default 10) at create time via `algo-depth-threshold-evaluation`. In advisory mode, the system **MUST** emit the `hierarchy_depth_exceedance` metric increment plus a structured warning log entry and proceed with creation. In strict mode (operator-opt-in), the system **MUST** reject the creation with `CanonicalError::FailedPrecondition` (HTTP 400) and `reason=TENANT_DEPTH_EXCEEDED`. The data model imposes no hard cap beyond strict mode; production support beyond the benchmarked profile is out of scope until representative benchmarks exist.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-depth-threshold-evaluation`

**Touches**:

- Metric family: `hierarchy_depth_exceedance` (catalog owned by `errors-observability`)
- Component: `cpt-cf-account-management-component-tenant-service`

### Status Change Is Non-Cascading

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-status-change-non-cascading`

**PR1 scope**: domain validator `TenantUpdate::validate_status_transition` rejects PATCH targets `Deleted` / `Provisioning` and current rows in those states with `DomainError::Conflict` (boundary-converted to `CanonicalError::FailedPrecondition`, HTTP 400). The repo-side `update_tenant_mutable` rewrites `tenant_closure.descendant_status` in the same TX as the `tenants.status` flip. REST PATCH handler arrives in a later PR.

PATCH-initiated status changes **MUST** be limited to `active ↔ suspended` transitions on the target tenant only and **MUST NOT** cascade to descendants. Child tenants **MUST** remain fully operational when a parent is suspended. Transitions to `deleted` via PATCH **MUST** be rejected with `CanonicalError::FailedPrecondition` (HTTP 400); the DELETE flow owns soft-delete and enforces the child/resource preconditions. Every status transition **MUST** rewrite `tenant_closure.descendant_status` atomically for every row where this tenant is the descendant, via `algo-closure-maintenance` status-change branch.

**Implements**:

- `cpt-cf-account-management-flow-tenant-hierarchy-management-update-tenant`
- `cpt-cf-account-management-state-tenant-hierarchy-management-tenant-status` (transitions 3, 4)

**Touches**:

- API: `PATCH /api/account-management/v1/tenants/{tenant_id}` (`updateTenant`)
- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`

### Tenant-Update Mutable-Fields-Only Guard

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-update-mutable-only`

**PR1 scope**: `TenantUpdate` carries only `name` + `status` fields; the validators (`validate_name`, `validate_status_transition`) reject everything else at the type / domain level. The repo-side `update_tenant_mutable` further rejects `patch.status = Deleted | Provisioning`. REST surface mapping arrives in a later PR.

The PATCH surface **MUST** accept only `name` and `status` (limited to `active ↔ suspended`); attempts to modify `id`, `parent_id`, `tenant_type`, `self_managed`, or `depth` **MUST** be rejected with `CanonicalError::InvalidArgument` (HTTP 400). Mode changes (toggling `self_managed` post-creation) are rejected at this boundary and belong to `managed-self-managed-modes`' dual-consent flow.

**Implements**:

- `cpt-cf-account-management-flow-tenant-hierarchy-management-update-tenant`

**Touches**:

- API: `PATCH /api/account-management/v1/tenants/{tenant_id}`

### Soft-Delete Preconditions

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-soft-delete-preconditions`

The DELETE flow **MUST** reject root-tenant deletion with `CanonicalError::InvalidArgument` (HTTP 400) and `reason=ROOT_TENANT_CANNOT_DELETE`, reject deletion when any non-deleted child exists with `CanonicalError::FailedPrecondition` (HTTP 400) and `reason=TENANT_HAS_CHILDREN`, and reject deletion when Resource-Group-owned resources remain under the tenant's ownership scope with `CanonicalError::FailedPrecondition` (HTTP 400) and `reason=TENANT_HAS_RESOURCES`. On precondition pass, the tenant transitions to `status=deleted` transactionally with the closure `descendant_status` rewrite, and the hard-delete scheduler is informed.

**Current implementation status (this PR)**:

- Storage-floor primitives are landed: `TenantRepo::schedule_deletion`, `TenantRepo::count_children`. Root detection rides on `TenantModel.parent_id` (root iff `parent_id.is_none()`); no separate `find_root` primitive is needed.
- `TenantService::soft_delete` and the `ResourceOwnershipChecker` port land **in this PR** with the domain-service layer: the saga rejects root-tenant deletion (`RootTenantCannotDelete`), runs the `count_children` guard against `allow_all` (`TenantHasChildren`), invokes `ResourceOwnershipChecker::count_ownership_links(ctx, tenant_id)` (`TenantHasResources`), and only then calls `schedule_deletion`. Production binds `RgResourceOwnershipChecker` (see below); `InertResourceOwnershipChecker` is reserved for unit tests, which construct `TenantService` directly and bypass the module entry-point.
- The `RgResourceOwnershipChecker` production impl ships **in this PR** at `infra/rg/checker.rs`. It issues `list_groups(ctx, $top=1, $filter=tenant_id eq <tenant_id>)` against `ResourceGroupClient` (post `cyberware-rust#1626` `tenant_id` whitelist), wraps the call in a `tokio::time::timeout` (default 2 s) so a hung RG never stalls a soft-delete saga, and reports `1` for non-empty pages / `0` for empty / `DomainError::ServiceUnavailable` (HTTP 503) on RG client error or timeout. The `ClientHub` binding is **wired** in the AM module entry-point (`module.rs`): `resource-group` is a hard `deps` of AM, so the runtime guarantees its init runs first, the entry-point hard-resolves `ResourceGroupClient`, and `init` fails closed if the client cannot be obtained — soft-delete safety is contract-load-bearing, so we do not admit-everything via an inert fallback in production.

**Post-#1626 target behavior**: with `cyberware-rust#1626` shipped, `tenant_id` on RG's filter whitelist, and the AM module entry-point binding `RgResourceOwnershipChecker` via `ClientHub`, `RgResourceOwnershipChecker` reports a non-zero result whenever the page contains any rows and the soft-delete flow surfaces the canonical `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_RESOURCES`. The DoD AC below remains gated on the REST wiring (separate PR) that exposes `DELETE /tenants/{id}` to clients; the service-layer call into `count_ownership_links` is already wired through this PR.

**Implements**:

- `cpt-cf-account-management-flow-tenant-hierarchy-management-soft-delete-tenant`
- `cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions`

**Touches**:

- API: `DELETE /api/account-management/v1/tenants/{tenant_id}` (`deleteTenant`)
- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- Resource Group ownership graph (read-side check)

### Hard-Delete Leaf-First Ordering

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-hard-delete-leaf-first`

Hard-deletion **MUST** run after the configurable retention period (default 90 days) via a background job that processes due tenants in `depth DESC` order so a parent is never hard-deleted while it still has `tenants` children (avoids FK violation and orphan rows). Each tenant **MUST** have `IdpPluginClient::deprovision_tenant` invoked before its `tenants` row is removed; an IdP terminal failure **MUST** defer the DB delete to the next tick, emit the `dependency_health` metric, and emit `actor=system` audit. On success, the `tenant_closure` rows where this tenant is descendant **MUST** be removed atomically with the `tenants` row.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler`

**Touches**:

- IdP contract: `IdpPluginClient::deprovision_tenant`
- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- Metric families: `dependency_health`, `tenant_retention` (catalog owned by `errors-observability`)

### Tenant-Read Scope

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-tenant-read-scope`

GET `/tenants/{tenant_id}` **MUST** return tenant details (`id`, `parent_id`, `tenant_type` re-hydrated from Types Registry, `status`, `self_managed`, `depth`, `name`, administrative timestamps) only for tenants inside the caller's authorized scope; `provisioning` tenants **MUST NOT** be surfaced (they return `CanonicalError::NotFound` (HTTP 404)). Cross-tenant access outside scope **MUST** surface as `CanonicalError::PermissionDenied` (HTTP 403, `reason=CROSS_TENANT_DENIED`) at the `errors-observability` boundary.

**Implements**:

- `cpt-cf-account-management-flow-tenant-hierarchy-management-read-tenant`

**Touches**:

- API: `GET /api/account-management/v1/tenants/{tenant_id}` (`getTenant`)
- DB: `cpt-cf-account-management-dbtable-tenants`
- Types Registry (read-side re-hydration)

### Children-Query Pagination

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-children-query-paginated`

GET `/tenants/{tenant_id}/children` **MUST** return direct children (single-level, not transitive) with cursor pagination and optional `status` filter across `{active, suspended, deleted}`; `provisioning` children **MUST NOT** be surfaced. Page size **MUST** be capped by platform policy; deeper barrier-aware traversal is out of scope (owned by `tenant-resolver-plugin`).

**Implements**:

- `cpt-cf-account-management-flow-tenant-hierarchy-management-list-children`

**Touches**:

- API: `GET /api/account-management/v1/tenants/{tenant_id}/children` (`listChildren`)
- DB: `cpt-cf-account-management-dbtable-tenants`

### IdP Tenant-Provision Contract

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provision`

Every successful tenant creation **MUST** invoke `IdpPluginClient::provision_tenant` (saga step 2) with the tenant identity and deployment-supplied provisioning context; providers **MUST NOT** silently no-op on this mutating operation. Any provider-returned metadata entries **MUST** be persisted into `dbtable-tenant-metadata` inside the finalization TX (saga step 3).

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga` (step 2 + step 3 metadata persist)

**Touches**:

- IdP contract: `IdpPluginClient::provision_tenant`
- DB: `cpt-cf-account-management-dbtable-tenant-metadata` (persist only; schema owned by `tenant-metadata`)

### IdP Tenant-Provisioning-Failure Contract

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provisioning-failure`

Provisioning failures **MUST** surface as one of two deterministic categories per `fr-idp-tenant-provision-failure`: clean compensable (`idp_unavailable`) when AM can prove the IdP retained no tenant state — AM then compensates synchronously by deleting the `provisioning` row; or reconciliation-required (`internal`) when the external outcome is ambiguous (transport error, timeout, generic 5xx) — the provisioning reaper compensates asynchronously and the caller **MUST** reconcile before retry. AM **MUST NOT** classify ambiguous failures as clean retryable.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga` (compensable + ambiguous branches)
- `cpt-cf-account-management-algo-tenant-hierarchy-management-provisioning-reaper-compensation`

**Touches**:

- IdP contract: `IdpPluginClient::provision_tenant` / `deprovision_tenant`
- Error taxonomy: `errors-observability` `idp_unavailable` + `internal` categories

### IdP Tenant-Deprovision Contract

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-deprovision`

Hard-delete **MUST** invoke `IdpPluginClient::deprovision_tenant` before removing the `tenants` row; already-absent is treated as success. The provisioning reaper **MUST** also invoke `deprovision_tenant` when compensating stuck `provisioning` rows. Deprovisioning **MUST NOT** run at soft-delete time — IdP resources remain available throughout the retention window to permit recovery flows.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler`
- `cpt-cf-account-management-algo-tenant-hierarchy-management-provisioning-reaper-compensation`

**Touches**:

- IdP contract: `IdpPluginClient::deprovision_tenant`

### Hierarchy-Integrity Diagnostics

- [x] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-integrity-diagnostics`

`TenantService::check_hierarchy_integrity()` **MUST** be exposed as an internal SDK method producing a structured report assembled from 8 pure-Rust classifiers defined in DESIGN §3.2 Diagnostic Capabilities and detailed in [Hierarchy Integrity Check](#8-hierarchy-integrity-check):

- `orphan` — tenant rows whose `parent_id` references a tenant absent from the snapshot (orphan child).
- `cycle` — `parent_id` cycles, detected by DFS with seen-set over `tenants[].parent_id`.
- `depth` — `tenants` rows whose stored `tenants.depth` disagrees with the depth derived from the `parent_id` walk (root depth `0`; each step adds `1`). The self-row `(id, id)` exists for every SDK-visible tenant, so consistency is determined by parity with the parent-walk-derived depth (root must be `0`; non-root must equal its computed depth). Note the `tenant_closure` table itself carries **no** `depth` column (see `m0001_initial_schema`); the classifier reads `tenants.depth` exclusively.
- `self_row` — SDK-visible tenants with no `(id, id)` self-row in closure.
- `strict_ancestor` — strict `(ancestor, descendant)` pairs present in the parent-walk but absent from `tenant_closure`.
- `extra_edge` — closure rows whose `(ancestor, descendant)` pair is not produced by the `parent_id` walk (closure EXCEPT parent-walk); includes orphan closure rows whose endpoints are absent from `tenants`.
- `root` — violations of the single-root invariant (zero or multiple `parent_id IS NULL` rows in scope).
- `barrier` — rows whose materialized `barrier` flag in `tenant_closure` disagrees with the parent-walk-derived barrier coverage.

The 8 classifiers **MUST** run synchronously over a `(tenants, tenant_closure)` snapshot loaded via SecureSelect inside a `REPEATABLE READ` read-only transaction so the report reflects one consistent state. Single-flight **MUST** be enforced uniformly across PostgreSQL and SQLite via the `integrity_check_runs` singleton PK gate, acquired and released in **separate** short committed transactions outside the snapshot tx (`lock::acquire_committed` → REPEATABLE READ snapshot/classify → `lock::release_committed`); the committed gate row is what makes contenders surface `DomainError::IntegrityCheckInProgress` mapped to `CanonicalError::ResourceExhausted` (HTTP 429) immediately rather than queueing on an uncommitted PK and then succeeding redundantly after the original transaction commits. Memory footprint **MUST** be `O(tenants + closure_rows + violations)` — bounded by the snapshot's tenant rows plus the strict-ancestor closure rows plus the violation count. Closure-row count materially exceeds tenant count on deep or dense trees (a tenant at depth `d` contributes `d + 1` closure rows), so operators MUST size limits and any future streaming knob against the closure side, not just the tenant count. The trade-off is explicit avoidance of a raw-SQL escape hatch in the production runtime (no `query_raw_all` consumers in production source).

Each category **MUST** update the `am.hierarchy_integrity_violations` gauge metric with a `category` label. Zero-value emissions **MUST** occur on clean runs so alert rules observe a known baseline.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check`

**Touches**:

- Metric family: `am.hierarchy_integrity_violations` gauge (naming-alignment owned by `errors-observability` `dod-ops-metrics-treatment`)
- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`

### Data Remediation Telemetry + Documented Path

- [x] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-data-remediation`

AM-owned integrity anomalies detected by `algo-hierarchy-integrity-check` and compensation failures from the provisioning reaper / hard-delete scheduler **MUST** produce operator-visible telemetry within 15 minutes of detection via the `errors-observability` metric families. The provisioning-reaper `actor=system` audit envelope via `errors-observability` `algo-audit-emission` is **deferred to a follow-up** pending the platform append-only audit sink; in the interim, the structured `am.events` stand-in (`inst-algo-reap-defer` / `inst-algo-reap-audit`) is the v1 audit signal that DoD compliance evaluates against, paired with the `tenant_retention` / `dependency_health` metric families. Each anomaly category **MUST** have a documented remediation path triageable within one business day. Cross-module cleanup gaps this feature cannot correct automatically (e.g., residual Resource Group ownership links after soft-delete) **MUST** remain explicitly surfaced rather than silently ignored.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check` (telemetry)
- `cpt-cf-account-management-algo-tenant-hierarchy-management-provisioning-reaper-compensation` (failure telemetry)
- `cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler` (IdP deferral telemetry)

**Touches**:

- Metric families: `dependency_health`, `tenant_retention`, `hierarchy_depth_exceedance` (catalog owned by `errors-observability`)
- Runbook: platform on-call runbook links per `errors-observability` `dod-ops-metrics-treatment`

### Data Lifecycle — Soft/Hard Delete + IdP Deprovision

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-data-lifecycle`

Tenant end-of-life **MUST** flow soft-delete → retention window → leaf-first hard-delete with IdP deprovisioning before row removal (per `dod-hard-delete-leaf-first`). Tenant-scoped metadata rows **MUST** be removed through the tenant-metadata cascade-delete contract when the tenant row is removed. Resource Group residual-resource checks happen at soft-delete precondition time (`dod-soft-delete-preconditions`): if any RG-owned resources remain scoped to the tenant, soft-delete is refused with a precondition failure and the caller **MUST** clean them up through `ResourceGroupClient` before re-attempting — AM does NOT perform the cleanup itself at soft-delete time, the caller owns that responsibility. At hard-delete, AM invokes `feature-user-groups`' cascade-cleanup trigger (`cpt-cf-account-management-flow-user-groups-cascade-cleanup-trigger`) to remove any remaining tenant-scoped user-group subtree before the `tenants` row is deleted, as a belt-and-suspenders safeguard against residuals appearing between soft-delete and hard-delete. The PRD §6.11 sequence "remove metadata → RG cleanup → IdP deprovision → hard-delete" is realized as: soft-delete precondition residual check (refuses if RG has residuals; caller cleans up via RG), hard-delete-time RG cleanup trigger via `feature-user-groups` (safety-belt cleanup), metadata removal atomically with tenant row removal via the tenant-metadata cascade-delete contract, and IdP `deprovision_tenant` invoked by the hard-delete scheduler before the `tenants` row is removed — same effective order, anchored to the implementation surfaces above.

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions`
- `cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler`

**Touches**:

- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`, `dbtable-tenant-metadata` (cascade)
- IdP contract: `IdpPluginClient::deprovision_tenant`

### Production Scale Envelope

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-production-scale`

Implementation **MUST** operate within the PRD §6.8 v1 design targets: 100K tenants, 3–10 typical depth (benchmarked to ≥15), 1K rps peak; administrative mutations sustain ≥25 writes/second over a 15-minute window; background expiry and retention clear a 10K-row backlog within 60 minutes; index layout on `tenant_closure(ancestor_id, barrier, descendant_status)` **MUST** support the anchored decisions in DESIGN §3.1 / §3.7. Enlarging any dimension **MUST** revisit those decisions — not be treated as a documentation change.

**Implements**:

- Operational envelope anchored to DESIGN §3.1 Domain Model + DESIGN §3.7 Database schemas; enforced through benchmark-gated validation rather than a single algorithm

**Touches**:

- DB: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure` (index layout)
- Platform benchmark suite (per GA load-test gates)

### Concurrency Serializability

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-hierarchy-management-concurrency-serializability`

**PR1 scope**: `infra::storage::repo_impl::helpers::with_serializable_retry` wraps every transactional repo write under SQL `SERIALIZABLE` isolation with bounded retry on `40001`. The raw `DbErr` is carried through retry by the infra-internal `TxError::Db` enum (`infra/storage/repo_impl/helpers.rs`) — `DomainError` itself stays pure (no `sea_orm` references, `#[domain_model]`-validated). After retry exhaustion `infra::canonical_mapping::classify_db_err_to_domain` translates the surviving `DbErr` into `DomainError::Aborted { reason: "SERIALIZATION_CONFLICT" }`, and the boundary mapping (`From<DomainError> for CanonicalError`) forwards that to `CanonicalError::Aborted` (HTTP 409). The unique index on `tenants(id)` plus the partial single-root unique index ship in `m0001_initial_schema`. Service-layer callers and racing-saga end-to-end coverage land in subsequent PRs.

Hierarchy-mutating operations on overlapping scopes **MUST** resolve with deterministic, serializable outcomes per PRD §5.2 cross-cut. Two parallel creates under the same parent, status-change racing soft-delete on the same tenant, and concurrent closure writes **MUST NOT** leave partial state: at every commit point the `tenants` + `tenant_closure` invariants hold, and losing writers **MUST** receive a deterministic canonical category (`CanonicalError::Aborted` (HTTP 409, `reason=SERIALIZATION_CONFLICT`) for retry-exhausted serialization conflicts; `CanonicalError::AlreadyExists` (HTTP 409) for unique-key collisions; `CanonicalError::FailedPrecondition` (HTTP 400) for state-precondition violations) rather than a partial-write success. Tenant creation **MUST** rely on the unique index on `tenants(id)` to guarantee at-most-one row per tenant (racing saga-step-1 inserts for the same `{child_id}` collide deterministically). Status-change and soft-delete operations **MUST** run under SQL serializable transaction isolation so that the paired `tenants.status` write and `tenant_closure.descendant_status` rewrite (or `tenants.status=deleted` flip) commit together or abort together, and racing writers serialize into a well-defined winner/loser order. This DoD is fingerprinted by AC 15 (concurrent hierarchy-mutating operations resolve serializable per PRD §5.2 cross-cut).

**Implements**:

- `cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga`
- `cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance`
- `cpt-cf-account-management-algo-tenant-hierarchy-management-soft-delete-preconditions`

**Touches**:

- DB: `cpt-cf-account-management-dbtable-tenants` (unique index on `(id)` for tenant creation), `cpt-cf-account-management-dbtable-tenant-closure`
- Transaction isolation: SQL serializable isolation for status-change and soft-delete operations
- PRD §5.2 concurrency cross-cut

## 6. Acceptance Criteria

- [ ] Creating a child tenant under an `active` parent returns `201 Created`; the `tenants` row ends with `status=active`; `tenant_closure` contains the new tenant's self-row `(id, id, 0, active)` plus one strict-ancestor row per step up `parent_id` with `barrier` materialized per canonical rule; the IdP provider received exactly one `provision_tenant` call.
- [ ] A synthetic IdP clean compensable failure during `provision_tenant` leaves the `tenants` table with no row for the attempted child and the `tenant_closure` table unchanged; the caller receives `CanonicalError::ServiceUnavailable` (HTTP 503).
- [ ] A synthetic finalization-TX failure leaves the `tenants` row in `status=provisioning` with no closure rows; after `provisioning-timeout + 1 tick`, the provisioning reaper calls `deprovision_tenant` (idempotent), deletes the `tenants` row only when deprovision succeeds or reports already-absent state, and emits the v1 stand-in `am.events` structured log with `kind=provisioningReaperCompensated`, `actor=system` (the full `errors-observability` audit envelope is **deferred to a follow-up** until the platform append-only audit sink lands; see `inst-algo-reap-audit`) — with no closure cleanup needed.
- [ ] Creating a child at depth > threshold in advisory mode returns `201 Created` AND emits exactly one `hierarchy_depth_exceedance` metric sample plus exactly one structured warning log entry with `tenant_id`, `parent_id`, `observed_depth`, `threshold`.
- [ ] Creating a child at depth > threshold in strict mode returns `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_DEPTH_EXCEEDED`; the `tenants` table is unchanged.
- [ ] PATCH `status=suspended` on a parent leaves every direct and transitive descendant's `tenants.status` unchanged; for the target tenant, `tenant_closure.descendant_status` rewrites to `suspended` across every row where `descendant_id = target`; child tenants' own rows are unaffected.
- [ ] PATCH `status=deleted` is rejected with `CanonicalError::FailedPrecondition` (HTTP 400); PATCH modifying `parent_id`, `tenant_type`, `self_managed`, or `depth` is rejected with `CanonicalError::InvalidArgument` (HTTP 400) in each case.
- [ ] **Post-service-wiring target**: DELETE on the root tenant returns `CanonicalError::InvalidArgument` (HTTP 400) with `reason=ROOT_TENANT_CANNOT_DELETE`; DELETE on a tenant with a non-deleted child returns `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_CHILDREN`; DELETE on a tenant with remaining Resource-Group-owned resources returns `CanonicalError::FailedPrecondition` (HTTP 400) with `reason=TENANT_HAS_RESOURCES`; DELETE on a childless, resource-free non-root tenant transitions `tenants.status=deleted` and rewrites `tenant_closure.descendant_status=deleted` atomically. *Current behavior*: the `TENANT_HAS_RESOURCES` arm fires once the AM module entry-point binds `RgResourceOwnershipChecker` via `ClientHub` (the `#1626` filter whitelist has shipped) — see the §5 DoD note for details.
- [ ] GET `/tenants/{id}` for a tenant in internal `provisioning` state returns `CanonicalError::NotFound` (HTTP 404); GET for an SDK-visible tenant returns `200` with `tenant_type` re-hydrated to the public chained identifier.
- [ ] GET `/tenants/{id}/children` returns direct children only (no transitive descendants), paginated with a next-cursor, filtered by the optional `status` parameter, and never surfaces `provisioning` rows.
- [ ] After retention expiry, the hard-delete background job processes due tenants in `depth DESC` order; a parent is not hard-deleted while any `tenants` child still exists; `IdpPluginClient::deprovision_tenant` is invoked exactly once per tenant before its `tenants` row is removed; closure rows where `descendant_id = tenant_id` are deleted in the same transaction as the `tenants` row delete.
- [ ] A synthetic IdP `deprovision_tenant` terminal failure during hard-delete leaves the `tenants` row intact, emits a `dependency_health` metric increment labeled `target=idp, op=deprovision_tenant`, emits an `actor=system` audit via `errors-observability`, and retries on the next scheduler tick.
- [ ] A retention tick where a due child is deferred because `deprovision_tenant` failed and the parent is also due keeps the parent `tenants` row intact because the in-transaction child-existence guard observes the remaining child; the parent emits deferred-cleanup telemetry and is retried on a later tick.
- [ ] A synthetic IdP `deprovision_tenant` retryable or terminal failure during provisioning-reaper compensation retains the `status=provisioning` row, emits `dependency_health` and `tenant_retention` telemetry, and retries or requires operator remediation without deleting AM's compensating state. The full `actor=system` audit via `errors-observability` is **deferred to a follow-up** pending the platform append-only audit sink; the v1 stand-in is the structured `am.events` log emitted from the defer path (see `inst-algo-reap-defer`).
- [ ] `TenantService::check_hierarchy_integrity()` returns a structured report with non-empty per-category arrays on a deliberately seeded dataset covering each anomaly category, and clean (`[]`) arrays on a known-good hierarchy; the report is produced by 8 pure-Rust classifiers over a `(tenants, tenant_closure)` snapshot loaded once via SecureSelect inside a `REPEATABLE READ` read-only transaction; the `integrity_check_runs` single-flight gate is acquired and released in **separate** committed transactions outside the snapshot tx (`lock::acquire_committed` → snapshot/classify → `lock::release_committed`) so contenders observe the gate immediately; the `am.hierarchy_integrity_violations` gauge carries the `category` label for every one of the 10 fixed-shape categories (zero-valued on a clean run).
- [ ] Under the v1 benchmark profile, the background provisioning reaper clears a 10K-row stuck-provisioning backlog within 60 minutes and the hard-delete scheduler clears a 10K-row due-retention backlog within 60 minutes without violating the 1K rps peak read budget against `dbtable-tenants` + `dbtable-tenant-closure`.
- [ ] Concurrent hierarchy-mutating operations on overlapping scopes (two parallel creates under the same parent, status-change racing soft-delete on the same tenant) resolve serializable per PRD §5.2 cross-cut: losing writers receive a deterministic canonical category (`CanonicalError::Aborted` HTTP 409 with `reason=SERIALIZATION_CONFLICT`, `CanonicalError::AlreadyExists` HTTP 409 for unique-key collisions, or `CanonicalError::FailedPrecondition` HTTP 400 for state-precondition violations) rather than partial state; the `tenants` + `tenant_closure` invariants hold at every commit point.
- [ ] A closure-invariant property test exercises 1K randomized hierarchies (mixed managed / self-managed) under the property-test budget cap `MAX_DEPTH = 8` and asserts, for every tenant: (a) exactly one self-row with `barrier=0`, (b) exactly one strict-ancestor row per step up `parent_id` with `barrier=1` iff some tenant on the strict path `(ancestor, descendant]` has `self_managed=true`, (c) `descendant_status` equals the mapped `tenants.status` (domain `{active, suspended, deleted}` only), (d) zero rows exist for tenants in internal `provisioning` state. The PRD `≥ 15 levels` requirement is enforced separately by the v1 benchmark profile / integration gate, not by these property tests.
- [ ] A data-remediation telemetry test asserts that every anomaly category emitted by `algo-hierarchy-integrity-check` produces the corresponding `am.hierarchy_integrity_violations` gauge sample within 15 minutes of the anomaly being seeded, and that every runbook entry for the anomaly categories is linked from the on-call escalation path registered via `errors-observability` `dod-ops-metrics-treatment`; concurrent-integrity-check contention surfaces as `DomainError::IntegrityCheckInProgress` → `CanonicalError::ResourceExhausted` (HTTP 429); residual-resource failures at soft-delete and IdP-terminal failures at hard-delete are each surfaced as a triageable operator signal rather than silently swallowed.

## 7. Deliberate Omissions

- **Tenant-type parent-child validation (`allowed_parent_types`, same-type nesting)** — *Owned by `tenant-type-enforcement`* (DECOMPOSITION §2.3). `algo-create-tenant-saga` step 3 (`inst-algo-saga-type-check`) invokes that feature's barrier at its API boundary; the rule catalog and GTS registry integration live there, not here.
- **Mode selection (`self_managed` toggle) post-creation, barrier semantics, `ConversionRequest` state machine** — *Owned by `managed-self-managed-modes`* (DECOMPOSITION §2.4). This FEATURE only maintains the `barrier` column in `tenant_closure` as a transactional consequence of mode writes performed by that feature. Create-time `self_managed=true` is accepted here because the parent's explicit create call is the consent.
- **User-level IdP operations (user provision / deprovision / query)** — *Owned by `idp-user-operations-contract`* (DECOMPOSITION §2.5). Tenant-side IdP operations (tenant-provision / tenant-deprovision) remain in this feature as hierarchy-op side-effects.
- **Tenant metadata CRUD, schemas, inheritance resolution** — *Owned by `tenant-metadata`* (DECOMPOSITION §2.7). This FEATURE persists only the metadata entries the IdP provider returns at saga step 3; the schema catalog and resolution logic live in that feature. Metadata rows are removed through the tenant-metadata cascade-delete contract when a tenant row is removed.
- **User-group Resource Group type registration and lifecycle** — *Owned by `user-groups`* (DECOMPOSITION §2.6). The soft-delete precondition check reads the Resource Group ownership graph but does not register types or manage user-group lifecycle.
- **Read-only plugin query facade (`get_tenant`, `get_ancestors`, `get_descendants`, barrier-mode reductions)** — *Owned by `tenant-resolver-plugin`* (DECOMPOSITION §2.9). That plugin reads AM-owned `tenants` + `tenant_closure` directly via a dedicated SecureConn read-only pool; this feature writes the tables the plugin consumes.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, reliability/SLA policy, data-classification policy, metric catalog naming-alignment contract** — *Owned by `errors-observability`* (DECOMPOSITION §2.8). This FEATURE emits metric samples and audit events per the catalogs registered there; the public `code` identifiers and metric-family canonical names are catalog-resolved, not redefined here.
- **Tenant lifecycle CloudEvents / event bus integration** — *Deferred to a future EVT module* (DESIGN §4.1). v1 remains synchronous and request-driven; advisory depth threshold is an operator-visible warning signal (metric + structured log), not a CloudEvent.
- **Subtree moves (reparenting)** — *Not supported in v1* (DESIGN §3.2 `TenantService`). `update_tenant` accepts only `name` and `status`; no subtree-wide closure rebuild is required because no subtree-move mutator exists.
- **Barrier filtering and `BarrierMode` reductions on read-time queries** — *Owned by `tenant-resolver-plugin`* (DECOMPOSITION §2.9). Hierarchical-action authorization is wired in this feature: every `TenantService` CRUD method calls `PolicyEnforcer::access_scope_with` on `gts.cf.core.am.tenant.v1~` (exported as `account_management_sdk::gts::TENANT_RESOURCE_TYPE`) with the action vocabulary from DESIGN §4.2 line 1363 (`create`, `read`, `update`, `delete`, `list_children`) before any structural precondition. The PDP gate is the single authority on cross-tenant authorization for `tenants` rows in the current PR — the `tenants` entity is declared `#[secure(no_tenant, no_resource, no_owner, no_type)]`, so the SecureORM applies no automatic `WHERE` filter on its reads or writes (a `no_*` entity zero-rows on any narrowed scope by construction; the `TenantRepo` trait contract therefore requires callers to pass `AccessScope::allow_all` until `InTenantSubtree` lands — see `domain/tenant/repo.rs` trait doc). AM-local saga-internal structural reads (parent-status precondition, `count_children`) explicitly use `allow_all` per DESIGN §4.2 line 1370 (structural-read carve-out). The Resource Group probe (`count_ownership_links`) propagates the caller's `ctx` instead — that call crosses the AM/RG trust boundary, and the structural-read carve-out is scoped to AM-local reads only; RG enforces its own PEP gate against the original principal. PDP transport failures fail closed (`service_unavailable` HTTP 503) per DESIGN §4.3.

  *Current AM-side state* (this PR, cyberware-rust#1813 AM half): the subtree clamp **is** wired on `tenants` reads through the `InTenantSubtree` predicate type (mirror of the existing `InGroupSubtree` stack — `authz-resolver-sdk` + `modkit-security` + `modkit-db secure`). The `tenants` entity is now declared `#[secure(no_tenant, resource_col = "id", no_owner, no_type)]`; `TenantService::module init` advertises `Capability::TenantHierarchy` on the `PolicyEnforcer` (see `account-management/src/module.rs::init`); the secure builder compiles `InTenantSubtree(RESOURCE_ID, root, BarrierMode::Respect)` into the canonical `tenants.id IN (SELECT descendant_id FROM tenant_closure WHERE ancestor_id = :root AND barrier = 0)` JOIN at query time; and `TenantRepo`'s trait doc drops the "MUST pass `allow_all`" requirement on caller-driven reads (`get_tenant`, `list_children`, `update_tenant`, `delete_tenant`, plus the new metadata / user-ops / conversion tenant-visibility fences that load via `tenant_repo.find_by_id(&scope, …)`). AM-local saga-internal structural reads (`count_children`, retention sweep) still use `allow_all` per DESIGN §4.2 line 1370 — the structural-read carve-out is unchanged.

  *Deployment coordination — known gap (cyberware-rust#1813 plugin half, NOT YET LANDED)*: the in-tree `tr-authz-plugin` and `static-authz-plugin` (`modules/system/authz-resolver/plugins/{tr,static}-authz-plugin/`) **do not yet read** `EvaluationRequestContext.capabilities`. They unconditionally emit `Predicate::In(OWNER_TENANT_ID, …)` regardless of advertised capabilities (see `tr-authz-plugin/src/domain/service.rs::allow_eq` / `allow_in`). On a `tenants` entity declared `no_tenant`, the `OWNER_TENANT_ID` property does not resolve and `SecureEntityExt::scope_with` fails closed to `WHERE false` for every narrowed scope — meaning every authorized `get_tenant` / `list_children` / `update_tenant` / `delete_tenant` (and the new metadata / user / conversion tenant-visibility fences built on the same `find_by_id(&scope, …)`) returns `NotFound` in production. Integration / unit tests pass because the AM test harness uses a `PermitWithSubtreeResolver` / `MockAuthZResolver` that already emits `InTenantSubtree(RESOURCE_ID)`; the gap is invisible to test runs. **Production deployment of this PR therefore requires a coordinated `tr-authz-plugin` (and `static-authz-plugin`) update that honours `Capability::TenantHierarchy` and emits `Predicate::InTenantSubtree(RESOURCE_ID, …)` when the consumer advertises it.** Until that lands, production rollouts MUST either (a) hold the AM upgrade together with the plugin upgrade, or (b) temporarily downgrade the AM enforcer construction in `module.rs::init` to `PolicyEnforcer::new(authz)` (no capabilities) AND revert the `tenants` Scopable declaration to `no_tenant, no_resource` — a one-line revert pair that restores the `allow_all`-only contract for caller-driven reads. Tracked as the plugin half of cyberware-rust#1813.
- **`ClientHub` binding for the production `RgResourceOwnershipChecker`** — *Wired in the AM module entry-point (`module.rs`).* Both halves of the production soft-delete `TENANT_HAS_RESOURCES` probe were already landed in earlier PRs: the `TenantService::soft_delete` invocation site (calls `ResourceOwnershipChecker::count_ownership_links(ctx, tenant_id)` between the `count_children` guard and `schedule_deletion`, rejecting with `DomainError::TenantHasResources` when the count is non-zero) **and** the `RgResourceOwnershipChecker` trait impl at `modules/system/account-management/account-management/src/infra/rg/checker.rs` (issues `list_groups(ctx, $top=1, $filter=tenant_id eq <tenant_id>)` against `ResourceGroupClient`, with a 2 s `tokio::time::timeout` so a hung RG never stalls the saga, surfacing transport faults as `DomainError::ServiceUnavailable` → `CanonicalError::ServiceUnavailable` HTTP 503). The wire-up is now in place: `ResourceGroupClient` is treated as a hard runtime dependency. The AM module entry-point declares `resource-group` in `#[modkit::module(deps = [...])]` (the runtime guarantees init ordering), resolves the client from `ClientHub`, and fails `init` with a propagated error if the client cannot be obtained — there is no production fallback to `InertResourceOwnershipChecker`. On success the entry-point binds `Arc::new(RgResourceOwnershipChecker::new(client))`. `InertResourceOwnershipChecker` is reserved for unit tests, which construct `TenantService` directly and bypass this init path. REST surfacing of `DELETE /tenants/{id}` is the remaining piece and is tracked in its own follow-up PR.

## 8. Hierarchy Integrity Check

The hierarchy-integrity check is implemented as 8 pure-Rust classifier functions that run over an in-memory `(tenants, tenant_closure)` snapshot loaded via SecureSelect inside a `REPEATABLE READ` read-only transaction on `PostgreSQL` (transparently `SERIALIZABLE` on `SQLite`, since `SQLite` does not implement `REPEATABLE READ` and `modkit-db` maps the requested level per its `TxIsolationLevel` backend-notes contract — both produce the snapshot consistency the integrity check requires). The snapshot tx contains no writes; the `integrity_check_runs` single-flight gate is acquired and released in two **separate** short committed transactions (`lock::acquire_committed` before the snapshot tx, `lock::release_committed` after) so the gate row is committed for the duration of the work and contenders surface `DomainError::IntegrityCheckInProgress` immediately. Uniform across `PostgreSQL` and `SQLite`. A long-running whole-tree integrity check cannot pile up against itself, and the snapshot tx itself cannot self-evict on SI conflicts because it never writes.

### Classifier Catalog

| # | Category                   | Classifier (pure-Rust)                                                            | Returns           |
|---|----------------------------|-----------------------------------------------------------------------------------|-------------------|
| 1 | Missing parent (orphan)    | walk `tenants[].parent_id`; flag rows whose parent is absent from the snapshot    | `Vec<Violation>`  |
| 2 | Parent-id cycle            | DFS with seen-set over `tenants[].parent_id` to detect self-reachability          | `Vec<Violation>`  |
| 3 | Tenant depth mismatch      | derive depth via `parent_id` walk; compare against stored `tenants.depth`         | `Vec<Violation>`  |
| 4 | Missing self-row           | `tenants[]` for which `(id, id)` self-edge is absent from `tenant_closure`        | `Vec<Violation>`  |
| 5 | Missing strict-ancestor    | parent-walk-derived strict `(ancestor, descendant)` pairs absent from closure     | `Vec<Violation>`  |
| 6 | Extra closure edge         | closure pairs not produced by the `parent_id` walk (closure EXCEPT parent-walk)   | `Vec<Violation>`  |
| 7 | Root anomaly               | violations of the single-root invariant (zero or multiple `parent_id IS NULL`)    | `Vec<Violation>`  |
| 8 | Barrier coverage           | rows whose materialized `barrier` flag disagrees with the parent-walk derivation  | `Vec<Violation>`  |

Each classifier is a synchronous, DB-free function that reads the loaded `Snapshot` and returns `Vec<Violation>`. Classification logic lives in Rust; only the snapshot load and the single-flight gate touch the database, both via SecureORM (no `query_raw_all` in production runtime). The results are composed into the structured report by `TenantService::check_hierarchy_integrity()`.

The 8 classifiers expand into **10 metric-label categories** on `am.hierarchy_integrity_violations` because two classifiers split their findings:

- `orphan` → `orphaned_child` (the parent is absent from `tenants` entirely) and `broken_parent_reference` (the parent row exists but is in `provisioning` and therefore invisible to closure).
- `barrier` → `barrier_column_divergence` (materialized `tenant_closure.barrier` disagrees with the parent-walk derivation) and `descendant_status_divergence` (`tenant_closure.descendant_status` disagrees with `tenants.status` for the same descendant).

The remaining 6 classifiers (`cycle`, `depth`, `self_row`, `strict_ancestor`, `extra_edge`, `root`) each emit a single label of the same name. Tests must seed all 10 labels.

### Snapshot Consistency

- A read-only `REPEATABLE READ` transaction on `PostgreSQL` (transparently `SERIALIZABLE` on `SQLite` per `modkit-db`'s `TxIsolationLevel` backend-notes mapping) loads `tenants` and `tenant_closure` for the requested scope via SecureSelect (`secure().scope_with(...).all(tx)`), so the snapshot is one consistent `(tenants, tenant_closure)` view — concurrent writes do not interleave categories. The `integrity_check_runs` single-flight gate is committed in a separate short transaction outside this snapshot tx; the snapshot tx itself contains no writes.
- Memory footprint is `O(tenants + closure_rows + violations)`: bounded by the snapshot's tenant rows, the strict-ancestor closure rows, and the violation count. Closure-row count grows with hierarchy depth (a tenant at depth `d` contributes `d + 1` closure rows), so on deep or dense trees the snapshot is dominated by `closure_rows`, not `tenants` — operational limits and any future streaming knob MUST size against the closure side. The trade-off is explicit avoidance of a raw-SQL escape hatch in the production runtime — `query_raw_all` is removed from AM's production path. The violation **output** size guarantee remains proportional to the number of returned violations; the change is only about transient input snapshot memory.

### Single-Flight

- **Uniform contract**: an `integrity_check_runs(id, worker_id, started_at)` singleton-row table holds the lock. Acquisition is a SecureORM `secure_insert` inside its own short committed transaction (`lock::acquire_committed`); the commit makes the row visible to concurrent contenders so they surface `IntegrityCheckInProgress` from their own acquire instead of queueing on an uncommitted PK. The PK is pinned to `id = 1` by a `CHECK` constraint so the table holds at most one row at a time. Release is a separate committed `secure_delete` (`lock::release_committed`) pinned by `worker_id`. The same code path runs on PostgreSQL and SQLite — no backend branching.
- **Stale-lock sweep**: every `acquire_committed` deletes any row whose `started_at` exceeds `MAX_LOCK_AGE` (1h, well above the largest realistic check or repair runtime), so a crashed worker cannot block indefinitely. A live release that finds zero rows affected (the sweeper raced in) emits a structured warn-log on `am.integrity` for telemetry.
- **Conflict surface**: when the acquire INSERT fails on PK conflict, `TenantService` surfaces `DomainError::IntegrityCheckInProgress`, which the REST layer maps to `CanonicalError::ResourceExhausted` (HTTP 429) per `errors-observability`. The 429 envelope carries `quota_violations[].subject = "integrity_check"`. On-demand callers retry with backoff; AM does not queue.

### Periodic In-Process Job

In addition to the on-demand SDK method, AM ships an in-process periodic job that invokes `TenantService::check_hierarchy_integrity()` on a jittered interval. The job is configured via `AccountManagementConfig.integrity_check` with these knobs (defaults shown):

- `enabled = true` — opt-out via `false`; when disabled the on-demand SDK method continues to work and `am.hierarchy_integrity_runs` simply never advances.
- `interval_secs = 3600` — bounded `[60, 86400]` by `AccountManagementConfig::validate`.
- `initial_delay_secs = 300` — must be `<= interval_secs`; gives the bootstrap saga and the retention/reaper loops time to settle before the first whole-tree snapshot.
- `jitter = 0.1` — multiplicative spread `[1 - jitter, 1 + jitter]` per tick, bounded `[0.0, 0.5]`; spreads load across replicas without leader-election.

Per-tick error policy: a `DomainError::IntegrityCheckInProgress` (peer-replica or operator on-demand call holds the gate) records `am.hierarchy_integrity_runs{outcome=skipped_in_progress}` and resumes the loop without retrying inside the tick — a competing run is already producing fresh telemetry. Any other error records `am.hierarchy_integrity_runs{outcome=failed}` and resumes; transient DB blips MUST NOT silently disable the periodic audit. Successful ticks emit `am.hierarchy_integrity_runs{outcome=completed}` plus `am.hierarchy_integrity_duration{phase=check}` (histogram, ms) and `am.hierarchy_integrity_last_success` (gauge, unix seconds) — the latter feeds the freshness watchdog (`alert if last_success older than 2 × interval_secs`) that the violation gauge cannot satisfy alone.

### Test Strategy

- **Classifier-level (unit)**: each of the 8 classifiers has at least three in-source unit tests over hand-built `Snapshot` fixtures (positive, negative, and edge-case shapes such as deep linear chains), running entirely in-process without a database.
- **End-to-end (integration)**: a single `tests/integrity_integration.rs` exercises both backends (`#[cfg(feature = "postgres")]` and `#[cfg(feature = "sqlite")]`) using SecureORM `ActiveModel` inserts and `SecureDeleteExt` deletes for seeding. PostgreSQL runs against testcontainers; SQLite runs against `:memory:`. Each backend asserts at least one positive and one negative case per category, plus a single-flight contention case asserting `429`.
- **Per-category coverage**: every category has at least one positive and one negative test path; the negative case asserts an empty `Vec<Violation>` and a zero-valued `am.hierarchy_integrity_violations` gauge sample for that `category` label.
- **Periodic-job coverage**: `domain::integrity_check::config_tests` asserts the validation matrix (interval bounds, jitter bounds, `initial_delay <= interval`); `domain::integrity_check::service_tests` runs the loop under `tokio::time::pause()` against a counting fake `IntegrityChecker` to assert `initial_delay` honoured, jitter within `[1 - jitter, 1 + jitter]` bounds with measurable variance, `IntegrityCheckInProgress` and other errors treated as skip-and-continue, and prompt shutdown during both the warmup sleep and the post-tick sleep.

### Removed Surface (post-refactor)

The following symbols and configuration knobs are removed by the Rust-side refactor and **MUST NOT** be referenced by new code or new docs outside this historical note:

- SQL-side classifier helpers `audit_integrity_pg`, `audit_integrity_sqlite`, `run_pg_classifiers`, `run_sqlite_classifiers`, and the lock helpers `acquire_pg_audit_lock`, `acquire_sqlite_audit_lock`, `release_sqlite_audit_lock` — replaced by the pure-Rust classifier module and the uniform `integrity_check_runs` PK gate.
- Postgres advisory-lock single-flight (`pg_try_advisory_lock` / `pg_try_advisory_xact_lock`) — replaced by the uniform `integrity_check_runs` PK gate on both backends.
- Per-call `query_raw_all` usage in production runtime, including the `DbConn::query_raw_all` / `DbTx::query_raw_all` extensions and the `DbConn::db_engine` / `DbTx::db_engine` accessors that existed only to support backend-branched raw SQL — production source has zero `query_raw_all` consumers after the refactor.
- `integrity_max_tenants` configuration knob — no longer applicable; the memory bound is now `O(tenants + closure_rows + violations)` set by the snapshot-loader rather than a hard pre-flight cap.
- The hard pre-flight whole-tree size guard — superseded by the in-memory snapshot model. The integrity check still materializes the `(tenants, tenant_closure)` rows in process memory at `O(tenants + closure_rows + violations)`, but contention against itself is now bounded by the `integrity_check_runs` singleton single-flight gate (`CanonicalError::ResourceExhausted` HTTP 429 on conflict) instead of a static row-count threshold.
